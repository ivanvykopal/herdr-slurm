//! Parsers for `herdr-slurm projects`: Slurm accounts, awarded and used
//! compute hours, and user / project storage quotas.
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// Awarded hours from a QOS `GrpTRESMins` (`<acct>_gpu` folded in).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Award {
    pub cpu_h: Option<u64>,
    pub gpu_h: Option<u64>,
    pub billing_h: Option<u64>,
}

/// Used hours of one account from `sreport … AccountUtilizationByUser`.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Usage {
    pub cpu: Option<u64>,
    pub gpu: Option<u64>,
    pub mine_cpu: Option<u64>,
    pub mine_gpu: Option<u64>,
    pub has_totals: bool,
}

/// One project of `sprojects -f` (hours).
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SProject {
    pub account: String,
    pub active: bool,
    pub cpu_used: Option<u64>,
    pub cpu_awarded: Option<u64>,
    pub gpu_used: Option<u64>,
    pub gpu_awarded: Option<u64>,
}

/// A user storage quota row (`myquota` / `perunfsusage`).
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Quota {
    pub kind: &'static str,
    pub name: String,
    pub path: String,
    pub used_kb: u64,
    pub limit_kb: u64,
    pub pct: Option<u8>,
}

/// A `perunfsusage` Project row; kept with a `TBD` limit (as `None`) so its
/// active / inactive status stays known.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectStorage {
    pub account: String,
    pub title: Option<String>,
    pub active: bool,
    pub path: String,
    pub used_kb: u64,
    pub limit_kb: Option<u64>,
    pub pct: Option<u8>,
}

/// GPU amounts split by TRES name: untyped `gres/gpu` wins over the sum of
/// typed `gres/gpu:<type>` entries, which would otherwise double count.
#[derive(Clone, Copy, Default)]
struct Gpu {
    plain: Option<u64>,
    typed: Option<u64>,
}

impl Gpu {
    /// Adds `v` when `tres` is a GPU TRES; other names are ignored.
    fn add(&mut self, tres: &str, v: u64) {
        if tres == "gres/gpu" {
            add(&mut self.plain, v);
        } else if tres.starts_with("gres/gpu:") {
            add(&mut self.typed, v);
        }
    }

    fn total(self) -> Option<u64> {
        self.plain.or(self.typed)
    }
}

fn add(slot: &mut Option<u64>, v: u64) {
    *slot = Some(slot.unwrap_or(0) + v);
}

/// Non-negative decimal number, rounded (`1947`, `12.6`).
fn rounded(s: &str) -> Option<u64> {
    let v: f64 = s.trim().parse().ok()?;
    (v.is_finite() && v >= 0.0).then(|| v.round() as u64)
}

/// `42.1%` → 42; `-` and other tokens → `None`.
fn parse_pct(s: &str) -> Option<u8> {
    rounded(s.strip_suffix('%')?).map(|v| v.min(255) as u8)
}

/// `s` without ANSI escape sequences (`ESC [ … letter`); `myquota` colours
/// figures over a threshold.
fn strip_ansi(s: &str) -> Cow<'_, str> {
    if !s.contains('\x1b') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
        } else if chars.next() == Some('[') {
            chars.by_ref().find(|c| c.is_ascii_alphabetic());
        }
    }
    Cow::Owned(out)
}

/// A token without whitespace that is not empty, as account names are.
fn is_name(s: &str) -> bool {
    !s.is_empty() && !s.contains(char::is_whitespace)
}

/// Account names from `sshare --parsable2` lines `Account|RawUsage|…`, in
/// first-seen order without duplicates.
#[allow(dead_code)]
pub(crate) fn parse_accounts(sshare: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in sshare.lines() {
        let Some((acct, _)) = line.split_once('|') else { continue };
        let acct = acct.trim();
        if is_name(acct) && !out.iter().any(|a| a == acct) {
            out.push(acct.to_string());
        }
    }
    out
}

/// Accounts whose association `GrpTRES` (`acct|GrpTRES`) contains `node=0`.
#[allow(dead_code)]
pub(crate) fn parse_expired(assoc: &str) -> HashSet<String> {
    assoc
        .lines()
        .filter_map(|l| {
            let (acct, tres) = l.split_once('|')?;
            let acct = acct.trim();
            (is_name(acct) && tres.split([',', '|']).any(|t| t.trim() == "node=0")).then(|| acct.to_string())
        })
        .collect()
}

