# Slurm projects, compute hours and quotas: design

Date: 2026-11-17. Status: implemented on `feat/slurm-projects`; the
corrections marked *Implemented:* record where the code differs.
Builds on `docs/superpowers/specs/2026-11-17-slurm-mod-extensions-design.md`
(merged to `main` in d7ce97a). Method borrowed from
`perun/slurm-monitor/src-tauri/src/projects.rs`.

## Goal

The Slurm panel shows, per cluster, the projects (Slurm accounts) you belong
to: compute hours used against what was awarded, how fast they are being
spent, your own share, and the project's storage against its quota. It warns
before an award or a quota runs out.

## Decisions (from the chat)

- Inactive / expired projects are **hidden**.
- Toasts at **90 %** of a project's CPU-hour or GPU-hour award, and at 90 %
  of a user or project storage **quota** (quota replaces filesystem % where
  a quota tool exists).
- Only **your own** share per project; no member list.
- New optional `slurm_conf` per machine applies to **every** remote Slurm
  command for that machine.

## Observed facts (read-only probe, 2026-11-17)

| | Perun (Slurm 26.05) | Devana (Slurm 25.11) |
|---|---|---|
| Accounts | `sshare -U -u <user> --noheader --parsable2 --format=Account,RawUsage,EffectvUsage` works | same, works |
| `sacctmgr` / `sreport` | refused by default; work with `SLURM_CONF=~/slurm-custom/slurm/custom_slurm.conf` (also fixes `sacct`; `squeue`, `sinfo`, `scontrol` keep working) | work |
| `sreport … AccountUtilizationByUser -t Hour -T cpu,gres/gpu,billing --parsable2` | account-total rows (empty Login) and per-user rows for all members | **only your own rows**; no account totals |
| `Start=-30days` | rejected ("Invalid time specification") and silently falls back to yesterday; `Start=$(date -d '30 days ago' +%F)` works | use the same form |
| Award (QOS `GrpTRESMins`, minutes) | `<acct>`: `cpu=5308440,gres/gpu=477840` → 88474 CPU h, 7964 GPU h | `<acct>`: `billing=300000` (5000 CPU h); `<acct>_gpu`: `billing=19200000` |
| Expired | assoc `GrpTRES` contains `node=0` (perun250128, perun25012246) | assoc GrpTRES empty |
| Site tool, hours | none | `sprojects -f`: per project `CPU: used/awarded`, `GPU: used/awarded` (hours), ACTIVE / INACTIVE sections, shared storage paths |
| Site tool, storage | `perunfsusage`: rows `User|Project <name> <fs> <used> <limit> <pct>% <active|inactive>` plus an indented title line per project | `myquota`: user row `/home : 863GB / 86% 1000GB` |

Consequences:
- Devana project totals cannot come from `sreport`; `sprojects` is the source
  of truth there. Perun has no `sprojects`; the slurm-monitor method is the
  source there.
- 30-day windows use an explicit date computed on the cluster.

## Configuration

`machines.toml` gains one optional key:

```toml
slurm_conf = "~/slurm-custom/slurm/custom_slurm.conf"   # optional
```

Validated like `storage` entries (charset `[A-Za-z0-9_./~$-]`, no `..`
segment, non-empty); invalid → machine skipped with warning
`invalid slurm_conf '<v>'`. When set, every remote script for that machine
(snapshot's squeue/qstat call, `job`, `action`, `final`, `cluster`,
`projects`) starts with `export SLURM_CONF="<value>";` (leading `~` rewritten
to `$HOME`, as for storage). The TUI pane and daemon use the same snapshot
query, so they gain it too; machines without the key are byte-for-byte
unchanged.

## CLI: `herdr-slurm projects`

All machines in parallel; one ssh per machine; 60 s cap per machine (sreport
can be slow); prints one JSON object; `error` null on success. Remote script
sections (marker lines `@@herdr:<name>@@`, end marker `@@herdr:end@@`):

1. `sshare` — accounts: `sshare -U -u <user> --noheader --parsable2 --format=Account,RawUsage`
2. `assoc` — `sacctmgr -n show assoc user=<user> format=Account,GrpTRES --parsable2`
3. `qos` — `sacctmgr -n show qos <acct>,<acct>_gpu,… format=Name,GrpTRESMins --parsable2`
4. `sreport` — all-time from `2020-01-01`: `sreport cluster AccountUtilizationByUser Start=2020-01-01 End=now -t Hour -T cpu,gres/gpu,billing --parsable2 account=<list>`
5. `sreport30` — same with `Start=$(date -d '30 days ago' +%F)`
6. `sprojects` — `command -v sprojects >/dev/null && sprojects -f`
7. `myquota` — `command -v myquota >/dev/null && myquota`
8. `perunfsusage` — `command -v perunfsusage >/dev/null && perunfsusage`

Account names come from section 1 of the **same** script (the remote shell
builds the comma list with `paste -sd,` and the `_gpu` list with `sed`); no
second ssh. Each tool's stderr is folded into its own section; a failing tool
leaves its section empty and the others still parse. Tools 2–5 run under
`SLURM_CONF` when configured. PBS machines: `{"projects": [], "quotas": [], "unsupported": "pbs"}`.

Output:

