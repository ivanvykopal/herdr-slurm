//! `herdr-slurm cluster`: per machine GPU headroom by partition, start
//! estimates of pending jobs, and storage fill / quota.
use std::time::Duration;

use serde_json::{json, Value};

use crate::remote::{load_parsed, print_json, run_remote, sections, tool};
use crate::{Machine, Scheduler};

const DEFAULT_STORAGE: [&str; 4] = ["$HOME", "/scratch", "/mnt/scratch", "/project"];

#[derive(Debug, PartialEq)]
struct PartitionGpus {
    name: String,
    gpu_type: String,
    total: u64,
    used: u64,
    free: u64,
}

#[derive(Debug, PartialEq)]
struct Quota {
    used_kb: u64,
    limit_kb: u64,
    pct: u8,
}

#[derive(Debug, PartialEq)]
struct Pending {
    id: String,
    start: Option<String>,
}

/// Split on commas outside parentheses (`gpu:a:2(IDX:0,2),shard:1`).
fn split_top(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// `(type, count)` for each `gpu` entry of a Gres / GresUsed string; the
/// `(S:..)` / `(IDX:..)` suffix is stripped, untyped `gpu:8` gives `""`.
fn parse_gres(s: &str) -> Vec<(String, u64)> {
    split_top(s.trim())
        .into_iter()
        .filter_map(|e| {
            let e = e.split('(').next()?.trim();
            let mut f = e.split(':');
            if f.next()? != "gpu" {
                return None;
            }
            let rest: Vec<&str> = f.collect();
            match rest.as_slice() {
                [n] => Some((String::new(), n.parse().ok()?)),
                [t, n] => Some((t.to_string(), n.parse().ok()?)),
                _ => None,
            }
        })
        .collect()
}

/// Lines `node|partition|gres|gres_used|state|` aggregated per partition and
/// GPU type, in first-seen order. Unavailable nodes add to total only.
fn aggregate_gpus(sinfo: &str) -> Vec<PartitionGpus> {
    let mut out: Vec<PartitionGpus> = Vec::new();
    for line in sinfo.lines() {
        let f: Vec<&str> = line.split('|').map(str::trim).collect();
        if f.len() < 5 || f[1].is_empty() {
            continue;
        }
        let part = f[1].trim_end_matches('*');
        let state = f[4].to_ascii_lowercase();
        let unavailable = ["down", "drain", "fail", "maint", "resv"]
            .iter()
            .any(|s| state.contains(s));
        let used = parse_gres(f[3]);
        for (ty, total) in parse_gres(f[2]) {
            let u = used
                .iter()
                .filter(|(t, _)| *t == ty)
                .map(|(_, n)| n)
                .sum::<u64>()
                .min(total);
            let idx = match out.iter().position(|p| p.name == part && p.gpu_type == ty) {
                Some(i) => i,
                None => {
                    out.push(PartitionGpus {
                        name: part.to_string(),
                        gpu_type: ty.clone(),
                        total: 0,
                        used: 0,
                        free: 0,
                    });
                    out.len() - 1
                }
            };
            let p = &mut out[idx];
            p.total += total;
            if !unavailable {
                p.used += u;
                p.free += total - u;
            }
        }
    }
    out.retain(|p| p.total > 0);
    out
}

/// `df -Pk` data line → `(size_kb, used_kb, pct)`.
fn parse_df(line: &str) -> Option<(u64, u64, u8)> {
    let f: Vec<&str> = line.split_whitespace().collect();
    let i = f.iter().rposition(|t| t.ends_with('%') && t[..t.len() - 1].parse::<u8>().is_ok())?;
    if i < 3 {
        return None;
    }
    Some((
        f[i - 3].parse().ok()?,
        f[i - 2].parse().ok()?,
        f[i].trim_end_matches('%').parse().ok()?,
    ))
}

/// `lfs quota -q` line `fs used soft hard grace ...`; limit is hard, else
/// soft; `None` when both are 0.
fn parse_lfs_quota(line: &str) -> Option<Quota> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 4 {
        return None;
    }
    let num = |s: &str| s.trim_end_matches('*').parse::<u64>().ok();
    let used = num(f[1])?;
    let (soft, hard) = (num(f[2])?, num(f[3])?);
    let limit_kb = if hard > 0 { hard } else { soft };
    if limit_kb == 0 {
        return None;
    }
    Some(Quota {
        used_kb: used,
        limit_kb,
        pct: (used.saturating_mul(100) / limit_kb).min(255) as u8,
    })
}

/// `id|start` lines from `squeue --start`; a non-date start gives `None`.
fn parse_pending(text: &str) -> Vec<Pending> {
    text.lines()
        .filter_map(|l| {
            let (id, start) = l.trim().split_once('|')?;
            let start = start.trim();
            Some(Pending {
                id: id.trim().to_string(),
                start: start
                    .starts_with(|c: char| c.is_ascii_digit())
                    .then(|| start.to_string()),
            })
        })
        .collect()
}

fn storage_paths(m: &Machine) -> Vec<String> {
    if m.storage.is_empty() {
        DEFAULT_STORAGE.iter().map(|s| s.to_string()).collect()
    } else {
        m.storage.clone()
    }
}

