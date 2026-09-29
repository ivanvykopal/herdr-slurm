# herdr-slurm

Live Slurm and PBS/OpenPBS job monitor sidebar for [herdr](https://herdr.dev).
Polls each configured cluster over SSH and renders a compact job table in a
split pane. Implemented in Rust.

## Features

- Per-machine status: job ID, name, account, state, elapsed time, nodes, and
  pending reason / node list, colored by state (green RUNNING, yellow
  PENDING, red FAILED/CANCELLED/TIMEOUT).
- Width-adaptive table: full (≥96 cols), medium (≥64), compact (below),
  re-measured on every poll.
- 20 s polling, `r` to refresh immediately, `q` to quit.
- Multiple clusters: one `[[machine]]` block each.
- Graceful degradation: unreachable machines and scheduler errors are shown
  inline instead of killing the pane.
- **Native sidebar integration, always on**: a detached background daemon
  reports a `$scheduler` token per machine with
  `herdr workspace report-metadata`, delivered over ssh straight to the
  herdr server on the cluster — no jobs pane required, and no `herdr
  machine` label has to match. The daemon is spawned by herdr's
  `[[startup]]` hook (and re-spawned by the pane if needed); it polls the
  configured scheduler every 20 s per cluster while herdr is up. A machine
  without a reachable herdr server falls back to local workspaces with a
  name prefix. Add it to your Space rows in `config.toml`:

  ```toml
  [ui.sidebar.spaces]
  rows = [
    ["state_icon", "workspace"],
    ["branch", "git_status"],
    [{ token = "$scheduler", fg = "#94e2d5", dim = false }],
  ]
  ```

  `$slurm` is also emitted for compatibility with existing sidebar configs.
  The tokens expire ~90 s after the last poll, so they disappear cleanly if
  the daemon dies. `herdr-slurm stop` shuts the daemon down (or delete
  `daemon.heartbeat` in the plugin state dir); the startup hook starts it
  again on the next herdr launch. (A full third sidebar *section* like
  Machines/Agents is not possible via the plugin API; only row tokens are
  supported.)

## Install / link

```sh
cargo build --release
herdr plugin link /path/to/herdr-slurm
herdr plugin pane open --plugin ivan.herdr-slurm --entrypoint jobs-windows
```

On linux/macos the entrypoint is `jobs`. `herdr plugin install` from GitHub
runs `cargo build --release` automatically via the manifest `[[build]]` step.

## Configuration

```sh
herdr plugin config-dir ivan.herdr-slurm
```

Edit `machines.toml` (auto-created from a template on first run):

```toml
[[machine]]
name = "MyCluster"          # display label (pane + sidebar fallback prefix)
host = "login.example.org"
scheduler = "slurm"         # "slurm" (default) or "pbs"
scheduler_user = "queue-user"
# scheduler_command = "/path/to/squeue" # optional; executable only
# ssh_user = "login-user"   # optional; prefer ~/.ssh/config
# session = "agents"        # herdr session on the cluster; default if omitted
```

`scheduler_user` selects the jobs shown. `ssh_user`, when set, controls only
the SSH identity; otherwise `host` is passed directly to SSH so `~/.ssh/config`
can select the login. Existing configs remain valid: omitted `scheduler`
defaults to `slurm`, and `user` remains an alias for `scheduler_user`.
SSH must work non-interactively (`BatchMode=yes`) — key-based auth required.

`name` is display-only; the sidebar token is reported over ssh to the herdr
server on the cluster itself (pinned to `session` when set), so no `herdr
machine` label needs to match. A machine without a reachable herdr server
falls back to local workspaces with the `name` prefix.

An invalid `[[machine]]` entry is skipped, not fatal. The pane prints
`herdr-slurm: skipping machine '<name>': <reason>` and keeps rendering every
valid cluster. Only an unparseable file (`invalid TOML in <path>`) or a file
with no valid entries left (`invalid config in <path>: no valid machines; ...`)
blanks the pane.

For ALCF PBS systems such as Aurora:

```toml
[[machine]]
name = "Aurora"
host = "aurora"             # SSH config alias
scheduler = "pbs"
scheduler_user = "your-alcf-username"
scheduler_command = "/opt/pbs/bin/qstat"
```

`scheduler_command` selects the remote scheduler executable when it is not on
the noninteractive SSH `PATH`. It defaults to `squeue` for Slurm and `qstat`
for PBS. Set an executable name or path only; shell syntax and arguments are
rejected.

PBS queries use `<scheduler_command> -f -F json`, then filter `Job_Owner`
locally, falling back to `euser` and `Variable_List.PBS_O_LOGNAME`. The parser
maps PBS states to the shared display states, tolerates common non-standard
`qstat` JSON values, and reads walltime, account/project, node count (`nodect`
or `select`), comments, and `exec_host`. Each
remote query is terminated after 15 seconds.
Scheduler capture retains at most 8 MiB of stdout and 1 MiB of stderr while
continuing to drain both streams, then reports oversized output as an error.

## Actions

| Action | What it does |
|---|---|
| `ivan.herdr-slurm.open` | Open/focus the Scheduler Jobs sidebar pane (resized to 50 cols) |
| `ivan.herdr-slurm.refresh` | One-shot scheduler snapshot, printed to stdout |

Example keybinding (in herdr config):

```toml
[[keys.command]]
key = "prefix+m"
type = "plugin_action"
command = "ivan.herdr-slurm.open-windows" # use ...open on linux/macos
description = "Scheduler jobs sidebar"
```

## Notes

- Slurm data comes from `squeue`; PBS data comes from `qstat`. Historical jobs
  are not shown.
- Requirements: Rust toolchain (to build), OpenSSH client on PATH, and either
  `squeue` or JSON-capable `qstat` on each remote cluster.
