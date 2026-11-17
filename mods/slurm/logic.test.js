import test from "node:test";
import assert from "node:assert/strict";
import {
  endedJobs, notifyLevel, gpuUnderuse, storageColorKey, storagePct,
  storageAlerts, formatStart, shouldLead, jobNotice, storageNotice, adoptState, mustAdopt,
  jobButtons, confirmText, factsLine, gpuChip, partitionLabel, partitionColorKey, storageLabel,
  pendingStart, logTabs, logLines, agentPrompt, endedSummary,
  projectBars, mineLine, mergedStorageLabel, rawFallbacks,
} from "./logic.js";

const job = (id, state, name = "j" + id) => ({ id, name, state });
const snap = (machines) => ({ error: null, machines });
const m = (name, jobs, error = null) => ({ name, error, jobs });

test("first snapshot seeds without ended", () => {
  const r = endedJobs(null, snap([m("a", [job("1", "RUNNING"), job("2", "PENDING")])]));
  assert.deepEqual(r.ended, []);
  assert.deepEqual(r.running, { "a/1": { machine: "a", id: "1", name: "j1" } });
});

test("job leaving RUNNING for COMPLETING is not ended", () => {
  const prev = endedJobs(null, snap([m("a", [job("1", "RUNNING")])])).running;
  const r = endedJobs(prev, snap([m("a", [job("1", "COMPLETING")])]));
  assert.deepEqual(r.ended, []);
  assert.ok(r.running["a/1"]);
});

test("job missing from snapshot is ended", () => {
  const prev = endedJobs(null, snap([m("a", [job("1", "RUNNING")])])).running;
  const r = endedJobs(prev, snap([m("a", [])]));
  assert.deepEqual(r.ended, [{ machine: "a", id: "1", name: "j1" }]);
  assert.deepEqual(r.running, {});
});

test("job in non-running state is ended", () => {
  const prev = endedJobs(null, snap([m("a", [job("1", "RUNNING")])])).running;
  const r = endedJobs(prev, snap([m("a", [job("1", "FAILED")])]));
  assert.equal(r.ended.length, 1);
});

test("unreachable machine keeps running map", () => {
  const prev = endedJobs(null, snap([m("a", [job("1", "RUNNING")]), m("b", [job("2", "RUNNING")])])).running;
  const r = endedJobs(prev, snap([m("a", [], "ssh failed")]));
  assert.deepEqual(r.ended, []);
  assert.deepEqual(r.running, prev);
});

test("errored snapshot returns prev unchanged", () => {
  const prev = { "a/1": { machine: "a", id: "1", name: "x" } };
  const r = endedJobs(prev, { error: "boom", machines: [] });
  assert.deepEqual(r, { running: prev, ended: [] });
});

test("notifyLevel OUT_OF_MEMORY is error", () => {
  assert.equal(notifyLevel("OUT_OF_MEMORY"), "error");
  assert.equal(notifyLevel("CANCELLED by 123"), "error");
  assert.equal(notifyLevel("completed"), "success");
});

test("notifyLevel UNKNOWN is warning", () => {
  assert.equal(notifyLevel("UNKNOWN"), "warning");
});

test("gpuUnderuse 4 alloc 1 busy", () => {
  assert.equal(gpuUnderuse(4, [{ util: 50 }, { util: 9 }, { util: 0 }, { util: 0 }]), "1 of 4 GPUs busy");
});

test("gpuUnderuse all busy is null", () => {
  assert.equal(gpuUnderuse(2, [{ util: 10 }, { util: 99 }]), null);
  assert.equal(gpuUnderuse(0, [{ util: 0 }]), null);
  assert.equal(gpuUnderuse(2, []), null);
});

test("storage pct and color", () => {
  assert.equal(storagePct({ pct: 50, quota: { pct: 91 } }), 91);
  assert.equal(storagePct({ pct: 50, quota: null }), 50);
  assert.equal(storageColorKey(79), "ansi2");
  assert.equal(storageColorKey(80), "ansi3");
  assert.equal(storageColorKey(89), "ansi3");
  assert.equal(storageColorKey(90), "blocked");
});