/// One script with sections `sinfo`, `pending` (Slurm), `st:<n>` per storage
/// path and `end`. Paths were validated at config load; a leading `~` is
/// rewritten to `$HOME` and each path is double-quoted so variables expand.
fn build_cluster_script(m: &Machine) -> String {
    let mut s = String::new();
    if m.scheduler == Scheduler::Slurm {
        s.push_str(&format!(
            "echo '@@herdr:sinfo@@'\n{} -N -h -O 'NodeHost:|,Partition:|,Gres:|,GresUsed:|,StateCompact:|' 2>/dev/null\n\
             echo '@@herdr:pending@@'\n{} -u {} -t PD --start -h -o '%i|%S' 2>/dev/null\n",
            tool(m, "sinfo"),
            m.scheduler_command,
            m.scheduler_user
        ));
    }
    for (i, p) in storage_paths(m).iter().enumerate() {
        let p = match p.strip_prefix('~') {
            Some(rest) => format!("$HOME{rest}"),
            None => p.clone(),
        };
        let lfs = if p.starts_with('-') {
            String::new()
        } else {
            format!(
                "; [ \"$t\" = lustre ] && lfs quota -q -u {} \"$p\" 2>/dev/null | head -2",
                m.scheduler_user
            )
        };
        s.push_str(&format!(
            "echo '@@herdr:st:{i}@@'\n\
             for p in \"{p}\"; do [ -d \"$p\" ] || continue; echo \"$p\"; \
             t=$(stat -f -c %T -- \"$p\" 2>/dev/null); echo \"$t\"; \
             df -Pk -- \"$p\" 2>/dev/null | tail -1{lfs}; done\n"
        ));
    }
    s.push_str("echo '@@herdr:end@@'\n");
    s
}

/// Storage section body: path, fs type, df line, optional lfs quota lines
/// (a long fs name may wrap onto its own line).
fn storage_json(body: &str) -> Option<Value> {
    let mut l = body.lines();
    let path = l.next()?.trim();
    if path.is_empty() {
        return None;
    }
    let fs = l.next().unwrap_or("").trim();
    let (size_kb, used_kb, pct) = parse_df(l.next().unwrap_or(""))?;
    let rest: Vec<&str> = l.collect();
    let quota_line = match rest.as_slice() {
        [a, b, ..] if a.split_whitespace().count() == 1 => format!("{a} {b}"),
        [a, ..] => a.to_string(),
        [] => String::new(),
    };
    let quota = parse_lfs_quota(&quota_line)
        .map(|q| json!({ "used_kb": q.used_kb, "limit_kb": q.limit_kb, "pct": q.pct }));
    Some(json!({ "path": path, "fs": fs, "size_kb": size_kb, "used_kb": used_kb,
                 "pct": pct, "quota": quota }))
}

fn machine_json(m: &Machine, out: Result<crate::remote::RemoteOut, String>) -> Value {
    let empty = |e: String| {
        json!({ "name": m.name, "error": e, "partitions": [], "pending": [], "storage": [] })
    };
    let out = match out {
        Ok(o) => o,
        Err(e) => return empty(e),
    };
    let secs = sections(&out.stdout);
    if !secs.contains_key("end") {
        let e = out
            .stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("no output from cluster");
        return empty(e.to_string());
    }
    let get = |k: &str| secs.get(k).map_or("", String::as_str);
    let partitions: Vec<Value> = aggregate_gpus(get("sinfo"))
        .into_iter()
        .map(|p| json!({ "name": p.name, "gpu_type": p.gpu_type, "gpus_total": p.total,
                         "gpus_used": p.used, "gpus_free": p.free }))
        .collect();
    let pending: Vec<Value> = parse_pending(get("pending"))
        .into_iter()
        .map(|p| json!({ "id": p.id, "start": p.start }))
        .collect();
    let storage: Vec<Value> = (0..storage_paths(m).len())
        .filter_map(|i| storage_json(get(&format!("st:{i}"))))
        .collect();
    json!({ "name": m.name, "error": null, "partitions": partitions,
            "pending": pending, "storage": storage })
}

