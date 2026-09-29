# herdr-slurm

Live SLURM/HPC job monitor sidebar for [herdr](https://herdr.dev). Polls
`squeue` over SSH for every configured cluster and renders a compact job
table in a split pane. Implemented in Rust.

## Features

- Per-machine status: job ID, name, account, state, elapsed time, nodes, and
  pending reason / node list, colored by state (green RUNNING, yellow
  PENDING, red FAILED/CANCELLED/TIMEOUT).
- Width-adaptive table: full (≥96 cols), medium (≥64), compact (below),
  re-measured on every poll.
- 20 s polling, `r` to refresh immediately, `q` to quit.
- Multiple clusters: one `[[machine]]` block each.
- Graceful degradation: unreachable machines and `squeue` errors are shown
  inline instead of killing the pane.
- **Native sidebar integration, always on**: a detached background daemon
  reports a `$slurm` token per machine with
  `herdr workspace report-metadata`, delivered over ssh straight to the
  herdr server on the cluster — no jobs pane required.
  The daemon is spawned by herdr's `[[startup]]` hook (and re-spawned by the
  pane if needed); it polls `squeue` every 20 s per cluster while herdr is
  up. A machine without a reachable herdr server falls back to local
  workspaces with a name prefix. Add it to your Space rows in `config.toml`:

  ```toml
  [ui.sidebar.spaces]
  rows = [
    ["state_icon", "workspace"],
    ["branch", "git_status"],
    [{ token = "$slurm", fg = "#94e2d5", dim = false }],
  ]
  ```

  The token expires ~90 s after the last poll, so it disappears cleanly if
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

[[machine]]
name = "MyCluster"          # display label (pane + sidebar fallback prefix)
host = "login.example.org"
user = "your-cluster-login"
# session = "agents"        # herdr session on the cluster; default if omitted
```

`user` is your **cluster** login, which usually differs from your local
username. SSH must work non-interactively (`BatchMode=yes`) — key-based auth
required. `name` is display-only; the sidebar token is reported over ssh to
the herdr server on the cluster itself, so no `herdr machine` label needs
to match.

## Actions

| Action | What it does |
|---|---|
| `ivan.herdr-slurm.open` | Open/focus the SLURM Jobs sidebar pane (resized to 50 cols) |
| `ivan.herdr-slurm.refresh` | One-shot `squeue` snapshot, printed to stdout |

Example keybinding (in herdr config):

```toml
[[keys.command]]
key = "prefix+m"
type = "plugin_action"
command = "ivan.herdr-slurm.open-windows" # use ...open on linux/macos
description = "SLURM jobs sidebar"
```

## Notes

- Data comes from `squeue` only. `sacct` history is not shown because the
  slurmdbd on the author's cluster was unreachable when this was written;
  errors rather than crashing if services change.
- Requirements: Rust toolchain (to build), OpenSSH client on PATH.
