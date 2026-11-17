# Slurm projects, compute hours and quotas Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Projects block per cluster in the herdr-app Slurm panel with compute hours used/awarded, burn rate, your share, project storage and real quotas, with 90 % toasts.

**Architecture:** A `slurm_conf` prelude shared by every remote script; a new `herdr-slurm projects` subcommand (one ssh per machine, sections parsed by pure functions in `src/projects.rs` tested on anonymised fixtures); pure JS helpers in `mods/slurm/logic.js`; the leader polls `projects` every 600 s and the panel renders a Projects block and quota-aware storage bars.

**Tech Stack:** Rust 2021 (serde_json, toml, chrono; no new crates), herdr-app mod JS (QuickJS + gpui-kit), Node 24 test runner.

**Spec:** `docs/superpowers/specs/2026-11-17-slurm-projects-design.md`

## Global Constraints

- No new Rust crates; hand-written validation; parsers are pure functions with unit tests.
- One ssh per machine per subcommand call; `projects` cap 60 s per machine; other subcommands keep their caps.
- Every subcommand prints exactly one JSON object with `error`.
- `slurm_conf` charset `[A-Za-z0-9_./~$-]`, non-empty, no `..` segment; leading `~` → `$HOME`; prelude text exactly `export SLURM_CONF="<value>"; ` at the very start of the remote script. Machines without `slurm_conf` produce byte-identical scripts to before.
- 30-day window: `Start=$(date -d '30 days ago' +%F)`; all-time window `Start=2020-01-01`. Never `-30days`.
- Inactive and expired projects are omitted from `projects`.
- Hours are integers (rounded). Sizes in KiB, parsed from `KB/MB/GB/TB` as powers of 1024.
- Thresholds: colors < 80 `ansi2`, 80–89 `ansi3`, ≥ 90 `blocked`; alerts fire at ≥ 90 %, re-arm below 85 %.
- Poll `projects` every 600 s (first right after the first snapshot), leader only.
- Fixtures committed to the repo are anonymised: other members' logins → `user1`, `user2`, …; proper names → `User One`, `User Two`, …; your own logins (`ivvyke441`, `ivykopal`) and names kept. Project titles from `perunfsusage` are kept.
- logic.js has no imports; test with `node --test mods/slurm/logic.test.js`.
- Never submit, cancel or modify cluster jobs; live checks are read-only.

## Review Focus

- Devana `sreport` returns only your own rows: `gpu.used` must be `null` there unless `sprojects` supplies it, never the sum of your rows — Rust test `sreport_without_totals_gives_null_used`.
- `sprojects` present but its format changed: `raw.sprojects` holds the text and the machine still returns accounts — Rust test `unparseable_sprojects_goes_to_raw`.
- Perun expired accounts (`node=0`) must not appear even though sreport lists them — Rust test `expired_accounts_are_omitted`.
- A strip path that is not under any quota path keeps filesystem % — JS test `quota merge keeps fs pct for unmatched path`.
- `/home` quota must not match `/homework` — JS test `quota merge matches path prefix on segment boundary`.

---

### Task 1: `slurm_conf` prelude for every remote script

**Files:**
- Modify: `src/main.rs` (`Machine.slurm_conf: Option<String>`, `parse_machine_entry`, `TEMPLATE` comment, `query_machine`), `src/remote.rs` (`pub fn prelude(m: &Machine) -> String`), `src/jobcmd.rs`, `src/cluster.rs` (each script builder prepends `prelude(m)`)

**Interfaces:**
- Produces: `pub fn prelude(m: &Machine) -> String` returning `""` or `export SLURM_CONF="<value with leading ~ → $HOME>"; `.

- [ ] **Step 1: Failing tests**
  - `slurm_conf_parses_and_validates`: `slurm_conf = "~/slurm-custom/slurm/custom_slurm.conf"` accepted; `"/a/../b"` and `"x;y"` → warning `skipping machine 'X': invalid slurm_conf '<v>'`.
  - `prelude_rewrites_tilde`: → `export SLURM_CONF="$HOME/slurm-custom/slurm/custom_slurm.conf"; `.
  - `scripts_start_with_prelude`: for a machine with `slurm_conf`, the snapshot remote command, `build_job_script`, the `action` command, the `final` script and the `cluster` script all start with the prelude.
  - `scripts_unchanged_without_slurm_conf`: the same builders for a machine without the key equal their current output (assert `!starts_with("export SLURM_CONF")` and spot-check one full script string from an existing test).