test("storage alert fires at 90 once and re-arms below 85", () => {
  const c = (pct) => ({ error: null, machines: [{ name: "a", error: null, storage: [{ path: "/s", pct, quota: null }] }] });
  let r = storageAlerts({}, c(90));
  assert.deepEqual(r.alerts, [{ machine: "a", path: "/s", pct: 90 }]);
  const armed1 = r.armed;
  r = storageAlerts(armed1, c(95));
  assert.deepEqual(r.alerts, []);
  r = storageAlerts(r.armed, c(86));
  assert.equal(r.armed["a:/s"], false);
  r = storageAlerts(r.armed, c(84));
  assert.equal(r.armed["a:/s"], true);
  r = storageAlerts(r.armed, c(92));
  assert.equal(r.alerts.length, 1);
  assert.equal(armed1["a:/s"], false);
  const kept = storageAlerts({ "a:/s": false }, { error: null, machines: [{ name: "a", error: "x", storage: [] }] });
  assert.deepEqual(kept.armed, { "a:/s": false });
});

test("storage alert uses the quota % when projects are given", () => {
  const cl = { error: null, machines: [{ name: "d", error: null, storage: [{ path: "/home/me", pct: 15, quota: null }] }] };
  const pr = { machines: [{ name: "d", error: null, quotas: [{ path: "/home", pct: 92 }] }] };
  assert.deepEqual(storageAlerts({}, cl, pr).alerts, [{ machine: "d", path: "/home/me", pct: 92 }]);
  assert.deepEqual(storageAlerts({}, cl).alerts, []);
  assert.deepEqual(storageAlerts({}, cl, { machines: [] }).alerts, []);
});

test("project row texts", () => {
  const p = { cpu: { used: 8154, awarded: 88474, used_30d: 712, mine: 712 },
    gpu: { used: 5253, awarded: 7964, used_30d: 446, mine: 446 } };
  assert.deepEqual(projectBars(p), [
    { kind: "CPU", label: "CPU 8154/88474 h", pct: 9, burn: "~3384 days left at the 30-day rate" },
    { kind: "GPU", label: "GPU 5253/7964 h", pct: 66, burn: "~182 days left at the 30-day rate" },
  ]);
  assert.deepEqual(projectBars({ cpu: { used: 5, awarded: null }, gpu: { used: null, awarded: 10 } }), [
    { kind: "CPU", label: "CPU 5 h", pct: null, burn: null },
    { kind: "GPU", label: "GPU ?/10 h", pct: null, burn: null },
  ]);
  assert.deepEqual(projectBars({ cpu: null, gpu: { used: null, awarded: null } }), []);
  assert.equal(mineLine(p), "you: 446 GPU h (30 d)");
  assert.equal(mineLine({ gpu: { mine: null } }), null);
  assert.equal(mergedStorageLabel("/home", { pct: 86, quota: true }), "/home 86% quota");
  assert.equal(mergedStorageLabel("/scratch", { pct: 4, quota: false }), "/scratch 4%");
});

test("rawFallbacks lists unparsed tools with first 5 lines", () => {
  const mp = { raw: { sprojects: "a\n\nb\nc\nd\ne\nf", myquota: null, perunfsusage: "" } };
  assert.deepEqual(rawFallbacks(mp), [{ title: "could not read sprojects", lines: ["a", "b", "c", "d", "e"] }]);
  assert.deepEqual(rawFallbacks(null), []);
});

test("formatStart today vs other day", () => {
  const now = new Date(2026, 10, 17, 9, 0); // Tue
  assert.equal(formatStart("2026-11-17T14:05:00", now), "14:05");
  assert.equal(formatStart("2026-11-18T04:00:00", now), "Wed 04:00");
  assert.equal(formatStart("garbage", now), "");
});

test("formatStart null is empty", () => {
  assert.equal(formatStart(null, new Date()), "");
  assert.equal(formatStart("", new Date()), "");
});

test("fresh lease held by other copy blocks polling", () => {
  assert.equal(shouldLead({ copy: "x", at: 1000, watch: false }, "me", 2000, false), false);
  assert.equal(shouldLead({ copy: "x", at: 1000, watch: true }, "me", 2000, true), false);
});

test("watcher takes fresh lease from panel copy", () => {
  assert.equal(shouldLead({ copy: "x", at: 1000, watch: false }, "me", 2000, true), true);
});

test("stale lease is taken", () => {
  assert.equal(shouldLead({ copy: "x", at: 1000, watch: true }, "me", 61001, false), true);
  assert.equal(shouldLead(undefined, "me", 0, false), true);
  assert.equal(shouldLead({ copy: "me", at: 0, watch: false }, "me", 1, false), true);
});

