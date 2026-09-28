use std::collections::HashMap;
use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

const REFRESH_SECS: u64 = 20;
const TARGET_COLS: usize = 50; // sidebar width used by the `open` subcommand
const SQUEUE_FMT: &str = "%.18i|%j|%T|%M|%D|%a|%R|%N";
const MAX_PBS_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_SCHEDULER_STDOUT_BYTES: usize = MAX_PBS_JSON_BYTES;
const MAX_SCHEDULER_STDERR_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Scheduler {
    Slurm,
    Pbs,
}

#[derive(Clone, Debug)]
struct Machine {
    name: String,
    host: String,
    scheduler: Scheduler,
    scheduler_user: String,
    scheduler_command: String,
    ssh_user: Option<String>,
}

impl Machine {
    fn ssh_target(&self) -> String {
        match &self.ssh_user {
            Some(user) if !user.is_empty() => format!("{user}@{}", self.host),
            _ => self.host.clone(),
        }
    }
}

#[derive(Clone)]
struct Job {
    id: String,
    name: String,
    state: String,
    elapsed: String,
    nodes: String,
    account: String,
    reason: String,
    where_: String,
}

type QueryResult = Result<Vec<Job>, String>;

// ------------------------------------------------------------------ config --

fn config_dir() -> PathBuf {
    env::var("HERDR_PLUGIN_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut p = env::current_exe().unwrap_or_default();
            p.pop();
            p
        })
}

const TEMPLATE: &str = r#"# herdr scheduler machines. One [[machine]] block per cluster.
# `scheduler` is "slurm" (default) or "pbs".
# `scheduler_user` is the account whose jobs should be shown.
# `scheduler_command` optionally sets the remote scheduler executable.
# SSH uses Host/User from ~/.ssh/config unless `ssh_user` is set.
# Legacy `user` remains an alias for `scheduler_user`.
# `name` should match a saved `herdr machine` label so the sidebar token
# lands under the right machine.

[[machine]]
name = "MyCluster"
host = "login.example.org"
scheduler = "slurm"
scheduler_user = "your-cluster-login"
# scheduler_command = "/path/to/squeue"
"#;

fn load_machines() -> Option<Vec<Machine>> {
    let path = config_dir().join("machines.toml");
    if !path.exists() {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(&path, TEMPLATE);
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            println!("herdr-slurm: cannot read {}: {}", path.display(), e);
            return None;
        }
    };
    let out = match parse_machines(&raw) {
        Ok(v) => v,
        Err(e) => {
            println!("herdr-slurm: invalid TOML in {}: {}", path.display(), e);
            return None;
        }
    };
    Some(out)
}

fn parse_machines(raw: &str) -> Result<Vec<Machine>, String> {
    let value: toml::Value = toml::from_str(raw).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(list) = value.get("machine").and_then(|v| v.as_array()) {
        for m in list {
            let g = |k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let legacy_user = g("user");
            let scheduler = match g("scheduler").to_ascii_lowercase().as_str() {
                "" | "slurm" => Scheduler::Slurm,
                "pbs" | "openpbs" => Scheduler::Pbs,
                other => return Err(format!("unsupported scheduler '{other}'")),
            };
            let scheduler_command = match g("scheduler_command") {
                value if value.is_empty() => match scheduler {
                    Scheduler::Slurm => "squeue".into(),
                    Scheduler::Pbs => "qstat".into(),
                },
                value => value,
            };
            if scheduler_command.starts_with('-')
                || !scheduler_command
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '\\' | '_' | '-' | '.'))
            {
                return Err(format!("invalid scheduler_command '{scheduler_command}'"));
            }
            let scheduler_user = match g("scheduler_user") {
                value if value.is_empty() => legacy_user,
                value => value,
            };
            if scheduler_user.is_empty()
                || !scheduler_user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                return Err(format!("invalid scheduler_user '{scheduler_user}'"));
            }
            let ssh_user = match g("ssh_user") {
                value if value.is_empty() => None,
                value => Some(value),
            };
            let host = g("host");
            if host.is_empty()
                || host.starts_with('-')
                || !host.chars().all(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '[' | ']')
                })
            {
                return Err(format!("invalid host '{host}'"));
            }
            if let Some(user) = &ssh_user {
                if user.starts_with('-')
                    || !user
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                {
                    return Err(format!("invalid ssh_user '{user}'"));
                }
            }
            out.push(Machine {
                name: g("name"),
                host,
                scheduler,
                scheduler_user,
                scheduler_command,
                ssh_user,
            });
        }
    }
    if out.is_empty() {
        return Err("no [[machine]] entries".into());
    }
    Ok(out)
}

// -------------------------------------------------------------------- data --

fn remote_command(machine: &Machine) -> String {
    match machine.scheduler {
        Scheduler::Slurm => format!(
            "{} -u {} -h -o '{SQUEUE_FMT}'",
            machine.scheduler_command, machine.scheduler_user
        ),
        Scheduler::Pbs => format!("{} -f -F json", machine.scheduler_command),
    }
}