/// QOS lines `name|GrpTRESMins` → awarded hours per account; `<acct>_gpu`
/// folds into `<acct>` (amounts summed); minutes / 60, rounded.
#[allow(dead_code)]
pub(crate) fn parse_awards(qos: &str) -> HashMap<String, Award> {
    #[derive(Default)]
    struct Mins {
        cpu: Option<u64>,
        gpu: Gpu,
        billing: Option<u64>,
    }
    let mut mins: HashMap<String, Mins> = HashMap::new();
    for line in qos.lines() {
        let Some((name, tres)) = line.split_once('|') else { continue };
        let name = name.trim();
        if !is_name(name) {
            continue;
        }
        let m = mins.entry(name.strip_suffix("_gpu").unwrap_or(name).to_string()).or_default();
        for entry in tres.split([',', '|']) {
            let Some((k, v)) = entry.trim().split_once('=') else { continue };
            let Ok(v) = v.parse::<u64>() else { continue };
            match k {
                "cpu" => add(&mut m.cpu, v),
                "billing" => add(&mut m.billing, v),
                _ => m.gpu.add(k, v),
            }
        }
    }
    let hours = |m: Option<u64>| m.map(|m| (m + 30) / 60);
    mins.into_iter()
        .map(|(acct, m)| {
            let award = Award { cpu_h: hours(m.cpu), gpu_h: hours(m.gpu.total()), billing_h: hours(m.billing) };
            (acct, award)
        })
        .collect()
}

/// `sreport … AccountUtilizationByUser --parsable2` rows
/// `Cluster|Account|Login|Proper Name|TRES Name|Used` → usage per account.
/// Totals come only from empty-Login rows, `mine_*` from Login == `me`;
/// header, separator and error lines are ignored.
#[allow(dead_code)]
pub(crate) fn parse_sreport(text: &str, me: &str) -> HashMap<String, Usage> {
    #[derive(Default)]
    struct Acc {
        cpu: Option<u64>,
        gpu: Gpu,
        mine_cpu: Option<u64>,
        mine_gpu: Gpu,
        has_totals: bool,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('|').map(str::trim).collect();
        if f.len() < 6 || !is_name(f[1]) {
            continue;
        }
        let Ok(used) = f[5].parse::<u64>() else { continue };
        let (login, tres) = (f[2], f[4]);
        let total = login.is_empty();
        if !total && login != me {
            continue;
        }
        let a = acc.entry(f[1].to_string()).or_default();
        a.has_totals |= total;
        let (cpu, gpu) = if total { (&mut a.cpu, &mut a.gpu) } else { (&mut a.mine_cpu, &mut a.mine_gpu) };
        if tres == "cpu" {
            add(cpu, used);
        } else {
            gpu.add(tres, used);
        }
    }
    acc.into_iter()
        .map(|(acct, a)| {
            let usage = Usage {
                cpu: a.cpu,
                gpu: a.gpu.total(),
                mine_cpu: a.mine_cpu,
                mine_gpu: a.mine_gpu.total(),
                has_totals: a.has_totals,
            };
            (acct, usage)
        })
        .collect()
}