```json
{ "error": null, "machines": [{
  "name": "Perun", "error": null, "source": "sreport",
  "projects": [{
    "account": "perun2601397", "title": "Slovak LLM Benchmarking", "active": true,
    "cpu":  { "used": 712,  "awarded": 88474, "used_30d": 712,  "mine": 712 },
    "gpu":  { "used": 3120, "awarded": 7964,  "used_30d": 446,  "mine": 446 },
    "storage": { "path": "/mnt/project", "used_kb": 6680000000, "limit_kb": 10490000000, "pct": 64 }
  }],
  "quotas": [{ "kind": "user", "name": "ivvyke441", "path": "/mnt/home", "used_kb": 221249536, "limit_kb": 524288000, "pct": 42 }],
  "raw": { "sprojects": null, "myquota": null, "perunfsusage": null }
}] }
```

Field rules:
- `projects` lists **active** accounts only (spec decision: hide inactive).
  Active = not expired by assoc `node=0`, and, when `sprojects` parsed, listed
  under ACTIVE PROJECTS; when `perunfsusage` parsed, not marked `inactive`.
- `source`: `"sprojects"` when sprojects parsed (its CPU/GPU used/awarded
  replace the sreport/QOS numbers), else `"sreport"`. *Implemented:* only
  when the parsed sprojects table has at least one awarded CPU or GPU value;
  otherwise `"sreport"`, and the sprojects text goes to `raw.sprojects`.
- `cpu`/`gpu`: `used`, `awarded` in hours (integers, rounded); `awarded`
  from QOS `GrpTRESMins` minutes / 60 (`cpu=`, `gres/gpu=`; `gres/gpu:<type>=`
  summed), `null` when no cap is found. `used` = account-total sreport row;
  when the cluster returns only per-user rows (Devana) and `source` is
  `sreport`, `used` is `null` (not the sum of the rows you can see). `used_30d`
  likewise from `sreport30` account totals, `null` when absent. `mine` = your
  own per-user row in `sreport30`, `null` when absent. Absent sreport rows for
  an account that sshare lists mean 0 usage only when the section itself
  parsed (sreport omits zero rows). *Implemented:* on Devana the 30-day
  report returns no rows at all, so `used_30d` and `mine` are `null` there
  and the panel shows no `you:` line and no days-left estimate.
- `title`: from `perunfsusage` indented title line, else `null`.
- `storage`: the project's quota row (`perunfsusage` Project row) else `null`.
  *Implemented:* also `null` when that row's limit is `TBD`; the row still
  decides active / inactive.
- `quotas`: user quota rows from `myquota` / `perunfsusage` (`kind: "user"`).
  Sizes are parsed from `KB`, `MB`, `GB`, `TB` (powers of 1024) into KiB.
  `limit` `TBD` or missing → row dropped.
- `raw`: the tool's raw text when it was present but its parser returned
  nothing (so the panel can show it), else `null`.

## Storage strip change

`cluster` stays as is. The mod merges: for a strip path that has a `quotas`
entry on the same machine whose `path` equals or is a prefix of it (e.g.
`/mnt/home` for `/mnt/home/ivvyke441`, `/home` for `/home/ivykopal`), the bar
shows the quota % and the label gets `quota`; otherwise filesystem % as
today. Storage toasts use the same merged % (Devana `/home` is at 86 % of
its quota today, so it shows `ansi3` and alerts once it reaches 90 %).

## Mod

- `projects` is polled by the leader every 600 s (first right after the
  first snapshot) and published in `shared("slurm.state").projects`.
  *Implemented:* each result is merged over the last good one (a top-level
  `error` keeps the previous result, a machine entry with `error` keeps its
  previous entry), so a failed poll neither drops quota % from the strip nor
  re-arms quota alerts.
- Per machine, under the cluster strip: a **Projects** block, one row per
  active project: `account` (and `title`, dim, truncated), then two bars
  `CPU used/awarded h` and `GPU used/awarded h` (colors by % like storage:
  < 80 ansi2, 80–89 ansi3, ≥ 90 blocked; a bar without `awarded` shows just
  `used h`), then `you: <mine> GPU h (30 d)` when `mine` present, then a burn
  line `~<N> days left at the 30-day rate` when `awarded`, `used` and
  `used_30d` > 0 are known (N = (awarded − used) / (used_30d / 30), floor; `∞`
  hidden), then the project storage bar when present. *Implemented:* one
  burn line per kind, prefixed `CPU ` or `GPU `
  (`GPU ~325 days left at the 30-day rate`), shown only while hours remain.
- Collapsed with the machine section. Empty state: `no active projects`.
  When `raw` is set for a tool and nothing parsed: a dim `could not read
  <tool>` line with the first 5 raw lines.
- Notifications (leader, with the same arm/re-arm hysteresis as storage:
  fire at ≥ 90 %, re-arm below 85 %): `${machine}: ${account} ${CPU|GPU}
  hours ${pct}% used` level warning; quota alerts reuse the storage notice
  with the merged quota %.

## Error handling

- Any section failing never fails the machine; only ssh failure, timeout or
  a missing end marker set the machine's `error`.
- Parsers ignore header, separator and unknown lines.

## Testing

- Rust unit tests with **anonymised** fixtures captured from both clusters
  (other members' logins and names replaced with `user1`, `User One`, …;
  your own login kept): sshare, assoc, qos (incl. `_gpu` billing), sreport
  with and without account totals, sreport30, sprojects -f, myquota,
  perunfsusage. Plus: `slurm_conf` validation and the `export SLURM_CONF`
  prefix appearing in every script builder; a machine without `slurm_conf`
  produces byte-identical scripts to before.
- JS logic tests: quota/path merge, burn-days, project alert hysteresis,
  bar color keys.
- Live (read-only): `herdr-slurm projects` on both machines; Perun requires
  `slurm_conf` set in the user's `machines.toml` (the implementer adds it,
  with a backup); screenshot of the Projects block.
