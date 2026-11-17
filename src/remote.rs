//! Helpers shared by the per-job / per-cluster subcommands: job-id checks,
//! sibling scheduler tools, ssh execution and `@@herdr:<name>@@` sections.
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use crate::{
    config_dir, new_command, parse_machines, run_command_with_timeout, ssh_argv, Machine,
    ParsedMachines, Scheduler, TEMPLATE,
};

pub struct RemoteOut {
    pub stdout: String,
    pub stderr: String,
    #[allow(dead_code)] // read by the action subcommand (later task)
    pub ok: bool,
}

/// Job ids are pasted into remote shell commands, so accept only the
/// scheduler's id grammar: Slurm `123` / `123_4`; PBS `4021`, `4021[]`,
/// `4021[7]`, each optionally followed by `.server` (alnum, `-`, `.`).
pub fn valid_job_id(s: &Scheduler, id: &str) -> bool {
    let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    match s {
        Scheduler::Slurm => match id.split_once('_') {
            Some((job, task)) => digits(job) && digits(task),
            None => digits(id),
        },
        Scheduler::Pbs => {
            let (head, server) = match id.split_once('.') {
                Some((h, srv)) => (h, Some(srv)),
                None => (id, None),
            };
            if let Some(srv) = server {
                if srv.is_empty()
                    || !srv
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
                {
                    return false;
                }
            }
            match head.split_once('[') {
                Some((job, rest)) => match rest.strip_suffix(']') {
                    Some(idx) => digits(job) && (idx.is_empty() || digits(idx)),
                    None => false,
                },
                None => digits(head),
            }
        }
    }
}

/// Path of a sibling scheduler tool: next to `scheduler_command` when that is
/// a path, else the bare name (resolved via the remote PATH).
pub fn tool(m: &Machine, name: &str) -> String {
    match m.scheduler_command.rfind('/') {
        Some(i) => format!("{}{name}", &m.scheduler_command[..=i]),
        None => name.to_string(),
    }
}

/// Run `script` on the cluster over ssh. `Err` only when ssh cannot be
/// spawned or times out; a failing remote command yields `ok: false`.
pub fn run_remote(m: &Machine, script: &str, timeout: Duration) -> Result<RemoteOut, String> {
    let args = ssh_argv(&m.ssh_target(), script);
    let mut command = new_command(&args[0]);
    command.args(&args[1..]).stdin(Stdio::null());
    let out = run_command_with_timeout(&mut command, timeout)?;
    Ok(RemoteOut {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        ok: out.status.success(),
    })
}

/// Split output on `@@herdr:<name>@@` marker lines; each body is its lines
/// joined with `\n` (no trailing newline). Text before the first marker is dropped.
pub fn sections(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        let marker = line
            .trim_end_matches('\r')
            .strip_prefix("@@herdr:")
            .and_then(|r| r.strip_suffix("@@"));
        if let Some(name) = marker {
            if let Some((n, body)) = current.take() {
                out.insert(n, body.join("\n"));
            }
            current = Some((name.to_string(), Vec::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((n, body)) = current {
        out.insert(n, body.join("\n"));
    }
    out
}

pub fn find_machine<'a>(ms: &'a [Machine], name: &str) -> Option<&'a Machine> {
    ms.iter().find(|m| m.name == name)
}

/// Read (creating the template if absent) and parse `machines.toml`.
pub fn load_parsed() -> Result<ParsedMachines, String> {
    let path = config_dir().join("machines.toml");
    if !path.exists() {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(&path, TEMPLATE);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_machines(&raw).map_err(|e| format!("invalid config in {}: {e}", path.display()))
}

pub fn print_json(v: serde_json::Value) {
    println!("{v}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_machine_entry, Scheduler};

    fn machine(scheduler_command: &str) -> Machine {
        let entry: toml::Value = toml::from_str(&format!(
            "host = \"h\"\nscheduler_user = \"u\"\nscheduler_command = \"{scheduler_command}\""
        ))
        .unwrap();
        parse_machine_entry(&entry).unwrap()
    }

    #[test]
    fn slurm_job_id_accepts_array_task() {
        assert!(valid_job_id(&Scheduler::Slurm, "123_4"));
        assert!(valid_job_id(&Scheduler::Slurm, "123"));
    }

    #[test]
    fn slurm_job_id_rejects_shell() {
        for bad in ["", "12;rm", "12 3", "-1", "12_", "_1"] {
            assert!(!valid_job_id(&Scheduler::Slurm, bad), "{bad}");
        }
    }

    #[test]
    fn pbs_job_id_accepts_server_and_array() {
        assert!(valid_job_id(&Scheduler::Pbs, "4021.pbs01"));
        assert!(valid_job_id(&Scheduler::Pbs, "4021[].pbs01"));
        assert!(!valid_job_id(&Scheduler::Pbs, "4021.pbs;x"));
    }

    #[test]
    fn tool_uses_scheduler_command_dir() {
        assert_eq!(
            tool(&machine("/opt/slurm/bin/squeue"), "scancel"),
            "/opt/slurm/bin/scancel"
        );
        assert_eq!(tool(&machine("squeue"), "scancel"), "scancel");
    }

    #[test]
    fn sections_split_on_markers() {
        let s = sections("@@herdr:a@@\nx\ny\n@@herdr:b@@\nz\n");
        assert_eq!(s["a"], "x\ny");
        assert_eq!(s["b"], "z");
    }

    #[test]
    fn find_machine_matches_name() {
        let mut m = machine("squeue");
        m.name = "Polaris".into();
        let ms = [m];
        assert!(find_machine(&ms, "Polaris").is_some());
        assert!(find_machine(&ms, "Other").is_none());
    }
}
