import test from "node:test";
import assert from "node:assert/strict";
import {
  endedJobs, notifyLevel, gpuUnderuse, storageColorKey, storagePct,
  storageAlerts, formatStart, shouldLead, jobNotice, storageNotice, adoptState,
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
  const st = { updated_at: 1000, running: { "a/1": { machine: "a", id: "1", name: "n" } }, armed: { "a:/x": false } };
  assert.deepEqual(adoptState(st, 1000 + 119999), { running: st.running, armed: st.armed });
  assert.deepEqual(adoptState(st, 1000 + 120000), { running: null, armed: {} });
  assert.deepEqual(adoptState(null, 5), { running: null, armed: {} });
  assert.deepEqual(adoptState({ updated_at: 1000 }, 1001), { running: null, armed: {} });
});
