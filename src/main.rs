use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

const REFRESH_SECS: u64 = 20;
const TARGET_COLS: usize = 50; // sidebar width used by the `open` subcommand
const SQUEUE_FMT: &str = "%.18i|%j|%T|%M|%D|%a|%R|%N";

#[derive(Clone)]
struct Machine {
    name: String,
    host: String,
    user: String,
    session: String, // herdr session on the cluster to report to; "" = default
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
    if let Ok(d) = env::var("HERDR_PLUGIN_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    // Ask herdr for the canonical plugin config dir (works when run from a
    // pane, an action, or manually with herdr on PATH).
    let args = vec![herdr_bin(), "plugin".into(), "config-dir".into(), "ivan.herdr-slurm".into()];
    if let Some(out) = run_capture(&args) {
        let t = out.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    // Platform fallback: herdr's default plugin config layout.
    let base = if cfg!(windows) {
        env::var("APPDATA").map(PathBuf::from).unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        env::var("HOME").map(|h| PathBuf::from(h).join("Library/Application Support")).unwrap_or_default()
    } else {
        env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|_| env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default()
    };
    base.join("herdr").join("plugins").join("config").join("ivan.herdr-slurm")
}

const TEMPLATE: &str = r#"# herdr-slurm machines. One [[machine]] block per cluster.
# `user` is your cluster login (often differs from your local username).
# `name` is display-only: the pane heading and the sidebar fallback prefix.
# The sidebar token is reported to the herdr server ON the cluster itself
# (over ssh to `host`), so no herdr machine label has to match.
# `session` (optional) is the herdr session on the cluster whose workspaces
# get the sidebar token; omit it for the default session.

[[machine]]
name = "MyCluster"
host = "login.example.org"
user = "your-cluster-login"
# session = "agents"
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
    let value: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            println!("herdr-slurm: invalid TOML in {}: {}", path.display(), e);
            return None;
        }
    };
    let mut out = Vec::new();
    if let Some(list) = value.get("machine").and_then(|v| v.as_array()) {
        for m in list {
            let g = |k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            out.push(Machine { name: g("name"), host: g("host"), user: g("user"), session: g("session") });
        }
    }
    if out.is_empty() {
        println!("herdr-slurm: no [[machine]] entries in {}", path.display());
        return None;
    }
    Some(out)
}

// -------------------------------------------------------------------- data --

fn ssh_squeue(host: &str, user: &str) -> QueryResult {
    let remote = format!("squeue -u {user} -h -o '{SQUEUE_FMT}'");
    let out = new_command("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8", "-o", "LogLevel=ERROR"])
        .arg(host)
        .arg(&remote)
        .stderr(Stdio::piped())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(_) => return Err("ssh not found in PATH".into()),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().last().unwrap_or("squeue failed");
        return Err(truncate(last, 100));
    }
    let text = String::from_utf8_lossy(&out.stdout);
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
    Ok(jobs)
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
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100)
}

