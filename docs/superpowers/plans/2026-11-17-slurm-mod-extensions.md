# Slurm mod extensions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Job logs, job actions, end-of-job notifications, GPU/storage headroom and GPU under-use in the herdr-app `slurm` mod.

**Architecture:** Four new JSON subcommands in the `herdr-slurm` binary (`job`, `action`, `final`, `cluster`) in new modules beside `src/main.rs`; the mod gets a pure `logic.js` (tested with `node --test`) and a leader copy that polls, publishes `shared("slurm.state")` and notifies.

**Tech Stack:** Rust 2021 (serde_json, toml, chrono; no new crates), herdr-app mod JS (gpui-kit, `herdr` module), Node 24 test runner.

**Spec:** `docs/superpowers/specs/2026-11-17-slurm-mod-extensions-design.md`

## Global Constraints

- TUI pane, `open`/`refresh`/`start`/`stop`/`daemon`, sidebar tokens and `herdr-plugin.toml` are not modified.
- `snapshot` JSON keeps every existing field; it only gains `ssh_target` per machine.
- One `ssh` per machine per subcommand call; `BatchMode=yes`, `ConnectTimeout=8`; 30 s cap per machine (`run_command_with_timeout`).
- Every subcommand prints exactly one JSON object with `error` (`null` on success); malformed arguments exit 2.
- Slurm job id `^[0-9]+(_[0-9]+)?$`; PBS job id `^[0-9]+(\[[0-9]*\])?(\.[A-Za-z0-9.-]+)?$`; storage entry `^[A-Za-z0-9_./~$-]+$` without `..`.
- Log tail default 50, clamp 1..=500. GPU busy threshold: util ≥ 10. Storage colors: < 80 `ansi2`, 80–89 `ansi3`, ≥ 90 `blocked`; notify at ≥ 90, re-arm below 85.
- Lease stale after 60 s; snapshot every 20 s; cluster every 120 s.
- Agent default `claude` (prop `agent` overrides).
- No new Rust dependencies; regex-free validation (hand-written char checks, like `parse_machine_entry`).

## Review Focus

- A job that ends while its machine is unreachable must not notify as ended: `logic.test.js` `unreachable machine keeps running map`.
- Perun has no `sacct`: a job that just ended must still resolve via `scontrol`: Rust test `final_prefers_scontrol_state`.
- Array job ids (`123_4`) must pass validation and reach `scancel` intact: Rust test `slurm_job_id_accepts_array_task`.
- Log paths with no StdErr line or StdErr == StdOut must not show a duplicate tab: Rust test `job_err_tail_empty_when_same_file`.
- Two watcher copies (two layouts, hot reload) must not double-notify: `logic.test.js` `fresh lease held by other copy blocks polling`.

---

### Task 1: Remote helpers, `storage` key, `ssh_target`

**Files:**
- Create: `src/remote.rs`
- Modify: `src/main.rs` (`mod remote;`, `Machine` gains `storage: Vec<String>`, `parse_machine_entry`, `machine_snapshot`, `TEMPLATE` comment line for `storage`, `fn main` dispatch later tasks use)
- Test: `src/remote.rs` `#[cfg(test)] mod tests`, plus `src/main.rs` tests

**Interfaces:**
- Produces:
  - `pub fn valid_job_id(s: &Scheduler, id: &str) -> bool`
  - `pub fn tool(m: &Machine, name: &str) -> String` — sibling of `scheduler_command` when it contains `/`, else `name`.
  - `pub fn run_remote(m: &Machine, script: &str, timeout: Duration) -> Result<RemoteOut, String>`; `pub struct RemoteOut { pub stdout: String, pub stderr: String, pub ok: bool }`. `Err` only when ssh cannot run / times out.
  - `pub fn sections(text: &str) -> HashMap<String, String>` — splits on lines `@@herdr:<name>@@`.
  - `pub fn find_machine<'a>(ms: &'a [Machine], name: &str) -> Option<&'a Machine>`
  - `pub fn load_parsed() -> Result<ParsedMachines, String>` — the read/parse half of `cmd_snapshot`, reused by it.
  - `pub fn print_json(v: serde_json::Value)`
  - Snapshot machine object gains `"ssh_target": m.ssh_target()`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn slurm_job_id_accepts_array_task() { assert!(valid_job_id(&Scheduler::Slurm, "123_4")); assert!(valid_job_id(&Scheduler::Slurm, "123")); }
