//! `herdr-slurm job <machine> <id> [--lines N] [--gpu]`: one job's state,
//! allocation, log tails, step usage and (optionally) live GPU use.
use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::remote::{
    find_machine, load_parsed, print_json, run_remote, sections, tool, valid_job_id,
};
use crate::{normalize_pbs_state, sanitize_display_text, truncate, Machine, Scheduler};

const USAGE: &str = "usage: herdr-slurm job <machine> <id> [--lines N] [--gpu]";

#[derive(Debug)]
struct JobArgs {
    machine: String,
    id: String,
    lines: u32,
    gpu: bool,
}

#[derive(Debug, PartialEq)]
struct GpuUse {
    node: String,
    index: u32,
    util: u32,
    mem_used_mib: u64,
    mem_total_mib: u64,
}

/// Parse `<machine> <id> [--lines N] [--gpu]`; `N` defaults to 50 and is
/// clamped to 1..=500. `Err` carries the message plus whatever positionals
/// were seen, so the error JSON can still name them.
fn parse_args(args: &[String]) -> Result<JobArgs, (String, Vec<String>)> {
    let mut pos = Vec::new();
    let mut lines = 50u64;
    let mut gpu = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--gpu" => gpu = true,
            "--lines" => match it.next().map(|v| v.parse::<u64>()) {
                Some(Ok(n)) => lines = n,
                Some(Err(_)) => return Err(("invalid --lines value".into(), pos)),
                None => return Err(("--lines needs a value".into(), pos)),
            },
            flag if flag.starts_with("--") => {
                return Err((format!("unknown option '{flag}'"), pos))
            }
            _ => pos.push(a.clone()),
        }
    }
    if pos.len() != 2 {
        return Err((USAGE.into(), pos));
    }
    let id = pos.pop().unwrap_or_default();
    let machine = pos.pop().unwrap_or_default();
    Ok(JobArgs {
        machine,
        id,
        lines: lines.clamp(1, 500) as u32,
        gpu,
    })
}

/// `Key=Value` tokens of one `scontrol show job -o` line. Tokens are split on
/// single spaces; values containing spaces are not supported.
fn parse_scontrol_kv(line: &str) -> HashMap<String, String> {
    line.split(' ')
        .filter_map(|t| t.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// `(cpus, mem, gpus)` from an AllocTRES string such as
/// `cpu=16,mem=64G,node=1,gres/gpu=4`; gpus sum `gres/gpu=` and
/// `gres/gpu:<type>=` entries.
fn parse_alloc_tres(s: &str) -> (Option<u64>, Option<String>, u64) {
    let (mut cpus, mut mem, mut gpus) = (None, None, 0);
    for (k, v) in s.split(',').filter_map(|t| t.split_once('=')) {
        match k {
            "cpu" => cpus = v.parse().ok(),
            "mem" => mem = Some(v.to_string()),
            k if k == "gres/gpu" || k.starts_with("gres/gpu:") => {
                gpus += v.parse::<u64>().unwrap_or(0)
            }
            _ => {}
        }
    }
    (cpus, mem, gpus)
}

/// Lines `node,index,util,mem_used,mem_total` (host-prefixed `nvidia-smi`
/// csv); anything else is skipped.
fn parse_gpu_lines(text: &str) -> Vec<GpuUse> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            if f.len() != 5 || f[0].is_empty() {
                return None;
            }
            Some(GpuUse {
                node: f[0].to_string(),
                index: f[1].parse().ok()?,
                util: f[2].parse().ok()?,
                mem_used_mib: f[3].parse().ok()?,
                mem_total_mib: f[4].parse().ok()?,
            })
        })
        .collect()
}

