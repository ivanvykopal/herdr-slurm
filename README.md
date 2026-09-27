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
- **Native sidebar integration**: while the pane runs, it reports a `$slurm`
  token per machine via `herdr --machine <label> workspace report-metadata`,
  so the summary shows under the matching machine in the Spaces sidebar. A
  machine without a reachable herdr server falls back to local workspaces
  with a name prefix. Add it to your Space rows in `config.toml`:

  ```toml
  [ui.sidebar.spaces]
  rows = [
    ["state_icon", "workspace"],
    ["branch", "git_status"],
    [{ token = "$slurm", fg = "#94e2d5", dim = false }],
  ]
  ```

  The token expires ~90 s after the last poll, so it disappears cleanly if
  the pane dies. (A full third sidebar *section* like Machines/Agents is not
  possible via the plugin API; only row tokens are supported.)

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
name = "MyCluster"          # match your `herdr machine` label
host = "login.example.org"
user = "your-cluster-login"
```

`user` is your **cluster** login, which usually differs from your local
username. SSH must work non-interactively (`BatchMode=yes`) — key-based auth
required. Make `name` identical to the label of a saved `herdr machine` so
the sidebar token lands under the right machine.

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