#[test] fn slurm_job_id_rejects_shell() { for bad in ["", "12;rm", "12 3", "-1", "12_", "_1"] { assert!(!valid_job_id(&Scheduler::Slurm, bad), "{bad}"); } }
#[test] fn pbs_job_id_accepts_server_and_array() { assert!(valid_job_id(&Scheduler::Pbs, "4021.pbs01")); assert!(valid_job_id(&Scheduler::Pbs, "4021[].pbs01")); assert!(!valid_job_id(&Scheduler::Pbs, "4021.pbs;x")); }
#[test] fn tool_uses_scheduler_command_dir() { /* scheduler_command "/opt/slurm/bin/squeue" -> tool(m,"scancel") == "/opt/slurm/bin/scancel"; "squeue" -> "scancel" */ }
#[test] fn sections_split_on_markers() { let s = sections("@@herdr:a@@\nx\ny\n@@herdr:b@@\nz\n"); assert_eq!(s["a"], "x\ny"); assert_eq!(s["b"], "z"); }
#[test] fn storage_key_parses_and_rejects_dotdot() { /* storage = ["$HOME","/mnt/scratch"] -> machine.storage len 2; storage = ["/a/../b"] -> warning "skipping machine 'X': invalid storage path '/a/../b'" */ }
```

- [ ] **Step 2: Run** `cargo test` — Expected: new tests fail to compile (missing items).
- [ ] **Step 3: Implement** the items above; `cmd_snapshot` calls `load_parsed`; missing `storage` key gives `vec![]`.
- [ ] **Step 4: Run** `cargo test` — Expected: all pass (29 old + new).
- [ ] **Step 5: Commit** `feat(cli): remote helpers, storage config, ssh_target in snapshot`

### Task 2: `job` subcommand

**Files:**
- Create: `src/jobcmd.rs`
- Modify: `src/main.rs` (`mod jobcmd;`, `Some("job") => jobcmd::cmd_job()`)

**Interfaces:**
- Consumes: Task 1 helpers.
- Produces: `herdr-slurm job <machine> <id> [--lines N] [--gpu]` with the spec JSON. Internal: `fn parse_scontrol_kv(line: &str) -> HashMap<String,String>` (tokens split on single spaces, key/value at the first `=`; values containing spaces are not supported), `fn parse_alloc_tres(s: &str) -> (Option<u64>, Option<String>, u64)` (cpus, mem, gpus summed over `gres/gpu=` and `gres/gpu:<type>=`), `fn parse_gpu_lines(text: &str) -> Vec<GpuUse>` for lines `node,index,util,mem_used,mem_total` (the spec's host-prefixed `nvidia-smi` output), `fn build_job_script(m: &Machine, id: &str, lines: u32, gpu: bool) -> String`.

- [ ] **Step 1: Write failing tests** — fixtures copied from the spec examples:
  - `scontrol_kv_reads_paths_and_tres`: line `JobId=123 JobState=RUNNING StdOut=/h/u/o.out StdErr=/h/u/o.out NodeList=n141 StartTime=2026-11-17T10:02:11 AllocTRES=cpu=16,mem=64G,node=1,billing=16,gres/gpu=4` → StdOut `/h/u/o.out`, AllocTRES parsed `(Some(16), Some("64G"), 4)`.
  - `alloc_tres_sums_typed_gpus`: `cpu=8,gres/gpu:a100=2,gres/gpu:v100=1` → gpus 3.
  - `gpu_lines_parse_node_prefix`: `n141,0, 97, 30000, 40960` → `GpuUse{node:"n141",index:0,util:97,mem_used_mib:30000,mem_total_mib:40960}`; junk lines skipped.
  - `job_err_tail_empty_when_same_file`: `job_json` built from sections where StdErr == StdOut → `err_tail == []`.
  - `job_script_rejects_nothing_unvalidated`: `build_job_script` output contains the id only in positions produced from the validated id (assert the script contains `scontrol show job -o 123` and `tail -n 50`).
  - `lines_clamped`: `--lines 0` → 1, `--lines 9999` → 500.
- [ ] **Step 2: Run** `cargo test jobcmd` — fail.
- [ ] **Step 3: Implement.** Remote script (Slurm), sections `scontrol`, `out`, `err`, `sstat`, `gpu`; the shell reads StdOut/StdErr from the scontrol line with `sed -n 's/.* StdOut=\([^ ]*\).*/\1/p'`; `tail -n N "$o" 2>/dev/null || echo "(cannot read $o)"`; err section printed only when `$e != $o`; GPU section only when `--gpu` and JobState=RUNNING, wrapped in `timeout 25`, stderr of that command appended into section `gpuerr`. PBS script: `qstat -f -F json <id>`, paths parsed with serde_json from `Jobs.<id>.Output_Path`, then a second ssh call for the tails is NOT allowed — instead the PBS script extracts paths with `sed` on `qstat -f <id>` (`Output_Path = host:/path`) and tails in the same call.
- [ ] **Step 4: Run** `cargo test` — pass. Then live: `cargo run --release -- job Devana <a running id or a recent id>`; expected JSON with `error: null` or a scheduler error string, never a panic.
- [ ] **Step 5: Commit** `feat(cli): job subcommand with log tails, sstat and GPU use`

### Task 3: `action` and `final`

**Files:**
- Modify: `src/jobcmd.rs`, `src/main.rs` dispatch (`action`, `final`)

**Interfaces:**
- Produces: spec JSON for both. `fn action_command(m: &Machine, id: &str, verb: &str) -> Option<String>`; `fn normalize_final_state(s: &str) -> String` (first whitespace token, uppercased); `fn parse_final(scontrol: &HashMap<String,String /*id->line*/>, sacct: &str, ids: &[String]) -> Vec<FinalJob>`.

- [ ] **Step 1: Write failing tests**
  - `action_maps_verbs_slurm`: cancel→`scancel 123`, requeue→`scontrol requeue 123`, hold→`scontrol hold 123`, release→`scontrol release 123`; `"rm"` → `None`.
  - `action_maps_verbs_pbs`: `qdel`, `qrerun`, `qhold`, `qrls`.
  - `final_prefers_scontrol_state`: scontrol line `JobId=7 JobState=OUT_OF_MEMORY ExitCode=0:125` and sacct text `7|FAILED|1:0` → state `OUT_OF_MEMORY`, exit `0:125`.
  - `final_falls_back_to_sacct_then_unknown`: no scontrol for 8, sacct `8|CANCELLED by 42|0:15` → `CANCELLED`; id 9 nowhere → `UNKNOWN`.
  - `final_pbs_exit_status`: `job_state F, Exit_status 0` → COMPLETED; `Exit_status 271` → FAILED.
- [ ] **Step 2: Run** `cargo test` — fail.
- [ ] **Step 3: Implement.** `final` remote script: for each id `echo "@@herdr:sc:<id>@@"; scontrol show job -o <id> 2>/dev/null`; then `@@herdr:sacct@@` and one `sacct ... 2>/dev/null` for all ids (empty output on Perun is fine).
- [ ] **Step 4: Run** `cargo test`; live `cargo run --release -- final Perun 1` → `{"error":null,"jobs":[{"id":"1","state":"UNKNOWN",...}]}`.
- [ ] **Step 5: Commit** `feat(cli): action and final subcommands`

### Task 4: `cluster` subcommand

**Files:**
- Create: `src/cluster.rs`
- Modify: `src/main.rs` (`mod cluster;`, dispatch)

**Interfaces:**
- Produces: spec JSON. `fn aggregate_gpus(sinfo: &str) -> Vec<PartitionGpus>`; `fn parse_gres(s: &str) -> Vec<(String /*type*/, u64)>` (strips `(S:..)`/`(IDX:..)`, ignores non-`gpu` gres); `fn parse_df(line: &str) -> Option<(u64,u64,u8)>` (size_kb, used_kb, pct); `fn parse_lfs_quota(line: &str) -> Option<Quota>` (`None` when limit 0); `fn parse_pending(text: &str) -> Vec<Pending>`.

- [ ] **Step 1: Write failing tests**
  - `gpus_typed_devana`: lines `n141|gpu|gpu:a100-sxm4:4|gpu:a100-sxm4:1(IDX:1)|mix-|` + `n142|gpu|gpu:a100-sxm4:4|gpu:a100-sxm4:4(IDX:0-3)|mix-|` → `{name:"gpu", gpu_type:"a100-sxm4", total:8, used:5, free:3}`.
  - `gpus_untyped_perun`: `gpu03|gpu_short|gpu:8|gpu:6|mix-|` → type `""`, total 8, used 6, free 2.
  - `drained_node_not_free`: `n150|gpu|gpu:a100-sxm4:4|gpu:a100-sxm4:0(IDX:N/A)|drain|` → total 4, used 0, free 0.
  - `cpu_partitions_omitted`: `n001|short|msrsafe:1,sysfs:1|msrsafe:0|idle|` → empty.
  - `df_line_parses`: `127.0.0.1:/ 287955146752 128954814464 159000332288 45% /scratch` → `(287955146752, 128954814464, 45)`.
  - `lfs_quota_zero_limit_is_none`: `   /mnt/scratch       4       0       0       -       1       0       0       -` → `None`.
  - `lfs_quota_with_limit_and_star`: `/project 950000000* 900000000 1000000000 - 1 0 0 -` → `used 950000000, limit 1000000000 (hard), pct 95`.
  - `pending_na_is_null`: `124|N/A\n125|2026-11-17T14:00:00` → `[{124,None},{125,Some(..)}]`.
- [ ] **Step 2: Run** `cargo test cluster` — fail.
- [ ] **Step 3: Implement.** Remote script sections `sinfo`, `pending`, then per storage path `@@herdr:st:<n>@@` with `p=<path>; [ -d "$p" ] || continue; echo "$p"; stat -f -c %T "$p"; df -Pk "$p" | tail -1; [ "$(stat -f -c %T "$p")" = lustre ] && lfs quota -q -u <user> "$p" 2>/dev/null | head -1`. Paths are inserted unquoted-safe because Task 1 validated them; `$HOME`/`$USER` expand remotely. Default list: `$HOME /scratch /mnt/scratch /project`. All machines in parallel with `std::thread::scope` like `cmd_snapshot`.
- [ ] **Step 4: Run** `cargo test`; live `cargo run --release -- cluster` → both machines, Devana `gpu` partition with `gpu_type:"a100-sxm4"`, Perun storage contains `/mnt/scratch` with `fs:"lustre"`.
- [ ] **Step 5: Commit** `feat(cli): cluster subcommand (GPU headroom, pending start, storage)`

### Task 5: `logic.js` pure functions

**Files:**
- Create: `mods/slurm/logic.js`, `mods/slurm/logic.test.js` (Herdr ignores neither: the test file must not import `herdr`; it only imports `./logic.js` and `node:test`)

**Interfaces:**
- Produces (named exports):
  - `endedJobs(prevRunning: Record<string,{machine,id,name}>|null, snapshot) -> { running, ended: Array<{machine,id,name}> }` — `prevRunning === null` seeds only; machine with `error` keeps its previous entries.
  - `notifyLevel(state: string) -> "success"|"error"|"warning"`
  - `gpuUnderuse(allocGpus: number, gpus: Array<{util}>) -> string|null` → `"2 of 4 GPUs busy"`.
  - `storageColorKey(pct: number) -> "ansi2"|"ansi3"|"blocked"`; `storagePct(entry) -> number` (max of fs and quota).
  - `storageAlerts(prevArmed: Record<string,boolean>, cluster) -> { armed, alerts: Array<{machine,path,pct}> }`.
  - `formatStart(iso: string|null, now: Date) -> string` → `"14:00"` today, `"Tue 14:00"` otherwise, `""` for null.
  - `shouldLead(lease: {copy,at}|undefined, me: string, now: number, isWatcher: boolean) -> boolean` — true when no lease, lease is mine, lease older than 60000 ms; a non-watcher also defers to any fresh lease.

- [ ] **Step 1: Write failing tests** (`node:test` + `node:assert/strict`), one per Review Focus line plus: `first snapshot seeds without ended`, `job leaving RUNNING for COMPLETING is not ended`, `job missing from snapshot is ended`, `unreachable machine keeps running map`, `notifyLevel OUT_OF_MEMORY is error`, `gpuUnderuse 4 alloc 1 busy`, `gpuUnderuse all busy is null`, `storage alert fires at 90 once and re-arms below 85`, `formatStart today vs other day`, `fresh lease held by other copy blocks polling`, `stale lease is taken`.
- [ ] **Step 2: Run** `node --test mods/slurm/` — fail (module missing).
- [ ] **Step 3: Implement** `logic.js`.
- [ ] **Step 4: Run** `node --test mods/slurm/` — all pass.
- [ ] **Step 5: Commit** `feat(mod): slurm logic module with tests`

### Task 6: Leader, shared state, notifications, watcher block

**Files:**
- Modify: `mods/slurm/main.js`
- Modify (user config, not repo): `~/.config/herdr-app/layouts/main-quota.json` — add after `slack-bg`: `{ "id": "slurm-bg", "type": "block", "size": { "px": 0 }, "content": { "mod": "slurm", "props": { "watch": true } } }`

**Interfaces:**
- Consumes: Task 5 exports; CLI `snapshot`, `cluster`, `final`.
- Produces: `shared("slurm.state") = { snapshot, cluster, updated_at }`, `shared("slurm.leader") = { copy, at }`, `shared("slurm.refresh") = <ms>`. Copy id: `Math.random().toString(36).slice(2)` at `init`.

- [ ] **Step 1:** In `init`: if `checking()` return before timers. Every 5 s tick: decide `shouldLead`; the leader polls `snapshot` when 20 s passed or `slurm.refresh` is newer than its last poll, `cluster` when 120 s passed; it writes lease and state. Ended jobs → one `final` per machine → `notify({title, body, level})`, `playSound("blocked")` for `error`. Storage alerts → `notify` level `warning`. Watcher `render` returns `div()`.
- [ ] **Step 2:** `↻ Refresh` sets `slurm.refresh`; non-leader copies render from `shared("slurm.state")`.
- [ ] **Step 3: Verify** `herdr-app check mods/slurm` passes; copy the mod to `~/.config/herdr-app/mods/slurm` (that is how the RECIPE installs it), add the layout block, wait 30 s, `jq '."slurm.leader", ."slurm.state".updated_at' ~/.config/herdr-app/shared.json` shows a fresh `at`. Notification smoke: temporarily run `notify` path via a stub snapshot is not possible live; rely on Task 5 tests plus one real end-to-end: submit `sbatch --wrap "sleep 30; exit 1" -t 1` on Devana and observe an error toast within ~60 s (screenshot).
- [ ] **Step 4: Commit** `feat(mod): single poller lease, shared state, job-end and storage notifications`

### Task 7: Expanded row, actions, terminal/agent, cluster strip

**Files:**
- Modify: `mods/slurm/main.js` (split rendering into `mods/slurm/detail.js` for the expanded block and `mods/slurm/strip.js` for the cluster strip; `main.js` keeps polling and layout)

**Interfaces:**
- Consumes: CLI `job`, `action`; Task 5 `gpuUnderuse`, `storageColorKey`, `storagePct`, `formatStart`; snapshot `ssh_target`.
- Produces: `export function detailBlock(t, ctx)` and `export function clusterStrip(t, machineCluster)` returning elements.

- [ ] **Step 1:** Row click toggles `this.open[machine] = id`; open block polls `job --lines 50` every 20 s (and `--gpu` once per click of `GPU usage`). Action buttons per spec; Cancel/Requeue show inline confirm row; result error goes to `notify` level `error`, success triggers `slurm.refresh`.
- [ ] **Step 2:** `Terminal on node` and `Open log in agent` via `run("tab.new", …)` exactly as in the spec; agent from `props()?.agent ?? "claude"`.
- [ ] **Step 3:** Cluster strip and pending `starts ~…` per spec.
- [ ] **Step 4: Verify** `herdr-app check mods/slurm`; copy to app; screenshot with an expanded row (any job on Devana or Perun; if none run, submit `sbatch --wrap "for i in $(seq 60); do echo line $i; sleep 5; done" -t 10` on Devana and cancel it afterwards with the panel's Cancel button — that exercises `action`).
- [ ] **Step 5: Commit** `feat(mod): job detail, actions, node terminal, agent log, cluster strip`

### Task 8: Docs

**Files:**
- Modify: `RECIPE.md` (What you see / How it behaves / Built from / Watch out / Done when for all new behaviour, the watcher block, `storage` key), `README.md` (new subcommands table, `storage` key)

- [ ] **Step 1:** Update both from the real files.
- [ ] **Step 2:** `cargo test && node --test mods/slurm/ && herdr-app check mods/slurm` all green.
- [ ] **Step 3: Commit** `docs: slurm mod job detail, actions, notifications, headroom`