/// One remote shell script printing `@@herdr:<section>@@` blocks. `id` must
/// already have passed `valid_job_id`; log paths are double-quoted.
fn build_job_script(m: &Machine, id: &str, lines: u32, gpu: bool) -> String {
    let tails = format!(
        "echo '@@herdr:out@@'\n\
         if [ -n \"$o\" ]; then tail -n {lines} \"$o\" 2>/dev/null || echo \"(cannot read $o)\"; fi\n\
         if [ -n \"$e\" ] && [ \"$e\" != \"$o\" ]; then echo '@@herdr:err@@'; \
         tail -n {lines} \"$e\" 2>/dev/null || echo \"(cannot read $e)\"; fi\n"
    );
    match m.scheduler {
        Scheduler::Slurm => {
            let field = |k: &str| {
                format!("$(printf '%s\\n' \"$S\" | sed -n 's/.* {k}=\\([^ ]*\\).*/\\1/p' | head -n 1)")
            };
            let mut s = format!(
                "S=$({scontrol} show job -o {id} 2>&1)\n\
                 echo '@@herdr:scontrol@@'\n\
                 printf '%s\\n' \"$S\"\n\
                 o={o}\n\
                 e={e}\n\
                 {tails}\
                 echo '@@herdr:sstat@@'\n\
                 {sstat} -a -n -P -j {id} -o JobID,AveCPU,MaxRSS 2>/dev/null\n",
                scontrol = tool(m, "scontrol"),
                sstat = tool(m, "sstat"),
                o = field("StdOut"),
                e = field("StdErr"),
            );
            if gpu {
                s.push_str(&format!(
                    "case \"$S\" in *' JobState=RUNNING '*)\n\
                     echo '@@herdr:gpu@@'\n\
                     {{ g=$(timeout 25 {srun} --jobid={id} --overlap --ntasks-per-node=1 sh -c \
                     'nvidia-smi --query-gpu=index,utilization.gpu,memory.used,memory.total \
                     --format=csv,noheader,nounits | sed \"s/^/$(hostname),/\"' 2>&1 1>&3 3>&-); r=$?; }} 3>&1\n\
                     echo '@@herdr:gpuerr@@'\n\
                     printf '%s\\n' \"$g\"\n\
                     case $r in 0) ;; 124) echo 'timed out after 25 s' ;; *) echo \"exit $r\" ;; esac\n\
                     ;; esac\n",
                    srun = tool(m, "srun"),
                ));
            }
            s
        }
        // `qstat -f` wraps long values onto tab-indented continuation lines;
        // join them before reading `key = value` pairs.
        Scheduler::Pbs => format!(
            "J=$({qstat} -f '{id}' 2>&1 | sed -e ':a' -e '$!N' -e 's/\\n\\t//' -e 'ta' -e 'P' -e 'D')\n\
             echo '@@herdr:qstat@@'\n\
             printf '%s\\n' \"$J\"\n\
             o=$(printf '%s\\n' \"$J\" | sed -n 's/^ *Output_Path = //p' | head -n 1); o=${{o#*:}}\n\
             e=$(printf '%s\\n' \"$J\" | sed -n 's/^ *Error_Path = //p' | head -n 1); e=${{e#*:}}\n\
             {tails}",
            qstat = m.scheduler_command,
        ),
    }
}