fn query_machine(machine: &Machine) -> QueryResult {
    let remote = remote_command(machine);
    let mut command = Command::new("ssh");
    command
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "LogLevel=ERROR",
        ])
        .arg(machine.ssh_target())
        .arg(&remote)
        .stderr(Stdio::piped());
    let out = run_command_with_timeout(&mut command, Duration::from_secs(15))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().last().unwrap_or("scheduler query failed");
        return Err(truncate(last, 100));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if machine.scheduler == Scheduler::Pbs {
        return parse_pbs_jobs(&text, &machine.scheduler_user);
    }
    Ok(parse_slurm_jobs(&text))
}

fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "ssh not found in PATH".to_string())?;
    let mut stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("failed to capture stderr")?;
    let stdout_reader =
        std::thread::spawn(move || read_bounded(&mut stdout, MAX_SCHEDULER_STDOUT_BYTES));
    let stderr_reader =
        std::thread::spawn(move || read_bounded(&mut stderr, MAX_SCHEDULER_STDERR_BYTES));
    let started = Instant::now();
    let mut status = None;
    loop {
        match child.try_wait() {
            Ok(Some(exit_status)) => status = Some(exit_status),
            Ok(None) => {}
            Err(error) => {
                terminate_command(&mut child);
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
        if status.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            break;
        }
        if started.elapsed() >= timeout {
            terminate_command(&mut child);
            if status.is_none() {
                let _ = child.wait();
            }
            return Err(format!(
                "scheduler query timed out after {}s",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let (stdout, stdout_exceeded) = stdout_reader
        .join()
        .map_err(|_| "stdout reader panicked".to_string())?
        .map_err(|error| error.to_string())?;
    let (stderr, stderr_exceeded) = stderr_reader
        .join()
        .map_err(|_| "stderr reader panicked".to_string())?
        .map_err(|error| error.to_string())?;

    let status = status.ok_or("scheduler command exited without a status")?;
    if stdout_exceeded {
        return Err(format!(
            "scheduler stdout exceeded {MAX_SCHEDULER_STDOUT_BYTES} bytes"
        ));
    }
    if stderr_exceeded {
        return Err(format!(
            "scheduler stderr exceeded {MAX_SCHEDULER_STDERR_BYTES} bytes"
        ));
    }

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_bounded(reader: &mut impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 64 * 1024];
    let mut exceeded = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        exceeded |= count > remaining;
    }
    Ok((bytes, exceeded))
}

fn terminate_command(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
}

fn parse_slurm_jobs(text: &str) -> Vec<Job> {
    let mut jobs = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        if parts.len() != 8 {
            continue;
        }
        jobs.push(Job {
            id: parts[0].into(),
            name: parts[1].into(),
            state: parts[2].into(),
            elapsed: parts[3].into(),
            nodes: parts[4].into(),
            account: parts[5].into(),
            reason: parts[6].into(),
            where_: parts[7].into(),
        });
    }
    jobs
}

fn parse_pbs_jobs(text: &str, scheduler_user: &str) -> QueryResult {
    let repaired = repair_pbs_json(text)?;
    let root: serde_json::Value = serde_json::from_str(&repaired).map_err(|e| e.to_string())?;
    let jobs = root
        .get("Jobs")
        .and_then(|value| value.as_object())
        .ok_or("qstat JSON has no Jobs object")?;
    let mut parsed = Vec::new();
    for (id, value) in jobs {
        let Some(job) = value.as_object() else {
            continue;
        };
        let owner = job
            .get("Job_Owner")
            .and_then(|value| value.as_str())
            .or_else(|| job.get("euser").and_then(|value| value.as_str()))
            .or_else(|| {
                job.get("Variable_List")
                    .and_then(|value| value.as_object())
                    .and_then(|variables| variables.get("PBS_O_LOGNAME"))
                    .and_then(|value| value.as_str())
            });
        if owner.and_then(|owner| owner.split('@').next()) != Some(scheduler_user) {
            continue;
        }
        let string = |key: &str| {
            job.get(key)
                .and_then(|value| value.as_str())
                .map(sanitize_display_text)
                .unwrap_or_default()
        };
        let resources = job.get("Resource_List").and_then(|value| value.as_object());
        let nodes = resources
            .and_then(|resources| resources.get("nodect"))
            .and_then(json_scalar)
            .or_else(|| {
                resources
                    .and_then(|resources| resources.get("select"))
                    .and_then(|value| value.as_str())
                    .map(nodes_from_select)
            })
            .unwrap_or_default();
        let account = ["Account_Name", "project"]
            .iter()
            .find_map(|key| job.get(*key).and_then(json_scalar))
            .unwrap_or_default();
        let elapsed = job
            .get("resources_used")
            .and_then(|value| value.as_object())
            .and_then(|resources| resources.get("walltime"))
            .and_then(json_scalar)
            .unwrap_or_default();
        let state = normalize_pbs_state(&string("job_state"));
        parsed.push(Job {
            id: sanitize_display_text(id),
            name: string("Job_Name"),
            state,
            elapsed,
            nodes,
            account,
            reason: string("comment"),
            where_: string("exec_host"),
        });
    }
    Ok(parsed)
}

fn sanitize_display_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn repair_pbs_json(text: &str) -> Result<String, String> {
    if text.len() > MAX_PBS_JSON_BYTES {
        return Err(format!(
            "qstat JSON is too large ({} bytes; limit is {MAX_PBS_JSON_BYTES})",
            text.len()
        ));
    }

    let chars: Vec<char> = text.chars().collect();
    let mut repaired = String::with_capacity(text.len());
    let mut in_string = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            match ch {
                '"' => {
                    in_string = false;
                    repaired.push(ch);
                }
                '\\' if index + 1 < chars.len() => {
                    let escaped = chars[index + 1];
                    let valid_unicode = escaped == 'u'
                        && index + 5 < chars.len()
                        && chars[index + 2..=index + 5]
                            .iter()
                            .all(|digit| digit.is_ascii_hexdigit());
                    if matches!(escaped, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't')
                        || valid_unicode
                    {
                        repaired.push(ch);
                        repaired.push(escaped);
                        index += 2;
                        continue;
                    }
                    repaired.push_str("\\\\");
                }
                c if c.is_control() => repaired.push(' '),
                _ => repaired.push(ch),
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            in_string = true;
            repaired.push(ch);
            index += 1;
            continue;
        }
        if ch.is_control() {
            repaired.push(' ');
            index += 1;
            continue;
        }

        let token_len = [
            "-infinity",
            "+infinity",
            "infinity",
            "-inf",
            "+inf",
            "nan",
            "inf",
        ]
        .iter()
        .find(|token| {
            let token_len = token.len();
            index + token_len <= chars.len()
                && chars[index..index + token_len]
                    .iter()
                    .zip(token.chars())
                    .all(|(actual, expected)| actual.eq_ignore_ascii_case(&expected))
        })
        .map(|token| token.len());
        if let Some(len) = token_len {
            let before_ok = index == 0 || !chars[index - 1].is_ascii_alphanumeric();
            let after = index + len;
            let after_ok = after >= chars.len() || !chars[after].is_ascii_alphanumeric();
            if before_ok && after_ok {
                repaired.push_str("null");
                index += len;
                continue;
            }
        }
        repaired.push(ch);
        index += 1;
    }
    Ok(repaired)
}

fn json_scalar(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn nodes_from_select(select: &str) -> String {
    select
        .split('+')
        .map(|chunk| {
            chunk
                .split(':')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(1)
        })
        .sum::<u64>()
        .to_string()
}

fn normalize_pbs_state(state: &str) -> String {
    match state {
        "R" | "B" => "RUNNING".into(),
        "Q" | "W" => "PENDING".into(),
        "H" | "S" | "U" => "SUSPENDED".into(),
        "E" => "COMPLETING".into(),
        "C" | "F" | "X" => "COMPLETED".into(),
        "T" => "TRANSIT".into(),
        "M" => "MOVED".into(),
        other => format!("UNKNOWN({other})"),
    }
}

fn truncate(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn state_col(s: &str) -> &'static str {
    match s {
        "RUNNING" => "\x1b[32m",
        "PENDING" => "\x1b[33m",
        "COMPLETING" => "\x1b[36m",
        "SUSPENDED" => "\x1b[35m",
        "FAILED" | "CANCELLED" | "TIMEOUT" | "OUT_OF_MEMORY" => "\x1b[31m",
        _ => "\x1b[90m",
    }
}

fn pad(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    while out.chars().count() < width {
        out.push(' ');
    }
    out
}

fn pad_left(s: &str, width: usize) -> String {
    let mut out = String::new();
    while out.chars().count() + s.chars().count() < width {
        out.push(' ');
    }
    out.push_str(s);
    out
}

// ------------------------------------------------------------------ render --

fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(100)
}

fn render(
    machines: &[Machine],
    results: &HashMap<String, QueryResult>,
    updated: SystemTime,
) -> String {
    let w = terminal_width().max(20);
    let mut out = String::from("\x1b[2J\x1b[H");
    let title = " Scheduler Jobs ";
    let pad_l = (w.saturating_sub(title.len())) / 2;
    out.push_str(&format!(
        "\x1b[1;7m{}{}\x1b[0m\r\n",
        " ".repeat(pad_l),
        title
    ));
    let stamp = local_timestamp(&updated);
    out.push_str(&format!(
        "updated {} · {}s poll · \x1b[1mr\x1b[0m refresh · \x1b[1mq\x1b[0m quit\r\n",
        stamp, REFRESH_SECS
    ));
    for m in machines {
        out.push_str("\r\n");
        out.push_str(&format!(
            "\x1b[1m▸ {}\x1b[0m  ({} on {})\r\n",
            m.name,
            m.scheduler_user,
            m.ssh_target()
        ));
        let jobs = match results.get(&m.name) {
            Some(Ok(j)) => j,
            Some(Err(e)) => {
                out.push_str(&format!("  \x1b[31m✗ {}\x1b[0m\r\n", e));
                continue;
            }
            None => continue,
        };
        if jobs.is_empty() {
            out.push_str("  \x1b[90m(no jobs in queue)\x1b[0m\r\n");
            continue;
        }
        let mut counts: Vec<(String, usize)> = Vec::new();
        for j in jobs {
            if let Some(c) = counts.iter_mut().find(|(s, _)| *s == j.state) {
                c.1 += 1;
            } else {
                counts.push((j.state.clone(), 1));
            }
        }
        counts.sort();
        let summary: Vec<String> = counts
            .iter()
            .map(|(s, n)| format!("{}{}:{}\x1b[0m", state_col(s), s, n))
            .collect();
        out.push_str(&format!("  {}\r\n", summary.join("  ")));
        render_jobs(&mut out, jobs, w);
    }
    out.push_str("\r\n");
    out
}

fn render_jobs(out: &mut String, jobs: &[Job], w: usize) {
    if w >= 96 {
        out.push_str(&format!(
            "\x1b[90m  {} {} {} {} {}  {}  REASON/NODELIST\x1b[0m\r\n",
            pad("JOBID", 16),
            pad("NAME", 20),
            pad("ACCOUNT", 12),
            pad("STATE", 11),
            pad_left("ELAPSED", 10),
            pad_left("NODES", 5)
        ));
        for j in jobs {
            let tail = if j.state == "PENDING" && !j.reason.is_empty() {
                &j.reason
            } else {
                &j.where_
            };
            out.push_str(&format!(
                "  {} {} {} {}{}\x1b[0m {}  {}  {}\r\n",
                pad(&truncate(&j.id, 16), 16),
                pad(&truncate(&j.name, 20), 20),
                pad(&truncate(&j.account, 12), 12),
                state_col(&j.state),
                pad(&truncate(&j.state, 11), 11),
                pad_left(&j.elapsed, 10),
                pad_left(&j.nodes, 5),
                truncate(tail, 60)
            ));
        }
    } else if w >= 64 {
        out.push_str(&format!(
            "\x1b[90m  {} {} {} {} {}  {}\x1b[0m\r\n",
            pad("JOBID", 14),
            pad("NAME", 14),
            pad("ACCOUNT", 12),
            pad("STATE", 8),
            pad_left("ELAPSED", 8),
            pad_left("NODES", 4)
        ));
        for j in jobs {
            out.push_str(&format!(
                "  {} {} {} {}{}\x1b[0m {}  {}\r\n",
                pad(&truncate(&j.id, 14), 14),
                pad(&truncate(&j.name, 14), 14),
                pad(&truncate(&j.account, 12), 12),
                state_col(&j.state),
                pad(&truncate(&j.state, 8), 8),
                pad_left(&j.elapsed, 8),
                pad_left(&j.nodes, 4)
            ));
        }
    } else {
        out.push_str(&format!(
            "\x1b[90m  {} {} {}  TAIL\x1b[0m\r\n",
            pad("JOBID", 12),
            pad("STATE", 7),
            pad_left("ELAPSED", 8)
        ));
        let room = w.saturating_sub(2 + 12 + 1 + 7 + 1 + 8 + 2);
        for j in jobs {
            let tail = if j.state == "PENDING" && !j.reason.is_empty() {
                &j.reason
            } else {
                &j.where_
            };
            out.push_str(&format!(
                "  {} {}{}\x1b[0m {}  {}\r\n",
                pad(&truncate(&j.id, 12), 12),
                state_col(&j.state),
                pad(&truncate(&j.state, 7), 7),
                pad_left(&j.elapsed, 8),
                truncate(tail, room)
            ));
        }
    }
}

fn local_timestamp(t: &SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = (*t).into();
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

// ----------------------------------------------------------- token reporting --

fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into())
}

