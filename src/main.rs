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

const TEMPLATE: &str = r#"# herdr-slurm machines. One [[machine]] block per cluster.
# `user` is your cluster login (often differs from your local username).
# `name` should match a saved `herdr machine` label so the sidebar token
# lands under the right machine.

[[machine]]
name = "MyCluster"
host = "login.example.org"
user = "your-cluster-login"
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
            out.push(Machine { name: g("name"), host: g("host"), user: g("user") });
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
    let out = Command::new("ssh")
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

fn run_capture(args: &[String]) -> Option<String> {
    let out = Command::new(&args[0]).args(&args[1..]).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

fn workspace_ids(machine: Option<&str>, cache: &mut HashMap<String, (Instant, Vec<String>)>) -> Vec<String> {
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
                "--token".to_string(),
                format!("slurm={value}"),
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
    let out = Command::new(&herdr)
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
    let out = Command::new(herdr)
        .args(["pane", "resize", "--pane", pane_id, "--direction", direction, "--amount"])
        .arg(format!("{amount:.4}"))
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err("resize failed".into()) }
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
            results.insert(m.name.clone(), ssh_squeue(&m.host, &m.user));
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