fn render(machines: &[Machine], results: &HashMap<String, QueryResult>, updated: SystemTime) -> String {
    let w = terminal_width().max(20);
    let mut out = String::from("\x1b[2J\x1b[H");
    let title = " SLURM Jobs ";
    let pad_l = (w.saturating_sub(title.len())) / 2;
    out.push_str(&format!("\x1b[1;7m{}{}\x1b[0m\r\n", " ".repeat(pad_l), title));
    let stamp = local_timestamp(&updated);
    out.push_str(&format!(
        "updated {} · {}s poll · \x1b[1mr\x1b[0m refresh · \x1b[1mq\x1b[0m quit\r\n",
        stamp, REFRESH_SECS
    ));
    for m in machines {
        out.push_str("\r\n");
        out.push_str(&format!("\x1b[1m▸ {}\x1b[0m  ({}@{})\r\n", m.name, m.user, m.host));
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
            pad("JOBID", 16), pad("NAME", 20), pad("ACCOUNT", 12),
            pad("STATE", 11), pad_left("ELAPSED", 10), pad_left("NODES", 5)
        ));
        for j in jobs {
            let tail = if j.state == "PENDING" && !j.reason.is_empty() { &j.reason } else { &j.where_ };
            out.push_str(&format!(
                "  {} {} {} {}{}\x1b[0m {}  {}  {}\r\n",
                pad(&truncate(&j.id, 16), 16), pad(&truncate(&j.name, 20), 20),
                pad(&truncate(&j.account, 12), 12), state_col(&j.state),
                pad(&truncate(&j.state, 11), 11), pad_left(&j.elapsed, 10),
                pad_left(&j.nodes, 5), truncate(tail, 60)
            ));
        }
    } else if w >= 64 {
        out.push_str(&format!(
            "\x1b[90m  {} {} {} {} {}  {}\x1b[0m\r\n",
            pad("JOBID", 14), pad("NAME", 14), pad("ACCOUNT", 12),
            pad("STATE", 8), pad_left("ELAPSED", 8), pad_left("NODES", 4)
        ));
        for j in jobs {
            out.push_str(&format!(
                "  {} {} {} {}{}\x1b[0m {}  {}\r\n",
                pad(&truncate(&j.id, 14), 14), pad(&truncate(&j.name, 14), 14),
                pad(&truncate(&j.account, 12), 12), state_col(&j.state),
                pad(&truncate(&j.state, 8), 8), pad_left(&j.elapsed, 8),
                pad_left(&j.nodes, 4)
            ));
        }
    } else {
        out.push_str(&format!(
            "\x1b[90m  {} {} {}  TAIL\x1b[0m\r\n",
            pad("JOBID", 12), pad("STATE", 7), pad_left("ELAPSED", 8)
        ));
        let room = w.saturating_sub(2 + 12 + 1 + 7 + 1 + 8 + 2);
        for j in jobs {
            let tail = if j.state == "PENDING" && !j.reason.is_empty() { &j.reason } else { &j.where_ };
            out.push_str(&format!(
                "  {} {}{}\x1b[0m {}  {}\r\n",
                pad(&truncate(&j.id, 12), 12), state_col(&j.state),
                pad(&truncate(&j.state, 7), 7), pad_left(&j.elapsed, 8),
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

// Every child this program spawns is capture-only (stdout/stderr piped or
// null). On Windows, console-subsystem children (ssh, herdr) launched from
// the console-less daemon would each allocate a fresh, visible console
// window; CREATE_NO_WINDOW prevents that. It is harmless when a console
// already exists (pane, action), so apply it unconditionally.
fn new_command<S: AsRef<std::ffi::OsStr>>(prog: S) -> Command {
    let mut cmd = Command::new(prog);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn run_capture(args: &[String]) -> Option<String> {
    let out = new_command(&args[0]).args(&args[1..]).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

// argv for running a command on the cluster over ssh. The local CLI's
// remote path (`herdr --machine <label> ...`) is NOT used on purpose: on
// Windows it allocates a visible console window whenever the caller has no
// console (the detached daemon), flashing a window every poll cycle.
fn ssh_argv(host: &str, remote: &str) -> Vec<String> {
    let mut v: Vec<String> = [
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=8",
        "-o",
        "LogLevel=ERROR",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.push(host.to_string());
    v.push(remote.to_string());
    v
}

// A herdr CLI invocation for the herdr server on the cluster, pinned to the
// machine's configured session (tokens must land in the session the user's
// sidebar actually shows).
fn remote_herdr(m: &Machine, sub: &str) -> String {
    if m.session.is_empty() {
        format!("herdr {sub}")
    } else {
        format!("herdr --session \"{}\" {sub}", m.session)
    }
}

fn workspace_ids(machine: Option<&Machine>, cache: &mut HashMap<String, (Instant, Vec<String>)>) -> Vec<String> {
    let key = machine.map(|m| m.name.as_str()).unwrap_or("local").to_string();
    if let Some((ts, ids)) = cache.get(&key) {
        if ts.elapsed() < Duration::from_secs(300) && !ids.is_empty() {
            return ids.clone();
        }
    }
    let args = match machine {
        Some(m) => ssh_argv(&m.host, &remote_herdr(m, "workspace list")),
        None => vec![herdr_bin(), "workspace".into(), "list".into()],
    };
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

fn report_sidebar_token(machines: &[Machine], results: &HashMap<String, QueryResult>) {
    let herdr = herdr_bin();
    let mut cache: HashMap<String, (Instant, Vec<String>)> = HashMap::new();
    let local_ids = {
        let ids = workspace_ids(None, &mut cache);
        if ids.is_empty() {
            env::var("HERDR_WORKSPACE_ID").map(|v| vec![v]).unwrap_or_default()
        } else {
            ids
        }
    };
    let ttl_ms = (REFRESH_SECS + 70) * 1000;
    for m in machines {
        let machine_ids = workspace_ids(Some(m), &mut cache);
        if !machine_ids.is_empty() {
            // The cluster runs a reachable herdr server: report there over ssh.
            let value = summarize(&results[&m.name], None);
            for wid in &machine_ids {
                let remote = remote_herdr(
                    m,
                    &format!(
                        "workspace report-metadata {wid} --source ivan.herdr-slurm \
                         --token \"slurm={value}\" --ttl-ms {ttl_ms}"
                    ),
                );
                let args = ssh_argv(&m.host, &remote);
                let _ = new_command(&args[0]).args(&args[1..]).output();
            }
        } else {
            // No reachable herdr on the cluster: tag local workspaces instead.
            let value = summarize(&results[&m.name], Some(&m.name));
            for wid in &local_ids {
                let args = vec![
                    herdr.clone(),
                    "workspace".into(),
                    "report-metadata".into(),
                    wid.clone(),
                    "--source".into(),
                    "ivan.herdr-slurm".into(),
                    "--token".into(),
                    format!("slurm={value}"),
                    "--ttl-ms".into(),
                    ttl_ms.to_string(),
                ];
                let _ = new_command(&args[0]).args(&args[1..]).output();
            }
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
        println!("▸ {} ({}@{})", m.name, m.user, m.host);
        match ssh_squeue(&m.host, &m.user) {
            Err(e) => println!("  ✗ {e}"),
            Ok(jobs) if jobs.is_empty() => println!("  (no jobs in queue)"),
            Ok(jobs) => {
                for j in &jobs {
                    println!(
                        "  {} {} {} {} {}  {}",
                        pad(&truncate(&j.id, 16), 16), pad(&truncate(&j.name, 20), 20),
                        pad(&truncate(&j.account, 12), 12), pad(&truncate(&j.state, 11), 11),
                        pad_left(&j.elapsed, 10), j.where_
                    );
                }
            }
        }
    }
}

fn cmd_open() {
    let herdr = herdr_bin();
    let entrypoint = if cfg!(windows) { "jobs-windows" } else { "jobs" };
    let plugin = env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "ivan.herdr-slurm".into());
    let out = new_command(&herdr)
        .args(["plugin", "pane", "open", "--plugin", &plugin, "--entrypoint", entrypoint])
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
    let area_w = layout.pointer("/area/width").and_then(|w| w.as_u64()).ok_or("no area")? as i64;
    let (x, width) = layout
        .pointer("/panes")
        .and_then(|p| p.as_array())
        .and_then(|panes| {
            panes.iter().find(|p| p.get("pane_id").and_then(|i| i.as_str()) == Some(pane_id))
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
    let out = new_command(herdr)
        .args(["pane", "resize", "--pane", pane_id, "--direction", direction, "--amount"])
        .arg(format!("{amount:.4}"))
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err("resize failed".into()) }
}

// --------------------------------------------------------------- daemon --

// The Machines sidebar `$slurm` token must outlive the jobs pane, so polling
// and token reporting happen in a detached background process. A heartbeat
// file in the plugin state dir is the liveness signal; deleting it asks the
// daemon to stop.

const DAEMON_ALIVE_SECS: u64 = 40; // heartbeat younger than this = daemon running
const HERDR_DEAD_CYCLE_LIMIT: u32 = 90; // ~30 min at 20 s polling

fn state_dir() -> PathBuf {
    if let Ok(d) = env::var("HERDR_PLUGIN_STATE_DIR") {
        return PathBuf::from(d);
    }
    config_dir()
}

fn heartbeat_path() -> PathBuf {
    state_dir().join("daemon.heartbeat")
}

fn heartbeat_age() -> Option<Duration> {
    std::fs::metadata(heartbeat_path())
        .and_then(|md| md.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
}

fn cmd_start() {
    if let Some(age) = heartbeat_age() {
        if age < Duration::from_secs(DAEMON_ALIVE_SECS) {
            return; // daemon already running
        }
    }
    let exe = match env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("herdr-slurm: cannot locate own executable: {e}");
            return;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        // Detach from the caller's console so closing a herdr pane does not
        // take the daemon down with it.
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    if let Err(e) = cmd.spawn() {
        eprintln!("herdr-slurm: failed to spawn daemon: {e}");
    }
}

fn cmd_stop() {
    let _ = std::fs::remove_file(heartbeat_path());
}

fn cmd_daemon() {
    let _ = std::fs::create_dir_all(state_dir());
    let hb = heartbeat_path();
    let mut beat_once = false;
    let mut herdr_dead: u32 = 0;
    loop {
        if beat_once && !hb.exists() {
            break; // stop requested
        }
        let _ = std::fs::write(&hb, std::process::id().to_string());
        beat_once = true;
        // If herdr is gone there is nobody to show tokens to; give up after
        // ~30 minutes so the process does not linger forever.
        let probe = vec![herdr_bin(), "workspace".into(), "list".into()];
        if run_capture(&probe).is_some() {
            herdr_dead = 0;
        } else {
            herdr_dead += 1;
            if herdr_dead >= HERDR_DEAD_CYCLE_LIMIT {
                break;
            }
        }
        if let Some(machines) = load_machines() {
            let mut results: HashMap<String, QueryResult> = HashMap::new();
            for m in &machines {
                results.insert(m.name.clone(), ssh_squeue(&m.host, &m.user));
            }
            report_sidebar_token(&machines, &results);
        }
        std::thread::sleep(Duration::from_secs(REFRESH_SECS));
    }
    let _ = std::fs::remove_file(&hb);
}

// -------------------------------------------------------------------- main --

fn cmd_sidebar() {
    use crossterm::event::{Event, KeyCode, KeyEventKind};
    // Make sure the background token reporter is running so the Machines
    // sidebar keeps job counts even after this pane is closed.
    cmd_start();
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
            results.insert(m.name.clone(), ssh_squeue(&m.host, &m.user));
        }
        print!("{}", render(&machines, &results, SystemTime::now()));
        use std::io::Write;
        let _ = std::io::stdout().flush();
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
        Some("start") => cmd_start(),
        Some("stop") => cmd_stop(),
        Some("daemon") => cmd_daemon(),
        Some("keytest") => cmd_keytest(),
        _ => cmd_sidebar(),
    }
}

// Keep Path import used even if config paths change.
#[allow(dead_code)]
fn _unused(_p: &Path) {}