test("jobNotice uses final state and exit code", () => {
  const n = jobNotice({ machine: "devana", id: "7", name: "train" }, { id: "7", state: "FAILED", exit_code: 1 });
  assert.deepEqual(n, { title: "devana: job 7 FAILED", body: "train · exit 1", level: "error" });
});

test("jobNotice without final is UNKNOWN warning", () => {
  const n = jobNotice({ machine: "a", id: "3", name: "x" }, null);
  assert.deepEqual(n, { title: "a: job 3 UNKNOWN", body: "x · exit ?", level: "warning" });
});

test("jobNotice keeps exit 0", () => {
  const n = jobNotice({ machine: "a", id: "3", name: "x" }, { id: "3", state: "COMPLETED", exit_code: 0 });
  assert.equal(n.body, "x · exit 0");
  assert.equal(n.level, "success");
});

test("storageNotice text", () => {
  assert.deepEqual(storageNotice({ machine: "a", path: "/scratch", pct: 93 }),
    { title: "a: /scratch 93% full", body: "Free space before jobs fail.", level: "warning" });
});

test("adoptState takes fresh state only", () => {
  const st = { updated_at: 1000, running: { "a/1": { machine: "a", id: "1", name: "n" } }, armed: { "a:/x": false },
    projArmed: { "a:p1:GPU": false } };
  assert.deepEqual(adoptState(st, 1000 + 119999), { running: st.running, armed: st.armed, projArmed: st.projArmed });
  assert.deepEqual(adoptState(st, 1000 + 120000), { running: null, armed: {}, projArmed: {} });
  assert.deepEqual(adoptState(null, 5), { running: null, armed: {}, projArmed: {} });
  assert.deepEqual(adoptState({ updated_at: 1000 }, 1001), { running: null, armed: {}, projArmed: {} });
});

test("mustAdopt when not leading or lease held by another copy", () => {
  assert.equal(mustAdopt(false, { copy: "me", at: 1 }, "me"), true);
  assert.equal(mustAdopt(true, null, "me"), true);
  // Watcher stalled; a panel copy took the lease meanwhile: re-adopt its maps.
  assert.equal(mustAdopt(true, { copy: "panel", at: 1 }, "me"), true);
  assert.equal(mustAdopt(true, { copy: "me", at: 1 }, "me"), false);
});

test("jobButtons: running Slurm job with GPUs has everything", () => {
  assert.deepEqual(jobButtons({ scheduler: "slurm", state: "RUNNING", allocGpus: 4 }),
    ["cancel", "requeue", "hold", "release", "terminal", "agent", "gpu"]);
});

test("jobButtons: running Slurm job without GPUs has no GPU usage", () => {
  assert.deepEqual(jobButtons({ scheduler: "slurm", state: "RUNNING", allocGpus: 0 }),
    ["cancel", "requeue", "hold", "release", "terminal", "agent"]);
});

test("jobButtons: pending Slurm job has no terminal or GPU usage", () => {
  assert.deepEqual(jobButtons({ scheduler: "slurm", state: "PENDING", allocGpus: 4 }),
    ["cancel", "requeue", "hold", "release", "agent"]);
});

test("jobButtons: PBS never has terminal or GPU usage", () => {
  assert.deepEqual(jobButtons({ scheduler: "pbs", state: "RUNNING", allocGpus: 2 }),
    ["cancel", "requeue", "hold", "release", "agent"]);
});

test("confirmText asks only for cancel and requeue", () => {
  assert.equal(confirmText("cancel", "123"), "Cancel job 123?");
  assert.equal(confirmText("requeue", "123"), "Requeue job 123?");
  assert.equal(confirmText("hold", "123"), null);
  assert.equal(confirmText("release", "123"), null);
});

test("factsLine joins nodes, start, alloc and steps", () => {
  const now = new Date(2026, 10, 17, 12, 0);
  const d = {
    node_list: "n141", start_time: "2026-11-17T10:02:11",
    alloc: { cpus: 16, mem: "64G", gpus: 4 },
    steps: [{ step: "123.batch", ave_cpu: "01:02:03", max_rss: "12G" }],
  };
  assert.equal(factsLine(d, now), "n141 · start 10:02 · 16 cpus · 64G · 4 GPU · 123.batch 01:02:03 / 12G");
});

test("factsLine skips missing parts and zero GPUs", () => {
  const now = new Date(2026, 10, 17, 12, 0);
  const d = { node_list: "", start_time: "Unknown", alloc: { cpus: 2, mem: "4G", gpus: 0 }, steps: [] };
  assert.equal(factsLine(d, now), "2 cpus · 4G");
  assert.equal(factsLine({}, now), "");
});

