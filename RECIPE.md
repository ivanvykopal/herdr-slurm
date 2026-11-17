# Slurm Jobs

A right-hand side panel for the Herdr desktop app that lists your Slurm and
PBS/OpenPBS jobs, one collapsible section per cluster. A **Slurm** button in the
top bar, beside Files, Git and Skills, switches it on and off. Click a job to
see its logs, allocation and GPU use, and to cancel, requeue, hold or release
it. Under each Slurm cluster a Projects block shows the compute hours of your
projects against their award. Finished jobs, nearly full storage or quotas,
and projects at 90 % of their hours raise notifications.

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
  - one storage bar per path, `/project 45%`: `ansi2` below 80, `ansi3` 80–89,
    `blocked` at 90 and above. The % is the higher of filesystem % and the
    `cluster` quota %, unless a user quota from `herdr-slurm projects` covers
    the path (its path equals the bar's path or is a parent directory of it;
    the longest such path wins). Then the bar shows that quota's % and the
    label ends in `quota`: `/mnt/home/<user> 42% quota`.
  - A cluster query error shows `cluster: <message>` in dim text instead.
- Projects block under the strip of an open Slurm machine (nothing on PBS
  machines, nothing before the first `projects` result):
  - a `Projects` heading, then per active project the account and, dim and
    truncated, its title when `perunfsusage` gives one;
  - one bar per kind, `CPU 712/88474 h` and `GPU 3120/7964 h`, colored by %
    used like storage. Without an award the label is `CPU 712 h`; with an
    award but unknown use it is `CPU ?/5000 h`; both draw an empty `muted`
    bar. A kind with neither use nor award is left out;
  - `you: 446 GPU h (30 d)`, your own GPU hours of the last 30 days, when
    known;
  - a burn line per kind, `GPU ~325 days left at the 30-day rate`:
    (awarded − used) / (30-day use / 30), rounded down. It appears only when
    used, awarded and a positive 30-day use are all known and hours remain;
  - the project's storage bar, `/mnt/project 64%`, when its quota has a
    limit;
  - `no active projects` for an empty list, `projects: <message>` when the
    machine's query failed and no earlier result exists, and
    `could not read <tool>` with its first 5 lines when `sprojects`, `myquota`
    or `perunfsusage` printed output the parser did not understand.
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
  20 s (or on `↻ Refresh`), `cluster` every 120 s and `projects` every 600 s
  (the first right after the first snapshot), and publishes the results in
  `shared("slurm.state")`; every panel copy renders from that.
  - The lease is `shared("slurm.leader") = {copy, at, watch}`, renewed every
    5 s and stale after 60 s.
  - A watcher copy (`props.watch`) is preferred: it takes the lease even from
    a panel copy that holds a fresh one. A panel copy leads only when no fresh
    lease exists, so the panel works in layouts without a watcher.
  - After a mod reload the old copy's lease stays fresh, so polling and
    notifications can pause for about 60 s until it goes stale.
  - The leader also keeps the running-job, storage-alert and project-alert
    maps in `slurm.state`; a new leader adopts them only when they are under
    120 s old, otherwise it reseeds (no notifications for jobs that ended
    while nobody watched).
- A `projects` result is merged over the last good one: a top-level `error`
  keeps the previous result whole, and a machine whose entry has an `error`
  keeps its previous entry. A cluster that fails one poll keeps its Projects
  block and its quota % in the strip, and its quota alerts do not re-arm.
- `↻ Refresh` writes `shared("slurm.refresh")`; the leader polls on its next
  tick. Clicking a machine bar folds it (and hides its strip, Projects block
  and open job).
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
  after it has dropped below 85 %. The % is the one on the bar, so a home
  directory at 92 % of its user quota alerts even when the filesystem is at
  15 %. Project hours work the same way per account and kind: one `warning`
  `<machine>: <account> GPU hours 92% used` (body `Ask the PI for more hours
  or slow down.`) at 90 % of the award, again only after it has dropped below
  85 %. A kind without an award or without known use never alerts. Project
  storage does the same at 90 % of its quota: `<machine>: <account> storage
  98% of quota` (body `Free space before jobs fail.`). Neither
  alert plays a sound.
- An unreachable cluster or scheduler error shows `✗ <message>` in red inside
  its section; other clusters keep working.
- A missing `herdr-slurm` binary shows a hint instead of a blank panel.
- The panel is a `panel-body` stack child, so the top-bar button, the layout's
  per-space panel memory and `layout.select`/`layout.show` all work on it.