/// `sprojects -f` tables: a project row `| acct | CPU: used/awarded | …`,
/// continuation rows `|  | GPU: used/awarded | …`, under the ACTIVE or
/// INACTIVE PROJECTS heading. `None` when the text is non-empty but no
/// project row parsed.
#[allow(dead_code)]
pub(crate) fn parse_sprojects(text: &str) -> Option<Vec<SProject>> {
    let text = strip_ansi(text);
    let mut out: Vec<SProject> = Vec::new();
    let (mut active, mut current) = (true, None);
    for line in text.lines() {
        let line = line.trim();
        if line.contains("INACTIVE PROJECTS") {
            (active, current) = (false, None);
            continue;
        }
        if line.contains("ACTIVE PROJECTS") {
            (active, current) = (true, None);
            continue;
        }
        if !line.starts_with('|') {
            current = None;
            continue;
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 3 {
            continue;
        }
        let (name, alloc) = (cells[1], cells[2]);
        if !name.is_empty() {
            if !is_name(name) || name == "Project" {
                current = None;
                continue;
            }
            out.push(SProject {
                account: name.to_string(),
                active,
                cpu_used: None,
                cpu_awarded: None,
                gpu_used: None,
                gpu_awarded: None,
            });
            current = Some(out.len() - 1);
        }
        let (Some(i), Some((kind, val))) = (current, alloc.split_once(':')) else { continue };
        let Some((used, awarded)) = val.split_once('/') else { continue };
        let p = &mut out[i];
        match kind.trim() {
            "CPU" => (p.cpu_used, p.cpu_awarded) = (rounded(used), rounded(awarded)),
            "GPU" => (p.gpu_used, p.gpu_awarded) = (rounded(used), rounded(awarded)),
            _ => {}
        }
    }
    (!out.is_empty() || text.trim().is_empty()).then_some(out)
}

/// `myquota` user rows `user <name> (<id>) <path> : <used> / <pct>% <limit> | …`.
/// `None` when the text is non-empty but no user row parsed.
#[allow(dead_code)]
pub(crate) fn parse_myquota(text: &str) -> Option<Vec<Quota>> {
    let text = strip_ansi(text);
    let mut out = Vec::new();
    for line in text.lines() {
        let data = line.split('|').next().unwrap_or("");
        let Some((who, data)) = data.split_once(':') else { continue };
        let who: Vec<&str> = who.split_whitespace().collect();
        let (Some(&"user"), Some(name), Some(path)) = (who.first(), who.get(1), who.last()) else { continue };
        if who.len() < 3 || !path.starts_with('/') {
            continue;
        }
        let f: Vec<&str> = data.split_whitespace().filter(|t| *t != "/").collect();
        let mut i = 0;
        let Some(used_kb) = take_size(&f, &mut i) else { continue };
        let pct = f.get(i).and_then(|t| parse_pct(t));
        if pct.is_some() {
            i += 1;
        }
        let Some(limit_kb) = take_size(&f, &mut i) else { continue };
        out.push(Quota { kind: "user", name: name.to_string(), path: path.to_string(), used_kb, limit_kb, pct });
    }
    (!out.is_empty() || text.trim().is_empty()).then_some(out)
}

/// `perunfsusage` rows `User|Project <name> <fs> <used> <limit> <pct>% [status]`;
/// an indented line right under a Project row is its title. User rows with
/// limit `TBD` are dropped; Project rows are kept with `limit_kb: None`.
/// `None` when the text is non-empty but no row parsed.
#[allow(dead_code)]
pub(crate) fn parse_perunfsusage(text: &str) -> Option<(Vec<Quota>, Vec<ProjectStorage>)> {
    let text = strip_ansi(text);
    let (mut users, mut projects) = (Vec::new(), Vec::<ProjectStorage>::new());
    let mut parsed = false;
    let mut titled: Option<usize> = None; // project whose title line may follow
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) {
            if let Some(i) = titled.take() {
                let title = line.trim();
                if !title.is_empty() {
                    projects[i].title = Some(title.to_string());
                }
            }
            continue;
        }
        titled = None;
        let f: Vec<&str> = line.split_whitespace().collect();
        let user = match f.first() {
            Some(&"User") => true,
            Some(&"Project") => false,
            _ => continue,
        };
        if f.len() < 6 || !f[2].starts_with('/') {
            continue;
        }
        let mut i = 3;
        let Some(used_kb) = take_size(&f, &mut i) else { continue };
        let limit_kb = if f.get(i) == Some(&"TBD") {
            i += 1;
            None
        } else {
            let Some(limit) = take_size(&f, &mut i) else { continue };
            Some(limit)
        };
        let pct = f.get(i).and_then(|t| parse_pct(t));
        let active = f.get(i + 1) != Some(&"inactive");
        parsed = true;
        let (name, path) = (f[1].to_string(), f[2].to_string());
        if user {
            if let Some(limit_kb) = limit_kb {
                users.push(Quota { kind: "user", name, path, used_kb, limit_kb, pct });
            }
        } else {
            projects.push(ProjectStorage { account: name, title: None, active, path, used_kb, limit_kb, pct });
            titled = Some(projects.len() - 1);
        }
    }
    (parsed || text.trim().is_empty()).then_some((users, projects))
}

/// `4.00 KB`, `211 GB`, `1.01 TB`, `863GB` → KiB (units are powers of 1024).
#[allow(dead_code)]
pub(crate) fn parse_size_kib(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    size_kib(&s[..split], s[split..].trim())
}

