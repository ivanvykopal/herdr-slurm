# Slurm Jobs

A right-hand side panel for the Herdr desktop app that lists your Slurm and
PBS/OpenPBS jobs, one collapsible section per cluster. A **Slurm** button in the
top bar, beside Files, Git and Skills, switches it on and off. Click a job to
see its logs, allocation and GPU use, and to cancel, requeue, hold or release
it. Finished jobs and nearly full storage raise notifications.

![Slurm Jobs](preview.png)

## What you see

- Header row: **Jobs** (14px, semibold), then "refreshing…" or "12s ago", then a
  `↻ Refresh` button.
- One section per machine in `machines.toml`: a raised bar with `▾`/`▸`, the
  machine name (13px semibold), `slurm · <user>` (dim) and a summary on the
  right (`1 running, 2 pending`, or `unreachable`, or `no jobs`).
- Cluster strip under an open machine bar:
  - one chip per GPU partition and type, `gpu a100-sxm4 11/32 free` (`ansi2`
    when any GPU is free, `muted` at 0);
  - one storage bar per path, `/project 45%` (the higher of filesystem % and
    quota %): `ansi2` below 80, `ansi3` 80–89, `blocked` at 90 and above.
  - A cluster query error shows `cluster: <message>` in dim text instead.
- Under an open section, one row per job: job id, state, name, account,
  elapsed, nodes, and the pending reason (else the node list). A pending Slurm
  job with a scheduler estimate shows `starts ~14:00` (or `starts ~Tue 09:30`
  when not today) in place of the reason.
- Colors come from the theme: RUNNING ansi green (`ansi2`), PENDING/QUEUED/HELD
  ansi yellow (`ansi3`), FAILED/CANCELLED/TIMEOUT/NODE_FAIL/OUT_OF_MEMORY/
  PREEMPTED `blocked`, anything else `muted`.
- Narrow panels drop columns: below 640px the account, nodes and detail columns
  go; below 420px the reason/node list goes too.
- Clicking a job row highlights it and opens a block under it (one open job per
  machine), top to bottom:
  - Action bar: `Cancel` `Requeue` `Hold` `Release`; `Terminal on node`
    (Slurm, RUNNING only); `Open log in agent`; `GPU usage` (Slurm, RUNNING,
    with allocated GPUs). Cancel and Requeue first show an inline
    `Cancel job 123? Yes / No` row; Hold and Release run at once. A running
    action shows `Cancel…` and disables the other verbs.
  - Facts line: node list, `start <time>`, `N cpus`, memory, `N GPU`, and per
    step `AveCPU / MaxRSS`.
  - After `GPU usage`: one chip per GPU, `n141#0 97% 30/40G`, and in `ansi3`
    `2 of 4 GPUs busy` when fewer GPUs than allocated reach 10 % utilisation.
  - Log tabs `out` and `err` (`err` hidden when both go to the same file), the
    last 50 lines in the editor font, at most 220px high, scrolled to the
    newest line when the log changes.
  - A detail error (for example a job past MinJobAge) shows in red above the
    last known detail.

## How it behaves

- Exactly one copy of the mod polls, the lease holder. It runs `snapshot` every
  20 s (or on `↻ Refresh`) and `cluster` every 120 s, and publishes the result
  in `shared("slurm.state")`; every panel copy renders from that.
  - The lease is `shared("slurm.leader") = {copy, at, watch}`, renewed every
    5 s and stale after 60 s.
  - A watcher copy (`props.watch`) is preferred: it takes the lease even from
    a panel copy that holds a fresh one. A panel copy leads only when no fresh
    lease exists, so the panel works in layouts without a watcher.
  - After a mod reload the old copy's lease stays fresh, so polling and
    notifications can pause for about 60 s until it goes stale.
  - The leader also keeps the running-job and storage-alert maps in
    `slurm.state`; a new leader adopts them only when they are under 120 s
    old, otherwise it reseeds (no notifications for jobs that ended while
    nobody watched).
- `↻ Refresh` writes `shared("slurm.refresh")`; the leader polls on its next
  tick. Clicking a machine bar folds it (and hides its strip and open job).
- An open job re-fetches `herdr-slurm job` every 20 s. An action refreshes the
  snapshot and the open job; a failed action raises an `error` notification.
- `Terminal on node` opens a tab running
  `ssh -t <target> srun --jobid=<id> --overlap --pty bash -l`, where `<target>`
  is the snapshot's `ssh_target` (else `host`).
- `Open log in agent` opens a tab named `log <id>` running the `agent` prop
  (default `claude`) with a prompt holding machine, ssh target, job id, state,
  log paths and the log tails, asking it to diagnose the job.