fn run_capture(args: &[String]) -> Option<String> {
    let out = Command::new(&args[0]).args(&args[1..]).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

fn workspace_ids(
    machine: Option<&str>,
    cache: &mut HashMap<String, (Instant, Vec<String>)>,
) -> Vec<String> {
    let key = machine.unwrap_or("local").to_string();
    if let Some((ts, ids)) = cache.get(&key) {
        if ts.elapsed() < Duration::from_secs(300) && !ids.is_empty() {
            return ids.clone();
        }
    }
    let herdr = herdr_bin();
    let mut args = vec![herdr];
    if let Some(m) = machine {
        args.push("--machine".into());
        args.push(m.to_string());
    }
    args.push("workspace".into());
    args.push("list".into());
    let mut ids = Vec::new();
    if let Some(stdout) = run_capture(&args) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(list) = v.pointer("/result/workspaces").and_then(|w| w.as_array()) {
                for w in list {
                    if let Some(id) = w.get("workspace_id").and_then(|i| i.as_str()) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    }
    if !ids.is_empty() {
        cache.insert(key, (Instant::now(), ids.clone()));
    }
    ids
}

fn summarize(jobs: &QueryResult, with_name: Option<&str>) -> String {
    let prefix = match with_name {
        Some(n) => format!("{n}: "),
        None => String::new(),
    };
    match jobs {
        Err(e) => format!("{prefix}✗ {e}"),
        Ok(js) => {
            let run = js.iter().filter(|j| j.state == "RUNNING").count();
            let pend = js.iter().filter(|j| j.state == "PENDING").count();
            let mut bits = vec![format!("{run} run")];
            if pend > 0 {
                bits.push(format!("{pend} pend"));
            }
            if run + pend < js.len() {
                bits.push(format!("{} other", js.len() - run - pend));
            }
            format!("{prefix}{}", bits.join(", "))
        }
    }
}

fn sidebar_tokens(value: &str) -> [String; 2] {
    [format!("scheduler={value}"), format!("slurm={value}")]
}

fn report_sidebar_token(machines: &[Machine], results: &HashMap<String, QueryResult>) {
    let herdr = herdr_bin();
    let mut cache: HashMap<String, (Instant, Vec<String>)> = HashMap::new();
    let local_ids = {
        let ids = workspace_ids(None, &mut cache);
        if ids.is_empty() {
            env::var("HERDR_WORKSPACE_ID")
                .map(|v| vec![v])
                .unwrap_or_default()
        } else {
            ids
        }
    };
    for m in machines {
        let machine_ids = workspace_ids(Some(&m.name), &mut cache);
        let (targets, prefix): (Vec<(String, String)>, Vec<String>) = if !machine_ids.is_empty() {
            (
                machine_ids
                    .iter()
                    .map(|w| (w.clone(), summarize(&results[&m.name], None)))
                    .collect(),
                vec![herdr.clone(), "--machine".into(), m.name.clone()],
            )
        } else {
            (
                local_ids
                    .iter()
                    .map(|w| (w.clone(), summarize(&results[&m.name], Some(&m.name))))
                    .collect(),
                vec![herdr.clone()],
            )
        };
        for (wid, value) in targets {
            let mut args = prefix.clone();
            args.extend([
                "workspace".to_string(),
                "report-metadata".to_string(),
                wid,
                "--source".to_string(),
                "ivan.herdr-slurm".to_string(),
            ]);
            for token in sidebar_tokens(&value) {
                args.extend(["--token".to_string(), token]);
            }
            args.extend([
                "--ttl-ms".to_string(),
                ((REFRESH_SECS + 70) * 1000).to_string(),
            ]);
            let _ = Command::new(&args[0]).args(&args[1..]).output();
        }
    }
}

// ------------------------------------------------------------------ actions --

fn cmd_refresh() {
    let machines = match load_machines() {
        Some(m) => m,
        None => return,
    };
    for m in &machines {
        println!("▸ {} ({} on {})", m.name, m.scheduler_user, m.ssh_target());
        match query_machine(m) {
            Err(e) => println!("  ✗ {e}"),
            Ok(jobs) if jobs.is_empty() => println!("  (no jobs in queue)"),
            Ok(jobs) => {
                for j in &jobs {
                    println!(
                        "  {} {} {} {} {}  {}",
                        pad(&truncate(&j.id, 16), 16),
                        pad(&truncate(&j.name, 20), 20),
                        pad(&truncate(&j.account, 12), 12),
                        pad(&truncate(&j.state, 11), 11),
                        pad_left(&j.elapsed, 10),
                        j.where_
                    );
                }
            }
        }
    }
}

fn cmd_open() {
    let herdr = herdr_bin();
    let entrypoint = if cfg!(windows) {
        "jobs-windows"
    } else {
        "jobs"
    };
    let plugin = env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "ivan.herdr-slurm".into());
    let out = Command::new(&herdr)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            &plugin,
            "--entrypoint",
            entrypoint,
        ])
        .output()
        .expect("failed to run herdr");
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        std::process::exit(out.status.code().unwrap_or(1));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let pane_id = serde_json::from_str::<serde_json::Value>(&stdout)
        .ok()
        .and_then(|v| {
            v.pointer("/result/plugin_pane/pane/pane_id")
                .and_then(|p| p.as_str())
                .map(str::to_string)
        });
    let pane_id = match pane_id {
        Some(p) => p,
        None => return, // already open
    };
    // The split layout can take a moment to settle after `pane open`.
    let mut last_err = String::new();
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(200));
        match resize_sidebar(&herdr, &pane_id) {
            Ok(()) => return,
            Err(e) => last_err = e,
        }
    }
    eprintln!("herdr-slurm: could not resize pane to sidebar width: {last_err}");
}