test("gpuChip shows util and memory in GiB", () => {
  assert.equal(gpuChip({ node: "n141", index: 0, util: 97, mem_used_mib: 30000, mem_total_mib: 40960 }), "n141#0 97% 29/40G");
});

test("partition chip text and color", () => {
  assert.equal(partitionLabel({ name: "gpu", gpu_type: "a100-sxm4", gpus_free: 11, gpus_total: 32 }), "gpu a100-sxm4 11/32 free");
  assert.equal(partitionLabel({ name: "gpu_long", gpu_type: "", gpus_free: 0, gpus_total: 208 }), "gpu_long 0/208 free");
  assert.equal(partitionColorKey({ gpus_free: 1 }), "ansi2");
  assert.equal(partitionColorKey({ gpus_free: 0 }), "muted");
});

test("storageLabel uses the higher of filesystem and quota percent", () => {
  assert.equal(storageLabel({ path: "/scratch", pct: 45, quota: null }), "/scratch 45%");
  assert.equal(storageLabel({ path: "/home/u", pct: 15, quota: { pct: 91 } }), "/home/u 91%");
});

test("pendingStart labels a known start, else null", () => {
  const now = new Date(2026, 10, 17, 12, 0);
  const cl = { pending: [{ id: "77", start: "2026-11-17T14:30:00" }, { id: "78", start: "2026-11-18T09:05:00" }] };
  assert.equal(pendingStart(cl, "77", "PENDING", now), "starts ~14:30");
  assert.equal(pendingStart(cl, 78, "PENDING", now), "starts ~Wed 09:05");
  assert.equal(pendingStart(cl, "79", "PENDING", now), null);
  assert.equal(pendingStart(null, "77", "PENDING", now), null);
  assert.equal(pendingStart({ pending: [{ id: "77", start: "garbage" }] }, "77", "PENDING", now), null);
});

test("pendingStart is null for a job that is no longer pending", () => {
  const now = new Date(2026, 10, 17, 12, 0);
  const cl = { pending: [{ id: "77", start: "2026-11-17T14:30:00" }] };
  assert.equal(pendingStart(cl, "77", "RUNNING", now), null);
  assert.equal(pendingStart(cl, "77", "COMPLETING", now), null);
  assert.equal(pendingStart(cl, "77", undefined, now), null);
});

test("logTabs hides err when its tail is empty", () => {
  assert.deepEqual(logTabs({ out_tail: ["a"], err_tail: [] }), ["out"]);
  assert.deepEqual(logTabs({ out_tail: ["a"], err_tail: ["b"] }), ["out", "err"]);
  assert.deepEqual(logTabs(null), ["out"]);
});

test("logLines keeps chronological order and at most 50 lines", () => {
  const tail = Array.from({ length: 60 }, (_, i) => `l${i}`);
  const lines = logLines({ out_tail: tail, err_tail: ["e"] }, "out");
  assert.equal(lines.length, 50);
  assert.equal(lines[0], "l10");
  assert.equal(lines[49], "l59");
  assert.deepEqual(logLines({ out_tail: ["a"], err_tail: ["e"] }, "err"), ["e"]);
  assert.deepEqual(logLines({ out_tail: ["a"], err_tail: [] }, "err"), ["a"]);
  assert.deepEqual(logLines(null, "out"), []);
});

test("agentPrompt holds the job facts, tails and the request", () => {
  const out = Array.from({ length: 60 }, (_, i) => `out${i}`);
  const p = agentPrompt({
    machine: "Devana", ssh_target: "login.devana.nscc.sk", id: "123", state: "FAILED", name: "train",
    stdout: "/home/u/slurm-123.out", stderr: "/home/u/slurm-123.err", out_tail: out, err_tail: ["boom"],
  });
  for (const s of ["Devana", "login.devana.nscc.sk", "123", "FAILED", "train", "/home/u/slurm-123.out", "/home/u/slurm-123.err", "out10", "out59", "boom"]) {
    assert.ok(p.includes(s), s);
  }
  assert.ok(!p.includes("out9\n"));
  assert.ok(p.includes("Diagnose why this Slurm job failed or is misbehaving. Use ssh login.devana.nscc.sk to read more of the logs."));
});