fn size_kib(num: &str, unit: &str) -> Option<u64> {
    let exp = match unit {
        "KB" => 0,
        "MB" => 1,
        "GB" => 2,
        "TB" => 3,
        _ => return None,
    };
    let v: f64 = num.parse().ok()?;
    (v.is_finite() && v >= 0.0).then(|| (v * 1024f64.powi(exp)).round() as u64)
}

/// A size at `f[*i]`, one token (`863GB`) or a number and a unit (`211 GB`);
/// advances `i` past it.
fn take_size(f: &[&str], i: &mut usize) -> Option<u64> {
    let first = *f.get(*i)?;
    if let Some(v) = parse_size_kib(first) {
        *i += 1;
        return Some(v);
    }
    let v = size_kib(first, f.get(*i + 1)?)?;
    *i += 2;
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERUN: &str = include_str!("../tests/fixtures/perun-projects.txt");
    const DEVANA: &str = include_str!("../tests/fixtures/devana-projects.txt");

    /// Body of the `@@<name>` section of a fixture.
    fn section<'a>(text: &'a str, name: &str) -> &'a str {
        let (mut pos, mut start) = (0, None);
        for line in text.split_inclusive('\n') {
            if line.starts_with("@@") {
                if let Some(s) = start {
                    return &text[s..pos];
                }
                if line.trim_end() == format!("@@{name}") {
                    start = Some(pos + line.len());
                }
            }
            pos += line.len();
        }
        &text[start.unwrap_or_else(|| panic!("no section {name}"))..]
    }

    const GIB: u64 = 1024 * 1024;

    #[test]
    fn accounts_from_sshare() {
        assert_eq!(
            parse_accounts(section(PERUN, "sshare")),
            vec!["perun25012246", "perun250128", "perun2601397", "perun2601474", "perun2601475", "perun2601476"]
        );
        assert_eq!(
            parse_accounts(section(DEVANA, "sshare")),
            vec!["p122-23-t", "p1605-25-3", "p1626-25-3", "p365-23-1", "p904-24-3"]
        );
    }

    #[test]
    fn expired_accounts_from_assoc() {
        let perun = parse_expired(section(PERUN, "assoc"));
        assert_eq!(perun, HashSet::from(["perun250128".to_string(), "perun25012246".to_string()]));
        assert!(parse_expired(section(DEVANA, "assoc")).is_empty());
    }

    #[test]
    fn awards_perun_from_qos() {
        let a = parse_awards(section(PERUN, "qos"));
        assert_eq!(a.len(), 6);
        assert_eq!(a["perun2601397"], Award { cpu_h: Some(88474), gpu_h: Some(7964), billing_h: None });
    }

    #[test]
    fn awards_devana_billing_split() {
        let a = parse_awards(section(DEVANA, "qos"));
        assert_eq!(a.len(), 5);
        assert!(!a.contains_key("p1605-25-3_gpu"));
        assert_eq!(a["p1605-25-3"], Award { cpu_h: None, gpu_h: None, billing_h: Some(325000) });
    }

    #[test]
    fn awards_typed_gpu_summed_untyped_preferred() {
        let a = parse_awards("x|cpu=600,gres/gpu:a100=120,gres/gpu:h100=90\ny|gres/gpu:a100=120,gres/gpu=60\n");
        assert_eq!(a["x"], Award { cpu_h: Some(10), gpu_h: Some(4), billing_h: None });
        assert_eq!(a["y"].gpu_h, Some(1));
    }

    #[test]
    fn sreport_totals_and_mine_perun() {
        let u = parse_sreport(section(PERUN, "sreport"), "ivvyke441");
        assert_eq!(u.len(), 6);
        let p = u["perun2601397"];
        assert!(p.has_totals);
        assert_eq!((p.cpu, p.gpu), (Some(8154), Some(5253)));
        assert_eq!((p.mine_cpu, p.mine_gpu), (Some(8154), Some(5253)));
        let shared = u["perun250128"];
        assert_eq!((shared.cpu, shared.gpu, shared.mine_gpu), (Some(56588), Some(8247), Some(0)));
        let other = u["perun2601475"];
        assert_eq!((other.cpu, other.mine_cpu), (Some(15), None));
    }

    #[test]
    fn sreport30_ignores_error_line() {
        let u = parse_sreport(section(PERUN, "sreport30"), "ivvyke441");
        assert_eq!(u.len(), 1);
        let p = u["perun2601474"];
        assert!(p.has_totals);
        assert_eq!((p.cpu, p.gpu, p.mine_cpu, p.mine_gpu), (Some(579), Some(72), None, None));
        assert!(parse_sreport(section(DEVANA, "sreport30"), "ivykopal").is_empty());
    }

    #[test]
    fn sreport_typed_gpu_counts() {
        let u = parse_sreport("c|a|||gres/gpu:a100|7\nc|a|me|Me|gres/gpu:a100|3\nc|a|me|Me|gres/gpu:h100|2\n", "me");
        assert_eq!((u["a"].gpu, u["a"].mine_gpu), (Some(7), Some(5)));
    }

    #[test]
    fn sreport_without_totals_gives_null_used() {
        let u = parse_sreport(section(DEVANA, "sreport"), "ivykopal");
        let p = u["p1605-25-3"];
        assert!(!p.has_totals);
        assert_eq!((p.cpu, p.gpu), (None, None));
        assert_eq!((p.mine_cpu, p.mine_gpu), (Some(11745), Some(1454)));
    }

    #[test]
    fn sprojects_active_and_inactive() {
        let p = parse_sprojects(section(DEVANA, "sprojects")).unwrap();
        assert_eq!(p.len(), 5);
        assert_eq!(
            p[0],
            SProject {
                account: "p1605-25-3".into(),
                active: true,
                cpu_used: Some(1947),
                cpu_awarded: Some(5000),
                gpu_used: Some(7090),
                gpu_awarded: Some(20000),
            }
        );
        assert!(p[1].active && p[1].account == "p1626-25-3");
        let inactive = p.iter().find(|s| s.account == "p904-24-3").unwrap();
        assert!(!inactive.active);
        assert_eq!((inactive.gpu_used, inactive.gpu_awarded), (Some(17107), Some(30000)));
    }

    #[test]
    fn unparseable_sprojects_returns_none() {
        assert_eq!(parse_sprojects("garbage\nlines"), None);
        assert_eq!(parse_sprojects(""), Some(vec![]));
    }

    #[test]
    fn myquota_user_home() {
        let q = parse_myquota(section(DEVANA, "myquota")).unwrap();
        assert_eq!(
            q,
            vec![Quota {
                kind: "user",
                name: "ivykopal".into(),
                path: "/home".into(),
                used_kb: 863 * GIB,
                limit_kb: 1000 * GIB,
                pct: Some(86),
            }]
        );
        assert_eq!(parse_myquota("garbage"), None);
        assert_eq!(parse_myquota(""), Some(vec![]));
    }

    #[test]
    fn perunfsusage_rows() {
        let (users, projects) = parse_perunfsusage(section(PERUN, "perunfsusage")).unwrap();
        assert_eq!(
            users,
            vec![Quota {
                kind: "user",
                name: "ivvyke441".into(),
                path: "/mnt/home".into(),
                used_kb: 211 * GIB,
                limit_kb: 500 * GIB,
                pct: Some(42),
            }]
        );
        assert_eq!(projects.len(), 6);
        let p = projects.iter().find(|p| p.account == "perun2601475").unwrap();
        assert_eq!(p.pct, Some(98));
        assert!(p.active);
        assert_eq!(p.path, "/mnt/project");
        assert_eq!(p.title.as_deref(), Some("Vision-Language Model Adaptation to the Slovak Language"));
        let gone = projects.iter().find(|p| p.account == "perun25012246").unwrap();
        assert!(!gone.active);
        assert_eq!((gone.limit_kb, gone.pct, gone.title.as_deref()), (None, None, None));
        let last = projects.iter().find(|p| p.account == "perun2601397").unwrap();
        assert_eq!(last.title.as_deref(), Some("Slovak LLM Benchmarking"));
        assert_eq!(parse_perunfsusage("garbage"), None);
        assert_eq!(parse_perunfsusage(""), Some((vec![], vec![])));
    }

    #[test]
    fn size_units() {
        assert_eq!(parse_size_kib("4.00 KB"), Some(4));
        assert_eq!(parse_size_kib("211 GB"), Some(211 * GIB));
        assert_eq!(parse_size_kib("1.01 TB"), Some((1.01f64 * 1024f64.powi(3)).round() as u64));
        assert_eq!(parse_size_kib("863GB"), Some(863 * GIB));
        assert_eq!(parse_size_kib("2 MB"), Some(2048));
        assert_eq!(parse_size_kib("TBD"), None);
        assert_eq!(parse_size_kib("12 XB"), None);
    }
}