fn resize_sidebar(herdr: &str, pane_id: &str) -> Result<(), String> {
    let stdout = run_capture(&[herdr.to_string(), "pane".into(), "edges".into()])
        .ok_or("pane edges failed")?;
    let v: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| e.to_string())?;
    let layout = v.pointer("/result/edges/layout").ok_or("no layout")?;
    let area_w = layout
        .pointer("/area/width")
        .and_then(|w| w.as_u64())
        .ok_or("no area")? as i64;
    let (x, width) = layout
        .pointer("/panes")
        .and_then(|p| p.as_array())
        .and_then(|panes| {
            panes
                .iter()
                .find(|p| p.get("pane_id").and_then(|i| i.as_str()) == Some(pane_id))
        })
        .and_then(|p| {
            Some((
                p.pointer("/rect/x").and_then(|v| v.as_i64())?,
                p.pointer("/rect/width").and_then(|v| v.as_i64())?,
            ))
        })
        .ok_or("pane not found in layout")?;
    if (width - TARGET_COLS as i64).abs() < 3 {
        return Ok(());
    }
    let target_x = (area_w - TARGET_COLS as i64).max(0);
    let delta = target_x - x;
    if delta.abs() < 2 {
        return Ok(());
    }
    let direction = if delta > 0 { "right" } else { "left" };
    let amount = (delta.abs() as f64 / area_w as f64).min(0.45);
    let out = Command::new(herdr)
        .args([
            "pane",
            "resize",
            "--pane",
            pane_id,
            "--direction",
            direction,
            "--amount",
        ])
        .arg(format!("{amount:.4}"))
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err("resize failed".into())
    }
}