- Notifications (leader only). A job seen RUNNING or COMPLETING in the
  previous snapshot that is now gone or in another state is resolved with one
  `herdr-slurm final` call per machine (up to 50 ids per call):

  | Final state | Level | Sound |
  |---|---|---|
  | COMPLETED | `success` | none |
  | FAILED, OUT_OF_MEMORY, TIMEOUT, NODE_FAIL, CANCELLED, PREEMPTED, BOOT_FAIL, DEADLINE | `error` | `playSound("blocked")` |
  | UNKNOWN, anything else | `warning` | none |

  Title `<machine>: job <id> <STATE>`. A machine whose snapshot has an error
  keeps its jobs (unreachable is not finished). Storage raises one `warning`
  (`<machine>: <path> 92% full`) when a path reaches 90 %, and again only
  after it has dropped below 85 %.
- An unreachable cluster or scheduler error shows `✗ <message>` in red inside
  its section; other clusters keep working.
- A missing `herdr-slurm` binary shows a hint instead of a blank panel.
- The panel is a `panel-body` stack child, so the top-bar button, the layout's
  per-space panel memory and `layout.select`/`layout.show` all work on it.

## Built from

- `mods/slurm/main.js` (polling, lease, notifications, actions),
  `detail.js` (expanded job block), `strip.js` (cluster strip), `logic.js`
  (pure helpers, tested by `logic.test.js`), `mod.json` (`{"place":"none"}`),
  `icon.svg`.
- The Rust CLI in this repo, over SSH (all subcommands print JSON with an
  `error` field):
  - `herdr-slurm snapshot`: every machine in parallel,
    `{error, warnings, machines:[{name, host, ssh_target, scheduler, user,
    error, fetched_at, jobs:[{id, name, state, elapsed, nodes, account,
    reason, where}]}]}`.
  - `herdr-slurm job <machine> <id> --lines 50 [--gpu]`: state, allocation,
    node list, start time, log paths and tails, step usage, and with `--gpu`
    live per-GPU use.
  - `herdr-slurm action <machine> <id> <cancel|requeue|hold|release>`:
    `scancel`/`scontrol requeue|hold|release` on Slurm,
    `qdel`/`qrerun`/`qhold`/`qrls` on PBS.
  - `herdr-slurm final <machine> <id>...`: final state and exit code.
  - `herdr-slurm cluster`: GPU partitions, pending start estimates and
    storage use per machine.
- Config is `machines.toml` in `herdr plugin config-dir ivan.herdr-slurm`. The
  optional per-machine `storage = ["$HOME", "/mnt/scratch"]` picks the paths
  for the storage bars (default: `$HOME`, `/scratch`, `/mnt/scratch`,
  `/project`, whichever exist).
- Props: `bin` overrides the executable (default `herdr-slurm` on PATH);
  `agent` is the command for `Open log in agent` (default `claude`); `watch`
  marks the background watcher copy.
- `herdr` functions: `exec`, `theme`, `props`, `shared`, `setShared`, `notify`,
  `playSound`, `run` (`tab.new`), `settings`, `checking`; `cx.timer.every`,
  `cx.spawn`, `on_size_change`, `ScrollHandle`.
- Layout:
  - add `{ "id": "slurm", "type": "block", "content": { "mod": "slurm" } }` to
    the `panel-body` stack, and a top-bar button that runs
    `layout.select {stack:"panel-body", child:"slurm"}` then
    `layout.show {id:"panel"}` (and add `"slurm"` to the bar's list of enabled
    panels);
  - add the zero-height watcher so polling and notifications run while the
    panel is hidden:
    `{ "id": "slurm-bg", "type": "block", "size": {"px": 0}, "content": {"mod": "slurm", "props": {"watch": true}} }`.

## Watch out

- The mod runs `ssh` (BatchMode, key auth) via the binary. It reads no files
  and makes no network request of its own.
- Needs `cargo install --path . --locked` first, so the `herdr-slurm` on PATH
  has the `job`, `action`, `final` and `cluster` subcommands. An older binary
  shows the job list but no detail, strip or notifications.
- Cancel, Requeue, Hold and Release act on the real scheduler.
- `Terminal on node` and `GPU usage` use `srun --overlap`, so they need Slurm
  and a RUNNING job.
- Without the `slurm-bg` watcher, polling and notifications stop when no panel
  copy is rendered; after a mod reload expect a gap of about 60 s.
- Colors must come from `ansi2`/`ansi3`, not `working`/`done`: those are amber
  and blue in the default theme.
- Only a stack child of a layout's `panel-body` is a toggleable panel. The mod
  alone (`place: none`) shows nowhere until a layout or a tab (`tab.new --mod slurm`) hosts it.

## Done when

- `herdr-app check mods/slurm` passes; `node --test mods/slurm/logic.test.js`
  and `cargo test` pass.
- With a stub binary printing the snapshot JSON above, running, pending and
  failed jobs show in green, yellow and red, and a machine with `error` shows
  `unreachable`.
- With no binary, the panel shows the install hint.
- The top-bar Slurm button toggles the panel.
- Clicking a running job shows the action bar, facts and log tabs; Cancel asks
  before running.
- The cluster strip shows GPU chips and storage bars under each open machine.
- A job leaving RUNNING raises one notification, also with the panel hidden
  when `slurm-bg` is in the layout.
