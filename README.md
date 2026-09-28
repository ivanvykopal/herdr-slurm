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
- **Native sidebar integration**: while the pane runs, it reports a
  `$scheduler` token per machine via
  `herdr --machine <label> workspace report-metadata`,
  so the summary shows under the matching machine in the Spaces sidebar. A
  machine without a reachable herdr server falls back to local workspaces
  with a name prefix. Add it to your Space rows in `config.toml`:

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
scheduler = "slurm"         # "slurm" (default) or "pbs"
scheduler_user = "queue-user"
# ssh_user = "login-user"   # optional; prefer ~/.ssh/config
```

`scheduler_user` selects the jobs shown. `ssh_user`, when set, controls only
the SSH identity; otherwise `host` is passed directly to SSH so `~/.ssh/config`
can select the login. Existing configs remain valid: omitted `scheduler`
defaults to `slurm`, and `user` remains an alias for `scheduler_user`.

For ALCF PBS systems such as Aurora:

```toml
[[machine]]
name = "Aurora"
host = "aurora"             # SSH config alias
scheduler = "pbs"
scheduler_user = "your-alcf-username"
```

PBS queries use `qstat -f -F json`, then filter `Job_Owner` locally. The
parser maps PBS states to the shared display states and reads walltime,
account/project, node count (`nodect` or `select`), comments, and `exec_host`.
SSH must work non-interactively (`BatchMode=yes`). Make `name` identical to
the label of a saved `herdr machine` so sidebar metadata lands correctly.

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