/// Section body as display lines: a `\r`-rewritten line (progress bars)
/// keeps its last segment, other control characters become spaces.
fn tail_lines(secs: &HashMap<String, String>, name: &str) -> Vec<String> {
    secs.get(name)
        .map(|body| {
            body.lines()
                .map(|l| {
                    let l = l.trim_end_matches('\r');
                    sanitize_display_text(l.rsplit('\r').next().unwrap_or(l))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn last_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(|l| truncate(l, 200))
}

fn err_json(machine: Option<&str>, id: Option<&str>, error: &str) -> Value {
    let mut v = json!({ "error": error });
    if let Some(m) = machine {
        v["machine"] = json!(m);
    }
    if let Some(i) = id {
        v["id"] = json!(i);
    }
    v
}

/// Build the `job` JSON from the remote sections; `stderr` is ssh's stderr,
/// used for the error when the scheduler section is missing.
fn job_json(
    m: &Machine,
    id: &str,
    gpu: bool,
    secs: &HashMap<String, String>,
    stderr: &str,
) -> Value {
    let (key, marker) = match m.scheduler {
        Scheduler::Slurm => ("scontrol", "JobId="),
        Scheduler::Pbs => ("qstat", "Job Id:"),
    };
    let body = secs.get(key).map(String::as_str).unwrap_or("");
    if !body.lines().any(|l| l.trim_start().starts_with(marker)) {
        let error = last_line(body)
            .or_else(|| last_line(stderr))
            .unwrap_or_else(|| "no output from cluster".into());
        return err_json(Some(&m.name), Some(id), &error);
    }
    let out_tail = tail_lines(secs, "out");
    let (state, node_list, start_time, stdout, stderr_path, alloc, steps, gpus, gpu_error);
    match m.scheduler {
        Scheduler::Slurm => {
            let line = body.lines().find(|l| l.starts_with(marker)).unwrap_or("");
            let kv = parse_scontrol_kv(line);
            let get = |k: &str| kv.get(k).cloned();
            let (cpus, mem, g) = parse_alloc_tres(kv.get("AllocTRES").map_or("", String::as_str));
            state = get("JobState");
            node_list = get("NodeList");
            start_time = get("StartTime");
            stdout = get("StdOut");
            stderr_path = get("StdErr");
            alloc = json!({ "cpus": cpus, "mem": mem, "gpus": g });
            steps = secs
                .get("sstat")
                .map(|t| {
                    t.lines()
                        .filter_map(|l| {
                            let f: Vec<&str> = l.trim().split('|').collect();
                            (f.len() >= 3 && !f[0].is_empty())
                                .then(|| json!({ "step": f[0], "ave_cpu": f[1], "max_rss": f[2] }))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let used = parse_gpu_lines(secs.get("gpu").map_or("", String::as_str));
            gpu_error = if !gpu || state.as_deref() != Some("RUNNING") || !used.is_empty() {
                None
            } else {
                let msg: Vec<&str> = secs
                    .get("gpuerr")
                    .map_or("", String::as_str)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
                Some(if msg.is_empty() {
                    "no GPU data".to_string()
                } else {
                    truncate(&msg.join("; "), 300)
                })
            };
            gpus = used
                .iter()
                .map(|g| {
                    json!({ "node": g.node, "index": g.index, "util": g.util,
                            "mem_used_mib": g.mem_used_mib, "mem_total_mib": g.mem_total_mib })
                })
                .collect::<Vec<_>>();
        }
        Scheduler::Pbs => {
            let kv: HashMap<&str, &str> = body
                .lines()
                .filter_map(|l| l.trim().split_once(" = "))
                .collect();
            let get = |k: &str| kv.get(k).map(|v| v.to_string());
            let path = |k: &str| {
                kv.get(k)
                    .map(|v| v.split_once(':').map_or(*v, |(_, p)| p).to_string())
            };
            state = get("job_state").map(|s| normalize_pbs_state(&s));
            node_list = get("exec_host").map(|h| {
                let mut nodes: Vec<&str> = Vec::new();
                for n in h.split('+').map(|c| c.split('/').next().unwrap_or(c)) {
                    if !nodes.contains(&n) {
                        nodes.push(n);
                    }
                }
                nodes.join(",")
            });
            start_time = get("stime");
            stdout = path("Output_Path");
            stderr_path = path("Error_Path");
            let num = |k: &str| kv.get(k).and_then(|v| v.parse::<u64>().ok());
            alloc = json!({
                "cpus": num("Resource_List.ncpus"),
                "mem": get("Resource_List.mem"),
                "gpus": num("Resource_List.ngpus").unwrap_or(0),
            });
            steps = Vec::new();
            gpus = Vec::new();
            gpu_error = gpu.then(|| "not supported on PBS".to_string());
        }
    }
    let err_tail = if stderr_path == stdout {
        Vec::new()
    } else {
        tail_lines(secs, "err")
    };
    json!({
        "error": null, "machine": m.name, "id": id, "state": state,
        "node_list": node_list, "start_time": start_time,
        "stdout": stdout, "stderr": stderr_path, "alloc": alloc,
        "out_tail": out_tail, "err_tail": err_tail, "steps": steps,
        "gpus": gpus, "gpu_error": gpu_error,
    })
}

pub fn cmd_job() {
    let argv: Vec<String> = std::env::args().skip(2).collect();
    let a = match parse_args(&argv) {
        Ok(a) => a,
        Err((e, pos)) => {
            print_json(err_json(
                pos.first().map(String::as_str),
                pos.get(1).map(String::as_str),
                &e,
            ));
            std::process::exit(2);
        }
    };
    let (name, id) = (Some(a.machine.as_str()), Some(a.id.as_str()));
    let parsed = match load_parsed() {
        Ok(p) => p,
        Err(e) => return print_json(err_json(name, id, &e)),
    };
    let Some(m) = find_machine(&parsed.machines, &a.machine) else {
        return print_json(err_json(
            name,
            id,
            &format!("unknown machine '{}'", a.machine),
        ));
    };
    if !valid_job_id(&m.scheduler, &a.id) {
        print_json(err_json(name, id, &format!("invalid job id '{}'", a.id)));
        std::process::exit(2);
    }
    let script = build_job_script(m, &a.id, a.lines, a.gpu);
    let v = match run_remote(m, &script, Duration::from_secs(30)) {
        Ok(out) => job_json(m, &a.id, a.gpu, &sections(&out.stdout), &out.stderr),
        Err(e) => err_json(name, id, &e),
    };
    print_json(v);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_machine_entry;

    fn machine(scheduler: &str, scheduler_command: &str) -> Machine {
        let entry: toml::Value = toml::from_str(&format!(
            "name = \"Devana\"\nhost = \"h\"\nscheduler = \"{scheduler}\"\nscheduler_user = \"u\"\nscheduler_command = \"{scheduler_command}\""
        ))
        .unwrap();
        parse_machine_entry(&entry).unwrap()
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    const SCONTROL: &str = "JobId=123 JobState=RUNNING StdOut=/h/u/o.out StdErr=/h/u/o.out NodeList=n141 StartTime=2026-11-17T10:02:11 AllocTRES=cpu=16,mem=64G,node=1,billing=16,gres/gpu=4";

    #[test]
    fn scontrol_kv_reads_paths_and_tres() {
        let kv = parse_scontrol_kv(SCONTROL);
        assert_eq!(kv["StdOut"], "/h/u/o.out");
        assert_eq!(kv["JobState"], "RUNNING");
        assert_eq!(
            parse_alloc_tres(&kv["AllocTRES"]),
            (Some(16), Some("64G".to_string()), 4)
        );
    }

    #[test]
    fn alloc_tres_sums_typed_gpus() {
        assert_eq!(
            parse_alloc_tres("cpu=8,gres/gpu:a100=2,gres/gpu:v100=1"),
            (Some(8), None, 3)
        );
        assert_eq!(parse_alloc_tres("(null)"), (None, None, 0));
    }

    #[test]
    fn gpu_lines_parse_node_prefix() {
        let g = parse_gpu_lines("n141,0, 97, 30000, 40960\nsrun: warning\nn141,x,1,2,3\n");
        assert_eq!(
            g,
            vec![GpuUse {
                node: "n141".into(),
                index: 0,
                util: 97,
                mem_used_mib: 30000,
                mem_total_mib: 40960
            }]
        );
    }

    #[test]
    fn job_err_tail_empty_when_same_file() {
        let m = machine("slurm", "squeue");
        let secs = crate::remote::sections(&format!(
            "@@herdr:scontrol@@\n{SCONTROL}\n@@herdr:out@@\nline1\nline2\n@@herdr:err@@\nline1\n@@herdr:sstat@@\n123.batch|01:02:03|12G\n"
        ));
        let v = job_json(&m, "123", false, &secs, "");
        assert_eq!(v["error"], serde_json::Value::Null);
        assert_eq!(v["state"], "RUNNING");
        assert_eq!(v["node_list"], "n141");
        assert_eq!(v["alloc"]["mem"], "64G");
        assert_eq!(v["alloc"]["gpus"], 4);
        assert_eq!(v["out_tail"], serde_json::json!(["line1", "line2"]));
        assert_eq!(v["err_tail"], serde_json::json!([]));
        assert_eq!(v["steps"][0]["step"], "123.batch");
        assert_eq!(v["steps"][0]["max_rss"], "12G");
        assert_eq!(v["gpus"], serde_json::json!([]));
        assert_eq!(v["gpu_error"], serde_json::Value::Null);
    }

    #[test]
    fn job_not_found_reports_scheduler_error() {
        let m = machine("slurm", "squeue");
        let secs = crate::remote::sections(
            "@@herdr:scontrol@@\nslurm_load_jobs error: Invalid job id specified\n@@herdr:sstat@@\n",
        );
        let v = job_json(&m, "1", false, &secs, "");
        assert_eq!(
            v["error"],
            "slurm_load_jobs error: Invalid job id specified"
        );
        assert_eq!(v["machine"], "Devana");
        assert_eq!(v["id"], "1");
        assert!(v.get("state").is_none());
    }

    #[test]
    fn gpu_failure_goes_to_gpu_error() {
        let m = machine("slurm", "squeue");
        let secs = crate::remote::sections(&format!(
            "@@herdr:scontrol@@\n{SCONTROL}\n@@herdr:gpu@@\n@@herdr:gpuerr@@\nsrun: error: oops\ntimed out after 25 s\n"
        ));
        let v = job_json(&m, "123", true, &secs, "");
        assert_eq!(v["gpus"], serde_json::json!([]));
        assert_eq!(v["gpu_error"], "srun: error: oops; timed out after 25 s");
    }

    #[test]
    fn pbs_job_reads_qstat_text() {
        let m = machine("pbs", "qstat");
        let secs = crate::remote::sections(
            "@@herdr:qstat@@\nJob Id: 4021.pbs01\n    job_state = R\n    exec_host = c1/0*8+c2/0*8\n    stime = Mon Nov 17 10:02:11 2026\n    Output_Path = c0:/h/u/j.o4021\n    Error_Path = c0:/h/u/j.e4021\n    Resource_List.ncpus = 16\n    Resource_List.mem = 64gb\n    Resource_List.ngpus = 2\n@@herdr:out@@\nhello\n@@herdr:err@@\n(cannot read /h/u/j.e4021)\n",
        );
        let v = job_json(&m, "4021.pbs01", true, &secs, "");
        assert_eq!(v["error"], serde_json::Value::Null);
        assert_eq!(v["state"], "RUNNING");
        assert_eq!(v["node_list"], "c1,c2");
        assert_eq!(v["stdout"], "/h/u/j.o4021");
        assert_eq!(
            v["alloc"],
            serde_json::json!({"cpus": 16, "mem": "64gb", "gpus": 2})
        );
        assert_eq!(
            v["err_tail"],
            serde_json::json!(["(cannot read /h/u/j.e4021)"])
        );
        assert_eq!(v["steps"], serde_json::json!([]));
        assert_eq!(v["gpu_error"], "not supported on PBS");
    }

    #[test]
    fn job_script_rejects_nothing_unvalidated() {
        let s = build_job_script(&machine("slurm", "/opt/slurm/bin/squeue"), "123", 50, true);
        assert!(s.contains("/opt/slurm/bin/scontrol show job -o 123"), "{s}");
        assert!(s.contains("tail -n 50"));
        assert!(s.contains("/opt/slurm/bin/sstat -a -n -P -j 123 -o JobID,AveCPU,MaxRSS"));
        assert!(s.contains("timeout 25 /opt/slurm/bin/srun --jobid=123 --overlap"));
        assert!(!build_job_script(&machine("slurm", "squeue"), "123", 50, false).contains("srun"));
        let p = build_job_script(&machine("pbs", "qstat"), "4021[].pbs01", 5, false);
        assert!(p.contains("qstat -f '4021[].pbs01'"), "{p}");
        assert!(p.contains("tail -n 5"));
    }

    #[test]
    fn lines_clamped() {
        let a = parse_args(&args(&["Devana", "123", "--lines", "0"])).unwrap();
        assert_eq!(a.lines, 1);
        let a = parse_args(&args(&["Devana", "123", "--lines", "9999", "--gpu"])).unwrap();
        assert_eq!((a.lines, a.gpu), (500, true));
        let a = parse_args(&args(&["Devana", "123"])).unwrap();
        assert_eq!(
            (a.machine.as_str(), a.id.as_str(), a.lines, a.gpu),
            ("Devana", "123", 50, false)
        );
    }

    #[test]
    fn bad_args_rejected() {
        assert!(parse_args(&args(&["Devana"])).is_err());
        assert!(parse_args(&args(&["Devana", "1", "--lines", "x"])).is_err());
        assert!(parse_args(&args(&["Devana", "1", "--lines"])).is_err());
        assert!(parse_args(&args(&["Devana", "1", "--bogus"])).is_err());
        assert!(parse_args(&args(&["Devana", "1", "2"])).is_err());
    }
}