pub fn cmd_cluster() {
    let parsed = match load_parsed() {
        Ok(p) => p,
        Err(e) => return print_json(json!({ "error": e, "machines": [] })),
    };
    let machines: Vec<Value> = std::thread::scope(|s| {
        let handles: Vec<_> = parsed
            .machines
            .iter()
            .map(|m| {
                s.spawn(move || {
                    machine_json(m, run_remote(m, &build_cluster_script(m), Duration::from_secs(30)))
                })
            })
            .collect();
        handles
            .into_iter()
            .zip(&parsed.machines)
            .map(|(h, m)| {
                h.join().unwrap_or_else(|_| {
                    json!({ "name": m.name, "error": "query panicked",
                            "partitions": [], "pending": [], "storage": [] })
                })
            })
            .collect()
    });
    print_json(json!({ "error": null, "machines": machines }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_machine_entry;

    fn pg(name: &str, ty: &str, total: u64, used: u64, free: u64) -> PartitionGpus {
        PartitionGpus { name: name.into(), gpu_type: ty.into(), total, used, free }
    }

    #[test]
    fn gpus_typed_devana() {
        let s = "n141|gpu|gpu:a100-sxm4:4|gpu:a100-sxm4:1(IDX:1)|mix-|\n\
                 n142|gpu|gpu:a100-sxm4:4|gpu:a100-sxm4:4(IDX:0-3)|mix-|";
        assert_eq!(aggregate_gpus(s), vec![pg("gpu", "a100-sxm4", 8, 5, 3)]);
    }

    #[test]
    fn gpus_untyped_perun() {
        assert_eq!(aggregate_gpus("gpu03|gpu_short|gpu:8|gpu:6|mix-|"), vec![pg("gpu_short", "", 8, 6, 2)]);
    }

    #[test]
    fn drained_node_not_free() {
        let s = "n150|gpu|gpu:a100-sxm4:4|gpu:a100-sxm4:0(IDX:N/A)|drain|";
        assert_eq!(aggregate_gpus(s), vec![pg("gpu", "a100-sxm4", 4, 0, 0)]);
    }

    #[test]
    fn cpu_partitions_omitted() {
        assert_eq!(aggregate_gpus("n001|short|msrsafe:1,sysfs:1|msrsafe:0|idle|"), vec![]);
    }

    #[test]
    fn gres_ignores_non_gpu_and_socket_suffix() {
        assert_eq!(
            parse_gres("gpu:h100:2(S:0-1),shard:a:4,gpu:a:1(IDX:0,2)"),
            vec![("h100".into(), 2), ("a".into(), 1)]
        );
    }

    #[test]
    fn df_line_parses() {
        assert_eq!(
            parse_df("127.0.0.1:/ 287955146752 128954814464 159000332288 45% /scratch"),
            Some((287955146752, 128954814464, 45))
        );
    }

    #[test]
    fn lfs_quota_zero_limit_is_none() {
        assert_eq!(parse_lfs_quota("   /mnt/scratch       4       0       0       -       1       0       0       -"), None);
    }

    #[test]
    fn lfs_quota_with_limit_and_star() {
        assert_eq!(
            parse_lfs_quota("/project 950000000* 900000000 1000000000 - 1 0 0 -"),
            Some(Quota { used_kb: 950000000, limit_kb: 1000000000, pct: 95 })
        );
    }

    #[test]
    fn pending_na_is_null() {
        assert_eq!(
            parse_pending("124|N/A\n125|2026-11-17T14:00:00"),
            vec![
                Pending { id: "124".into(), start: None },
                Pending { id: "125".into(), start: Some("2026-11-17T14:00:00".into()) },
            ]
        );
    }

    fn machine(extra: &str) -> Machine {
        let entry: toml::Value = toml::from_str(&format!(
            "name = \"P\"\nhost = \"h\"\nscheduler_user = \"u\"\n{extra}"
        ))
        .unwrap();
        parse_machine_entry(&entry).unwrap()
    }

    #[test]
    fn script_quotes_paths_and_defaults() {
        let s = build_cluster_script(&machine(""));
        for p in DEFAULT_STORAGE {
            assert!(s.contains(&format!("for p in \"{p}\"")), "{s}");
        }
        assert!(s.contains("squeue -u u -t PD --start"));
        assert!(s.ends_with("echo '@@herdr:end@@'\n"));
        let s = build_cluster_script(&machine("storage = [\"~/x\", \"-odd\"]"));
        assert!(s.contains("for p in \"$HOME/x\""));
        assert!(s.contains("df -Pk -- \"$p\""));
        let odd = s.split("@@herdr:st:1@@").nth(1).unwrap();
        assert!(!odd.contains("lfs"));
        let pbs = build_cluster_script(&machine("scheduler = \"pbs\""));
        assert!(!pbs.contains("sinfo") && !pbs.contains("pending"));
    }

    #[test]
    fn machine_json_storage_and_missing_end() {
        let m = machine("storage = [\"/mnt/scratch\", \"/gone\"]");
        let stdout = "@@herdr:sinfo@@\n@@herdr:pending@@\n@@herdr:st:0@@\n/mnt/scratch\nlustre\n\
                      fs 100 40 60 40% /mnt/scratch\n/mnt/scratch\n 4 0 0 - 1 0 0 -\n@@herdr:st:1@@\n@@herdr:end@@\n";
        let ok = |s: &str| Ok(crate::remote::RemoteOut { stdout: s.into(), stderr: "boom\n".into(), ok: true });
        let v = machine_json(&m, ok(stdout));
        assert_eq!(v["error"], Value::Null);
        assert_eq!(
            v["storage"],
            json!([{ "path": "/mnt/scratch", "fs": "lustre", "size_kb": 100, "used_kb": 40, "pct": 40, "quota": null }])
        );
        let v = machine_json(&m, ok("@@herdr:sinfo@@\n"));
        assert_eq!(v["error"], "boom");
        assert_eq!(v["storage"], json!([]));
    }
}