## Built from

- `mods/slurm/main.js` (polling, lease, notifications, actions),
  `detail.js` (expanded job block), `strip.js` (cluster strip), `projects.js`
  (Projects block), `logic.js` (pure helpers, tested by `logic.test.js`),
  `mod.json` (`{"place":"none"}`), `icon.svg`.
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
  - `herdr-slurm projects`: every machine in parallel, one ssh each, 60 s cap
    per machine (the mod waits up to 120 s for the whole call),
    `{error, machines:[{name, error, source, projects:[{account, title,
    active, cpu:{used, awarded, used_30d, mine}, gpu:{…}, storage:{path,
    used_kb, limit_kb, pct}}], quotas:[{kind, name, path, used_kb, limit_kb,
    pct}], raw:{sprojects, myquota, perunfsusage}}]}`. A PBS machine is
    `{name, error: null, projects: [], quotas: [], unsupported: "pbs"}`.
    - Accounts come from `sshare -U -u <user>`. Left out: accounts whose
      association `GrpTRES` contains `node=0` (expired), accounts that
      `sprojects` lists as inactive or does not list, and accounts that
      `perunfsusage` marks `inactive`.
    - `source` is `"sprojects"` when `sprojects -f` parsed with at least one
      awarded CPU or GPU value; its used and awarded hours are then taken as
      they are. Otherwise `source` is `"sreport"`, and any `sprojects`
      output ends up in `raw`.
    - With `"sreport"`, `awarded` is the QOS `GrpTRESMins` of `<acct>` and
      `<acct>_gpu` (`cpu=`, `gres/gpu=` or else the sum of
      `gres/gpu:<type>=`) in minutes / 60, `null` without a cap. `used` is
      the account-total row of `sreport cluster AccountUtilizationByUser`
      since 2020-01-01; `null` when the cluster returns no account totals
      (never the sum of the per-user rows you can see), and 0 when the report
      has totals for other accounts but none for this one (sreport omits
      zero rows).
    - `used_30d` is the account total of the same report from
      `$(date -d '30 days ago' +%F)`, and `mine` your own row in it. Either is
      `null` when the report has no such row. On some clusters (Devana) the
      30-day report returns no rows, so both stay `null` and the block shows
      no `you:` line and no days-left line.
    - Hours are integers. `title` and `storage` come from the account's
      `perunfsusage` Project row; a row whose limit is `TBD` gives
      `storage: null` but still marks the project active or inactive.
    - `quotas` are your user rows from `myquota` and `perunfsusage`, sizes in
      KiB (`KB` to `TB` as powers of 1024), `pct` from the tool or else
      used / limit. User rows with a `TBD` limit are dropped.
    - `raw.<tool>` is the tool's output when it printed something and
      nothing parsed, else `null`. A failing tool leaves only its own part
      empty; an ssh failure, the timeout or a missing end marker sets the
      machine's `error`.
- Config is `machines.toml` in `herdr plugin config-dir ivan.herdr-slurm`. The
  optional per-machine `storage = ["$HOME", "/mnt/scratch"]` picks the paths
  for the storage bars (default: `$HOME`, `/scratch`, `/mnt/scratch`,
  `/project`, whichever exist). The optional
  `slurm_conf = "~/slurm-custom/slurm/custom_slurm.conf"` is exported as
  `SLURM_CONF` (leading `~` as `$HOME`) at the start of every remote script
  for that machine, for clusters that refuse `sacctmgr` and `sreport` under
  the default config.
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
  has the `job`, `action`, `final`, `cluster` and `projects` subcommands. An
  older binary shows the job list; the expanded block shows only an error,
  there is no strip, Projects block or storage alert, and finished jobs are
  reported as `UNKNOWN`. A binary that has `cluster` but not `projects` shows
  no Projects block and no quota % on the bars.
- `projects` runs `sacctmgr` and `sreport`; where those are refused under the
  cluster's default config, set `slurm_conf` or the hours stay empty. sreport
  can be slow, hence the 60 s cap and the 600 s interval.
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
- The cluster strip shows GPU chips and storage bars under each open machine;
  a path under a user quota shows `<pct>% quota`.
- Each open Slurm machine shows a Projects block: hours bars per active
  project, or `no active projects`.
- A job leaving RUNNING raises one notification, also with the panel hidden
  when `slurm-bg` is in the layout.