test("agentPrompt tolerates a job without detail", () => {
  const p = agentPrompt({ machine: "Perun", ssh_target: "h", id: "9", state: "PENDING" });
  assert.ok(p.includes("Use ssh h to read more of the logs."));
});

test("endedSummary groups many ended jobs into one toast", () => {
  const notices = [
    ...Array(3).fill({ level: "success", state: "COMPLETED" }),
    ...Array(4).fill({ level: "error", state: "FAILED" }),
  ];
  assert.deepEqual(endedSummary("Devana", notices), {
    title: "Devana: 7 jobs ended", body: "3 COMPLETED, 4 FAILED", level: "error",
  });
  assert.equal(endedSummary("P", [{ level: "success", state: "COMPLETED" }, { level: "warning", state: "UNKNOWN" }]).level, "warning");
  assert.equal(endedSummary("P", [{ level: "success", state: "COMPLETED" }]).level, "success");
});

import { quotaFor, mergedStoragePct, burnDays, hoursPct, projectAlerts, projectNotice } from "./logic.js";

const mp = { name: "a", error: null, quotas: [
  { kind: "project", name: "p", path: "/mnt/data/", used_kb: 1, limit_kb: 2, pct: 70 },
  { kind: "project", name: "q", path: "/mnt/data/proj", used_kb: 1, limit_kb: 2, pct: 95 },
] };

test("quota merge matches path prefix on segment boundary", () => {
  assert.deepEqual(quotaFor(mp, "/mnt/data/proj"), { pct: 95, quota: true });
  assert.deepEqual(quotaFor(mp, "/mnt/data/proj/x"), { pct: 95, quota: true });
  assert.deepEqual(quotaFor(mp, "/mnt/data/projx"), { pct: 70, quota: true });
  assert.equal(quotaFor(mp, "/mnt/database"), null);
  assert.equal(quotaFor(null, "/mnt/data"), null);
  assert.deepEqual(mergedStoragePct({ path: "/mnt/data/proj", pct: 10 }, mp), { pct: 95, quota: true });
});

test("quota merge keeps fs pct for unmatched path", () => {
  assert.deepEqual(mergedStoragePct({ path: "/home", pct: 42 }, mp), { pct: 42, quota: false });
  assert.deepEqual(mergedStoragePct({ path: "/home", pct: 42 }, undefined), { pct: 42, quota: false });
});

test("burnDays basic", () => {
  assert.equal(burnDays({ used: 100, awarded: 1000, used_30d: 300 }), 90);
  assert.equal(burnDays({ used: 1000, awarded: 1000, used_30d: 300 }), null);
});

test("burnDays null without 30d usage", () => {
  assert.equal(burnDays({ used: 1, awarded: 10, used_30d: 0 }), null);
  assert.equal(burnDays({ used: 1, awarded: 10, used_30d: null }), null);
  assert.equal(burnDays(null), null);
});

test("hoursPct null without award", () => {
  assert.equal(hoursPct({ used: 5, awarded: null }), null);
  assert.equal(hoursPct({ used: 5, awarded: 0 }), null);
  assert.equal(hoursPct({ used: null, awarded: 10 }), null);
  assert.equal(hoursPct({ used: 1, awarded: 3 }), 33);
});

test("project alert fires at 90 once and re-arms below 85", () => {
  const st = (used, error = null) => ({ machines: [{ name: "a", error, projects: [
    { account: "acc", cpu: { used, awarded: 100 }, gpu: { used: null, awarded: null } },
  ] }] });
  let r = projectAlerts(null, st(90));
  assert.deepEqual(r.alerts, [{ machine: "a", account: "acc", kind: "CPU", pct: 90 }]);
  r = projectAlerts(r.armed, st(95));
  assert.deepEqual(r.alerts, []);
  r = projectAlerts(r.armed, st(50, "boom"));
  assert.equal(r.armed["a:acc:CPU"], false);
  r = projectAlerts(r.armed, st(86));
  assert.equal(r.armed["a:acc:CPU"], false);
  r = projectAlerts(r.armed, st(84));
  assert.equal(r.armed["a:acc:CPU"], true);
  assert.equal("a:acc:GPU" in r.armed, false);
  r = projectAlerts(r.armed, st(91));
  assert.equal(r.alerts.length, 1);
});

test("projectNotice text", () => {
  assert.deepEqual(projectNotice({ machine: "a", account: "acc", kind: "GPU", pct: 92 }), {
    title: "a: acc GPU hours 92% used", body: "Ask the PI for more hours or slow down.", level: "warning",
  });
});