- [ ] **Step 2:** `cargo test` — fail.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` green, no warnings.
- [ ] **Step 5:** Commit `feat(cli): optional slurm_conf applied to every remote script`

### Task 2: Pure parsers with anonymised fixtures

**Files:**
- Create: `src/projects.rs` (parsers only, no I/O, no dispatch yet), `tests/fixtures/perun-projects.txt`, `tests/fixtures/devana-projects.txt` (anonymised; source captures are given to the implementer)
- Modify: `src/main.rs` (`mod projects;`)

**Interfaces:**
- Produces (all `pub(crate)`):
  - `parse_accounts(sshare: &str) -> Vec<String>`
  - `parse_expired(assoc: &str) -> HashSet<String>` (GrpTRES contains `node=0`)
  - `parse_awards(qos: &str) -> HashMap<String, Award>`; `Award { cpu_h: Option<u64>, gpu_h: Option<u64>, billing_h: Option<u64> }`; `<acct>_gpu` folds into `<acct>` (billing summed); minutes / 60 rounded.
  - `parse_sreport(text: &str, me: &str) -> HashMap<String, Usage>`; `Usage { cpu: Option<u64>, gpu: Option<u64>, mine_cpu: Option<u64>, mine_gpu: Option<u64>, has_totals: bool }` — totals only from empty-Login rows; `mine_*` from Login == me; `gres/gpu` and `gres/gpu:<type>` both count as gpu.
  - `parse_sprojects(text: &str) -> Option<Vec<SProject>>`; `SProject { account, active: bool, cpu_used, cpu_awarded, gpu_used, gpu_awarded: Option<u64> }`; `None` when the text is non-empty but no project row parsed.
  - `parse_myquota(text: &str) -> Option<Vec<Quota>>`; `parse_perunfsusage(text: &str) -> Option<(Vec<Quota>, Vec<ProjectStorage>)>`; `Quota { kind: "user", name, path, used_kb, limit_kb, pct }`; `ProjectStorage { account, title: Option<String>, active: bool, path, used_kb, limit_kb, pct }`; rows with limit `TBD` dropped.
  - `parse_size_kib(s: &str) -> Option<u64>` (`4.00 KB`, `211 GB`, `1.01 TB`, `863GB`).

- [ ] **Step 1: Fixtures.** From the raw captures, write the two anonymised fixture files keeping the `@@<section>` markers as they are in the captures. Replace every login other than `ivvyke441`/`ivykopal` with `user<N>` and every proper name other than `Ivan Vykopal` with `User <N in words>`, consistently across a file.
- [ ] **Step 2: Failing tests** (fixture sections read via `include_str!("../tests/fixtures/…")` and a small test-only section splitter on `@@<name>` lines):
  - `accounts_from_sshare` (Perun: 6 accounts in order; Devana: 5).
  - `expired_accounts_from_assoc` (Perun: perun250128, perun25012246; Devana: none).
  - `awards_perun_from_qos`: perun2601397 cpu 88474, gpu 7964.
  - `awards_devana_billing_split`: p1605-25-3 billing 5000 + 320000 = 325000, cpu/gpu None.
  - `sreport_totals_and_mine_perun`: perun2601397 has totals; `mine_gpu` equals the ivvyke441 row.
  - `sreport_without_totals_gives_null_used`: Devana p1605-25-3 `has_totals == false`, `cpu == None`, `mine_cpu == Some(11745)`.
  - `sprojects_active_and_inactive`: p1605-25-3 active cpu 1947/5000 gpu 7090/20000; p904-24-3 inactive.
  - `unparseable_sprojects_returns_none`: `"garbage\nlines"` → None; `""` → `Some(vec![])`.
  - `myquota_user_home`: path `/home`, pct 86, used 863 GiB in KiB, limit 1000 GiB in KiB.
  - `perunfsusage_rows`: user `/mnt/home` 42 (42.1 rounded), scratch row dropped (TBD), project perun2601475 pct 98 (97.8 rounded) with title "Vision-Language Model Adaptation to the Slovak Language", perun25012246 inactive.
  - `size_units`: `4.00 KB`→4, `211 GB`→211·1024², `1.01 TB`→round(1.01·1024³), `863GB`→863·1024².
- [ ] **Step 3:** `cargo test projects` — fail.
- [ ] **Step 4:** Implement the parsers.
- [ ] **Step 5:** `cargo test` green, no warnings (allow dead_code only on items Task 3 will use).
- [ ] **Step 6:** Commit `feat(cli): parsers for project accounts, hours and quotas`

### Task 3: `herdr-slurm projects`

**Files:**
- Modify: `src/projects.rs` (script builder, `machine_json`, `cmd_projects`), `src/main.rs` (dispatch `Some("projects")`)
- Modify (user config, not repo): the plugin config `machines.toml` — add `slurm_conf = "~/slurm-custom/slurm/custom_slurm.conf"` to the Perun block (back up first).

**Interfaces:**
- Consumes: Task 1 `prelude`, Task 2 parsers, `remote::{run_remote, sections, load_parsed, print_json}`.
- Produces: the spec's `projects` JSON. `fn build_projects_script(m: &Machine) -> String`; `fn machine_json(m: &Machine, secs: &HashMap<String,String>) -> serde_json::Value`.

- [ ] **Step 1: Failing tests**
  - `script_has_sections_and_30d_date`: contains `@@herdr:sshare@@` … `@@herdr:end@@`, `Start=2020-01-01`, `date -d '30 days ago' +%F`, no `-30days`, `_gpu` built remotely, prelude first when configured.
  - `expired_accounts_are_omitted` (Perun fixture → no perun250128 / perun25012246).
  - `perun_project_json`: perun2601397 `source: "sreport"`, gpu awarded 7964, title "Slovak LLM Benchmarking", storage pct 64 (63.7 rounded).
  - `devana_project_json`: `source: "sprojects"`, only p1605-25-3 and p1626-25-3, gpu used 7090 awarded 20000, `mine` from sreport30 or null.
  - `unparseable_sprojects_goes_to_raw`: sprojects section replaced by garbage → `raw.sprojects` holds it, projects fall back to sreport/QOS (used null for Devana).
  - `quotas_listed`: Devana `/home` 86; Perun `/mnt/home` 42.
  - `pbs_machine_unsupported`.
- [ ] **Step 2:** `cargo test projects` — fail.
- [ ] **Step 3:** Implement; 60 s cap; parallel over machines like `cmd_cluster`.
- [ ] **Step 4:** `cargo test` green; add `slurm_conf` to Perun in the user's `machines.toml` (backup beside it); `cargo run --release -- projects` shows both machines with `error: null`, Perun projects perun2601397/2601474/2601475/2601476, Devana p1605-25-3 and p1626-25-3. Also `cargo run --release -- snapshot` still works for Perun with the prelude.
- [ ] **Step 5:** Commit `feat(cli): projects subcommand (hours, awards, burn data, quotas)`

### Task 4: JS logic helpers

**Files:**
- Modify: `mods/slurm/logic.js`, `mods/slurm/logic.test.js`

**Interfaces:**
- Produces:
  - `quotaFor(machineProjects, path) -> {pct, quota:true} | null` — longest `quotas[].path` that equals `path` or is a prefix ending at a `/` boundary.
  - `mergedStoragePct(entry, machineProjects) -> {pct, quota: boolean}` — quota pct when matched, else `storagePct(entry)`.
  - `burnDays(h) -> number|null` — `h = {used, awarded, used_30d}`; null unless all present, `awarded > used`, `used_30d > 0`; `Math.floor((awarded - used) / (used_30d / 30))`.
  - `hoursPct(h) -> number|null` — `round(used / awarded * 100)` or null.
  - `projectAlerts(prevArmed, projects) -> {armed, alerts:[{machine, account, kind:"CPU"|"GPU", pct}]}` — key `${machine}:${account}:${kind}`, same hysteresis as `storageAlerts`.
  - `projectNotice(alert) -> {title, body, level:"warning"}` — title `${machine}: ${account} ${kind} hours ${pct}% used`, body `Ask the PI for more hours or slow down.`
- [ ] **Step 1: Failing tests:** `quota merge matches path prefix on segment boundary`, `quota merge keeps fs pct for unmatched path`, `burnDays basic`, `burnDays null without 30d usage`, `hoursPct null without award`, `project alert fires at 90 once and re-arms below 85`, `projectNotice text`.
- [ ] **Step 2:** Run — fail. **Step 3:** Implement. **Step 4:** Run — pass.
- [ ] **Step 5:** Commit `feat(mod): logic for quota merge, burn rate and project alerts`

### Task 5: Mod wiring and Projects block

**Files:**
- Create: `mods/slurm/projects.js` (`export function projectsBlock(t, machineProjects, now)`)
- Modify: `mods/slurm/main.js` (leader polls `projects` every 600 s, publishes `state.projects`, adopts/keeps `projArmed` like `armed`, project and quota notifications), `mods/slurm/strip.js` (bars use `mergedStoragePct`, label suffix ` quota` when quota), `logic.js` `storageAlerts` callers pass merged pct (extend signature only if needed, with a test)

**Interfaces:**
- Consumes: Task 3 JSON, Task 4 helpers, existing lease/state code.

- [ ] **Step 1:** Leader poll + publish + notifications per spec; `checking()` guard kept; exec timeout 120000.
- [ ] **Step 2:** Projects block per spec (rows, bars, `you:` line, burn line, storage bar, empty state, `could not read <tool>` with first 5 raw lines); hidden when the machine is collapsed.
- [ ] **Step 3:** Strip shows quota % where matched.
- [ ] **Step 4: Verify:** `node --test mods/slurm/logic.test.js`; `timeout 60 herdr-app check mods/slurm`; copy changed files to `~/.config/herdr-app/mods/slurm` (main.js last); `herdr-app status` has no mod errors; open the Slurm panel, wait until `shared.json` has `slurm.state.projects`, `herdr-app shot`, inspect: Projects block with bars for both machines, Devana `/home` bar shows quota 86 %.
- [ ] **Step 5:** Commit `feat(mod): projects block, quota-aware storage, hour alerts`

### Task 6: Docs

**Files:**
- Modify: `README.md` (`projects` in the subcommand table; `slurm_conf` key), `RECIPE.md` (Projects block, quota merge, alerts, `slurm_conf`)
- [ ] **Step 1:** Write from the real files.
- [ ] **Step 2:** `cargo test && node --test mods/slurm/logic.test.js && timeout 60 herdr-app check mods/slurm`.
- [ ] **Step 3:** Commit `docs: slurm projects, hours and quotas`