// -------------------------------------------------------------------- main --

fn cmd_sidebar() {
    use crossterm::event::{Event, KeyCode, KeyEventKind};
    let _ = crossterm::terminal::enable_raw_mode();
    let mut stdout = std::io::stdout();
    let _ = crossterm::execute!(stdout, crossterm::terminal::LeaveAlternateScreen);
    loop {
        let machines = match load_machines() {
            Some(m) => m,
            None => {
                std::thread::sleep(Duration::from_secs(REFRESH_SECS));
                continue;
            }
        };
        let mut results: HashMap<String, QueryResult> = HashMap::new();
        for m in &machines {
            results.insert(m.name.clone(), query_machine(m));
        }
        print!("{}", render(&machines, &results, SystemTime::now()));
        use std::io::Write;
        let _ = std::io::stdout().flush();
        report_sidebar_token(&machines, &results);
        let deadline = Instant::now() + Duration::from_secs(REFRESH_SECS);
        let mut quit = false;
        while Instant::now() < deadline {
            if crossterm::event::poll(Duration::from_millis(200)).unwrap_or(false) {
                if let Ok(Event::Key(k)) = crossterm::event::read() {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => quit = true,
                        KeyCode::Char('r') | KeyCode::Char('R') => break,
                        _ => {}
                    }
                }
            }
            if quit {
                break;
            }
        }
        if quit {
            break;
        }
    }
    let _ = crossterm::terminal::disable_raw_mode();
}

#[allow(dead_code)]
fn cmd_keytest() {
    use crossterm::event::{KeyCode, KeyEventKind};
    match crossterm::terminal::enable_raw_mode() {
        Ok(()) => println!("raw mode: OK"),
        Err(e) => println!("raw mode FAILED: {e}"),
    }
    println!("waiting for keys... (10s)");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if crossterm::event::poll(Duration::from_millis(200)).unwrap_or(false) {
            match crossterm::event::read() {
                Ok(ev) => println!("event: {ev:?}"),
                Err(e) => println!("read err: {e}"),
            }
        }
    }
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = KeyCode::Null;
    let _ = KeyEventKind::Press;
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("open") => cmd_open(),
        Some("refresh") => cmd_refresh(),
        Some("keytest") => cmd_keytest(),
        _ => cmd_sidebar(),
    }
}

// Keep Path import used even if config paths change.
#[allow(dead_code)]
fn _unused(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_machine_config_defaults_to_slurm_and_ssh_config_identity() {
        let machines = parse_machines(
            r#"
[[machine]]
name = "Polaris"
host = "polaris.alcf.anl.gov"
user = "sam"
"#,
        )
        .unwrap();

        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].scheduler, Scheduler::Slurm);
        assert_eq!(machines[0].scheduler_user, "sam");
        assert_eq!(machines[0].ssh_target(), "polaris.alcf.anl.gov");
    }

    #[test]
    fn pbs_machine_config_separates_scheduler_and_ssh_users() {
        let machines = parse_machines(
            r#"
[[machine]]
name = "Aurora"
host = "aurora"
scheduler = "pbs"
scheduler_user = "queue-user"
ssh_user = "login-user"
"#,
        )
        .unwrap();

        assert_eq!(machines[0].scheduler, Scheduler::Pbs);
        assert_eq!(machines[0].scheduler_user, "queue-user");
        assert_eq!(machines[0].ssh_target(), "login-user@aurora");
    }

    #[test]
    fn pbs_machine_config_uses_configured_scheduler_executable() {
        let machines = parse_machines(
            r#"
[[machine]]
name = "Polaris"
host = "polaris"
scheduler = "pbs"
scheduler_user = "sam"
scheduler_command = "/opt/pbs/bin/qstat"
"#,
        )
        .unwrap();

        assert_eq!(
            remote_command(&machines[0]),
            "/opt/pbs/bin/qstat -f -F json"
        );
    }

    #[test]
    fn config_rejects_unknown_scheduler() {
        let error = parse_machines(
            r#"
[[machine]]
name = "Cluster"
host = "cluster"
user = "sam"
scheduler = "lsf"
"#,
        )
        .unwrap_err();

        assert!(error.contains("unsupported scheduler 'lsf'"));
    }

    #[test]
    fn config_rejects_unsafe_scheduler_user() {
        let error = parse_machines(
            r#"
[[machine]]
name = "Cluster"
host = "cluster"
scheduler_user = "sam; id"
"#,
        )
        .unwrap_err();

        assert!(error.contains("invalid scheduler_user"));
    }

    #[test]
    fn config_rejects_ssh_option_and_command_injection() {
        for (field, value) in [
            ("host", "-oProxyCommand=id"),
            ("host", "cluster;id"),
            ("host", "user@cluster"),
            ("ssh_user", "-oProxyCommand=id"),
            ("ssh_user", "sam@evil"),
            ("ssh_user", "sam;id"),
        ] {
            let host = if field == "host" { value } else { "cluster" };
            let extra = if field == "ssh_user" {
                format!("ssh_user = \"{value}\"")
            } else {
                String::new()
            };
            let raw = format!(
                r#"
[[machine]]
name = "Cluster"
host = "{host}"
scheduler_user = "sam"
{extra}
"#
            );

            let error = parse_machines(&raw).unwrap_err();
            assert!(
                error.contains(&format!("invalid {field}")),
                "{field}={value}: {error}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_terminates_descendants_holding_capture_pipes() {
        let pid_path = env::temp_dir().join(format!(
            "herdr-slurm-descendant-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let script = format!("sleep 30 & echo $! > '{}'", pid_path.display());
        let mut command = Command::new("sh");
        command.args(["-c", &script]);
        let started = Instant::now();
        let error = run_command_with_timeout(&mut command, Duration::from_millis(100)).unwrap_err();
        assert!(error.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));

        let descendant_pid: i32 = std::fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::fs::remove_file(pid_path).unwrap();
        let reap_deadline = Instant::now() + Duration::from_secs(1);
        while unsafe { libc::kill(descendant_pid, 0) } == 0 && Instant::now() < reap_deadline {
            std::thread::yield_now();
        }
        assert_eq!(unsafe { libc::kill(descendant_pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_capture_rejects_stdout_over_limit() {
        let mut command = Command::new("sh");
        command.args(["-c", "dd if=/dev/zero bs=1048576 count=9 2>/dev/null"]);

        let error = run_command_with_timeout(&mut command, Duration::from_secs(3)).unwrap_err();

        assert!(error.contains("stdout exceeded"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn command_capture_rejects_stderr_over_limit() {
        let mut command = Command::new("sh");
        command.args(["-c", "dd if=/dev/zero bs=1048576 count=2 1>&2 2>/dev/null"]);

        let error = run_command_with_timeout(&mut command, Duration::from_secs(3)).unwrap_err();

        assert!(error.contains("stderr exceeded"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn command_capture_drains_large_stdout_and_stderr_without_timing_out() {
        const OUTPUT_BYTES: usize = 1024 * 1024;
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "dd if=/dev/zero bs=1048576 count=1 2>/dev/null; dd if=/dev/zero bs=1048576 count=1 1>&2 2>/dev/null",
            ])
            .stderr(Stdio::piped());

        let output = run_command_with_timeout(&mut command, Duration::from_secs(3)).unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout.len(), OUTPUT_BYTES);
        assert_eq!(output.stderr.len(), OUTPUT_BYTES);
    }

    #[test]
    fn config_rejects_scheduler_command_shell_syntax() {
        for command in ["qstat;id", "qstat --version", "$(id)", "-qstat"] {
            let raw = format!(
                r#"
[[machine]]
name = "Cluster"
host = "cluster"
scheduler = "pbs"
scheduler_user = "sam"
scheduler_command = "{command}"
"#
            );

            let error = parse_machines(&raw).unwrap_err();
            assert!(error.contains("invalid scheduler_command"), "{command}");
        }
    }

    #[test]
    fn parses_pbs_json_and_filters_jobs_by_owner() {
        let jobs = parse_pbs_jobs(
            r#"{
  "timestamp": 1720000000,
  "Jobs": {
    "1234.aurora-pbs-0001.host": {
      "Job_Name": "train",
      "Job_Owner": "sam@uan01",
      "job_state": "R",
      "Account_Name": "project-a",
      "resources_used": {"walltime": "01:02:03"},
      "Resource_List": {"nodect": 2, "select": "2:ncpus=104"},
      "exec_host": "x1001/0*104+x1002/0*104"
    },
    "1235.aurora-pbs-0001.host": {
      "Job_Name": "waiting",
      "Job_Owner": "sam@uan01",
      "job_state": "Q",
      "project": "project-b",
      "Resource_List": {"select": "2:ncpus=104+1:ncpus=52"},
      "comment": "Not Running: Insufficient amount of resource"
    },
    "9999.aurora-pbs-0001.host": {
      "Job_Name": "someone-else",
      "Job_Owner": "other@uan01",
      "job_state": "R"
    }
  }
}"#,
            "sam",
        )
        .unwrap();

        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, "1234.aurora-pbs-0001.host");
        assert_eq!(jobs[0].state, "RUNNING");
        assert_eq!(jobs[0].elapsed, "01:02:03");
        assert_eq!(jobs[0].nodes, "2");
        assert_eq!(jobs[0].account, "project-a");
        assert_eq!(jobs[0].where_, "x1001/0*104+x1002/0*104");
        assert_eq!(jobs[1].state, "PENDING");
        assert_eq!(jobs[1].nodes, "3");
        assert_eq!(jobs[1].account, "project-b");
        assert_eq!(
            jobs[1].reason,
            "Not Running: Insufficient amount of resource"
        );
    }

    #[test]
    fn pbs_parser_falls_back_to_euser_and_pbs_logname() {
        let input = r#"{
          "Jobs": {
            "euser.server": {"euser":"sam", "Job_Name":"one", "job_state":"Q"},
            "logname.server": {"Variable_List":{"PBS_O_LOGNAME":"sam"}, "Job_Name":"two", "job_state":"Q"},
            "other.server": {"euser":"other", "Job_Name":"skip", "job_state":"Q"}
          }
        }"#;

        let jobs = parse_pbs_jobs(input, "sam").unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, "euser.server");
        assert_eq!(jobs[1].id, "logname.server");
    }

    #[test]
    fn normalizes_all_common_pbs_states() {
        let cases = [
            ("R", "RUNNING"),
            ("Q", "PENDING"),
            ("W", "PENDING"),
            ("H", "SUSPENDED"),
            ("S", "SUSPENDED"),
            ("U", "SUSPENDED"),
            ("E", "COMPLETING"),
            ("C", "COMPLETED"),
            ("F", "COMPLETED"),
            ("X", "COMPLETED"),
            ("T", "TRANSIT"),
            ("M", "MOVED"),
            ("B", "RUNNING"),
            ("?", "UNKNOWN(?)"),
        ];

        for (pbs, expected) in cases {
            assert_eq!(normalize_pbs_state(pbs), expected);
        }
    }

    #[test]
    fn pbs_parser_tolerates_malformed_jobs_but_rejects_malformed_output() {
        let mixed = r#"{
          "Jobs": {
            "good.server": {"Job_Owner":"sam@host", "Job_Name":"ok", "job_state":"R"},
            "missing-owner.server": {"Job_Name":"skip", "job_state":"Q"},
            "not-an-object.server": "skip"
          }
        }"#;
        let jobs = parse_pbs_jobs(mixed, "sam").unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "good.server");
        assert!(parse_pbs_jobs("not json", "sam").is_err());
        assert!(parse_pbs_jobs(r#"{"Jobs": []}"#, "sam").is_err());
    }

    #[test]
    fn pbs_parser_repairs_nonstandard_scalars_escapes_and_control_bytes() {
        let input = concat!(
            r#"{"Jobs":{"bad.server":{"Job_Owner":"other@host","score":nan,"limit":-inf,"path":"bad\q","unicode":"bad\uZZZZ"},"#,
            "\"good.server\":{\"Job_Owner\":\"sam@host\",\"Job_Name\":\"ok\\u0001 \\\"quoted\\\" name\",\"job_state\":\"R\"}}}"
        );

        let jobs = parse_pbs_jobs(input, "sam").unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "good.server");
        assert_eq!(jobs[0].name, "ok  \"quoted\" name");
    }

    #[test]
    fn pbs_parser_bounds_repair_input() {
        let oversized = " ".repeat(8 * 1024 * 1024 + 1);
        let error = match parse_pbs_jobs(&oversized, "sam") {
            Ok(_) => panic!("oversized input was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("too large"));
    }

    #[test]
    fn scheduler_commands_use_machine_scheduler_and_user() {
        let slurm = Machine {
            name: "Polaris".into(),
            host: "polaris".into(),
            scheduler: Scheduler::Slurm,
            scheduler_user: "sam".into(),
            scheduler_command: "squeue".into(),
            ssh_user: None,
        };
        let pbs = Machine {
            scheduler: Scheduler::Pbs,
            scheduler_command: "qstat".into(),
            ..slurm.clone()
        };

        assert_eq!(
            remote_command(&slurm),
            format!("squeue -u sam -h -o '{SQUEUE_FMT}'")
        );
        assert_eq!(remote_command(&pbs), "qstat -f -F json");
    }

    #[test]
    fn sidebar_metadata_has_neutral_and_legacy_tokens() {
        assert_eq!(
            sidebar_tokens("2 run, 1 pend"),
            ["scheduler=2 run, 1 pend", "slurm=2 run, 1 pend"]
        );
    }

    #[test]
    fn slurm_parser_skips_malformed_lines_in_mixed_output() {
        let jobs = parse_slurm_jobs(
            "123|train|RUNNING|00:02|2|proj||node[01-02]\nmalformed\n124|wait|PENDING|0:00|1|proj|Priority|\n",
        );

        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, "123");
        assert_eq!(jobs[1].reason, "Priority");
    }
}
