import { View, div } from "gpui-kit";
import { v_flex, h_flex, ScrollHandle } from "gpui-base";
import { exec, theme, props, shared, setShared, notify, playSound, checking, run, settings } from "herdr";
import { endedJobs, storageAlerts, shouldLead, mustAdopt, jobNotice, storageNotice, adoptState, pendingStart, agentPrompt } from "./logic.js";
import { detailBlock } from "./detail.js";
import { clusterStrip } from "./strip.js";

const TICK_MS = 5000;
const SNAPSHOT_MS = 20000;
const CLUSTER_MS = 120000;
const FINAL_MAX_IDS = 50;
const DETAIL_MS = 20000;
const LEASE = "slurm.leader";
const STATE = "slurm.state";
const REFRESH = "slurm.refresh";

// Same classes as the herdr-slurm TUI: green running, yellow pending, red bad ends.
function stateColor(state, t) {
  const s = (state || "").toUpperCase();
  if (s.startsWith("RUN") || s === "COMPLETING") return t.ansi2;
  if (s.startsWith("PEND") || s === "QUEUED" || s === "HELD") return t.ansi3;
  if (/FAIL|CANCEL|TIMEOUT|NODE_FAIL|OUT_OF_MEM|PREEMPT|BOOT_FAIL|DEADLINE/.test(s)) return t.blocked;
  return t.muted;
}

function ago(ts) {
  if (!ts) return "";
  const s = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  return s < 5 ? "just now" : s < 60 ? `${s}s ago` : `${Math.floor(s / 60)}m ago`;
}

export default class Slurm extends View {
  init(_props, cx) {
    // `bin` prop overrides the executable; default resolves herdr-slurm on PATH.
    this.bin = props()?.bin || "herdr-slurm";
    this.watch = Boolean(props()?.watch);
    this.copy = Math.random().toString(36).slice(2);
    this.width = 800;
    this.collapsed = {};
    // Panel-copy state for expanded jobs, independent of the lease: one open job per machine.
    this.open = {};
    this.details = {};
    this.logScroll = {};
    // Leader-only state; see tick().
    this.leading = false;
    this.polling = false;
    this.polled = false;
    this.clusterBusy = false;
    this.lastPoll = 0;
    this.lastCluster = 0;
    this.snapshot = null;
    this.cluster = null;
    this.failure = null;
    this.running = null;
    this.armed = {};
    if (checking()) return;
    this.tick(cx);
    cx.timer.every(TICK_MS, (cx) => this.tick(cx));
    if (!this.watch) cx.timer.every(TICK_MS, (cx) => this.detailTick(cx));
  }

  // Exactly one copy (the lease holder, watcher preferred) runs herdr-slurm and publishes
  // STATE; every panel copy renders from STATE.
  tick(cx) {
    const now = Date.now();
    if (!shouldLead(shared(LEASE), this.copy, now, this.watch)) {
      this.leading = false;
      return;
    }
    // Adopt before writing our lease: covers a copy that silently lost the lease while its
    // ticks stalled (another copy led and notified meanwhile), not only a fresh leader.
    if (mustAdopt(this.leading, shared(LEASE), this.copy)) {
      const st = shared(STATE);
      ({ running: this.running, armed: this.armed } = adoptState(st, now));
      this.snapshot = st?.snapshot ?? null;
      this.cluster = st?.cluster ?? null;
      this.failure = null;
      this.polled = false;
      this.lastPoll = 0;
      this.lastCluster = 0;
      this.leading = true;
    }
    setShared(LEASE, { copy: this.copy, at: now, watch: this.watch });
    const asked = Number(shared(REFRESH)) || 0;
    if (!this.polling && (now - this.lastPoll >= SNAPSHOT_MS || asked > this.lastPoll)) this.poll(cx);
    else this.maybeCluster(cx);
  }

  holdsLease() {
    return this.leading && shared(LEASE)?.copy === this.copy;
  }

  publish() {
    setShared(STATE, {
      snapshot: this.snapshot,
      cluster: this.cluster,
      failure: this.failure,
      updated_at: Date.now(),
      running: this.running,
      armed: this.armed,
    });
  }

  poll(cx) {
    this.polling = true;
    this.lastPoll = Date.now();
    cx.spawn(async (cx) => {
      let snap = null;
      let failure = null;
      try {
        // Each machine is queried over ssh with a 15 s cap in the binary.
        const r = await exec(this.bin, ["snapshot"], { timeout: 60000 });
        snap = JSON.parse(r.stdout);
      } catch (e) {
        failure = String(e?.message ?? e);
      }
      this.polling = false;
      // Leadership may have moved while ssh ran; the new leader owns the maps then.
      if (!this.holdsLease()) return;
      this.failure = failure;
      let ended = [];
      if (snap) {
        this.snapshot = snap;
        ({ running: this.running, ended } = endedJobs(this.running, snap));
      }
      this.polled = true;
      this.publish();
      cx.notify();
      this.maybeCluster(cx);
      if (ended.length) await this.notifyEnded(ended);
    });
  }

  maybeCluster(cx) {
    if (this.clusterBusy || !this.polled || Date.now() - this.lastCluster < CLUSTER_MS) return;
    this.clusterBusy = true;
    this.lastCluster = Date.now();
    cx.spawn(async (cx) => {
      let cl = null;
      try {
        cl = JSON.parse((await exec(this.bin, ["cluster"], { timeout: 120000 })).stdout);
      } catch (_e) {
        // Cluster data is optional; the next round retries.
      }
      this.clusterBusy = false;
      if (!cl || !this.holdsLease()) return;
      this.cluster = cl;
      const r = storageAlerts(this.armed, cl);
      this.armed = r.armed;
      this.publish();
      cx.notify();
      for (const a of r.alerts) notify(storageNotice(a));
    });
  }

  // One `final` call per machine (batches of FINAL_MAX_IDS); jobs it cannot resolve
  // are reported as UNKNOWN.
  async notifyEnded(ended) {
    const byMachine = {};
    for (const e of ended) (byMachine[e.machine] ??= []).push(e);
    for (const [machine, entries] of Object.entries(byMachine)) {
      const finals = {};
      for (let i = 0; i < entries.length; i += FINAL_MAX_IDS) {
        const batch = entries.slice(i, i + FINAL_MAX_IDS);
        try {
          const r = await exec(this.bin, ["final", machine, ...batch.map((e) => String(e.id))], { timeout: 60000 });
          const d = JSON.parse(r.stdout);
          if (!d.error) for (const j of d.jobs ?? []) finals[String(j.id)] = j;
        } catch (_e) {
          // Unresolved jobs fall through as UNKNOWN.
        }
      }
      for (const e of entries) {
        const n = jobNotice(e, finals[String(e.id)] ?? null);
        notify(n);
        if (n.level === "error") playSound("blocked");
      }
    }
  }

  // Expanded job: `herdr-slurm job` on open and every DETAIL_MS while open; `gpu` adds --gpu.
  toggleJob(m, j, cx) {
    const id = String(j.id);
    if (this.open[m.name] === id) {
      delete this.open[m.name];
      delete this.details[m.name];
    } else {
      this.open[m.name] = id;
      this.details[m.name] = { id, job: j, data: null, error: null, at: 0, fetching: false, tab: "out",
        confirm: null, busy: null, gpuBusy: false, gpu: null, logKey: null };
      this.logScroll[m.name] ??= ScrollHandle.new();
      this.fetchDetail(m.name, cx, false);
    }
    cx.notify();
  }

  // Blocks under a collapsed machine are not shown; they catch up within a tick of expanding.
  detailTick(cx) {
    const now = Date.now();
    for (const [machine, det] of Object.entries(this.details)) {
      if (!this.collapsed[machine] && !det.fetching && now - det.at >= DETAIL_MS) this.fetchDetail(machine, cx, false);
    }
  }

  fetchDetail(machine, cx, gpu) {
    const det = this.details[machine];
    if (!det) return;
    const done = () => { if (gpu) det.gpuBusy = false; else det.fetching = false; };
    if (gpu) det.gpuBusy = true;
    else { det.fetching = true; det.at = Date.now(); }
    const args = ["job", machine, det.id, "--lines", "50", ...(gpu ? ["--gpu"] : [])];
    // The busy flag is cleared on every path, so detailTick and the GPU button can retry.
    let spawned = false;
    try {
      cx.spawn(async (cx) => {
        let d = null;
        let error = null;
        try {
          d = JSON.parse((await exec(this.bin, args, { timeout: 60000 })).stdout);
          if (d.error) { error = d.error; d = null; }
        } catch (e) {
          error = String(e?.message ?? e);
        } finally {
          done();
        }
        // Closed, or another job opened on this machine meanwhile.
        if (this.details[machine] !== det) return;
        // A job past MinJobAge errors; the last detail stays shown with the error.
        det.error = error;
        if (d) {
          det.data = d;
          if (gpu) det.gpu = { gpus: d.gpus ?? [], gpu_error: d.gpu_error ?? null };
        }
        cx.notify();
      });
      spawned = true;
    } finally {
      if (!spawned) done();
    }
  }

  runAction(m, verb, cx) {
    const det = this.details[m.name];
    if (!det || det.busy) return;
    det.busy = verb;
    det.confirm = null;
    cx.notify();
    const id = det.id;
    let spawned = false;
    try {
      cx.spawn(async (cx) => {
        let error = null;
        try {
          const r = JSON.parse((await exec(this.bin, ["action", m.name, id, verb], { timeout: 60000 })).stdout);
          if (!r.ok || r.error) error = r.error || r.output || "failed";
        } catch (e) {
          error = String(e?.message ?? e);
        } finally {
          det.busy = null;
        }
        if (error) {
          notify({ title: `${m.name}: ${verb} ${id} failed`, body: error, level: "error" });
        } else {
          setShared(REFRESH, Date.now());
          if (this.leading) this.tick(cx);
          if (this.details[m.name] === det) this.fetchDetail(m.name, cx, false);
        }
        cx.notify();
      });
      spawned = true;
    } finally {
      if (!spawned) det.busy = null;
    }
  }

  // Handlers receive their own cx: a render cx is only valid during that render.
  detail(t, m) {
    const det = this.details[m.name];
    const target = m.ssh_target || m.host;
    return detailBlock(t, {
      m, det,
      font: settings().font_family ?? settings().font,
      scroll: this.logScroll[m.name],
      onVerb: (verb, cx) => this.runAction(m, verb, cx),
      onConfirm: (verb, cx) => { det.confirm = verb; cx.notify(); },
      onTab: (tab, cx) => { det.tab = tab; cx.notify(); },
      onGpu: (cx) => { if (!det.gpuBusy) { this.fetchDetail(m.name, cx, true); cx.notify(); } },
      onTerminal: () => run("tab.new", {
        name: `${det.id}@${m.name}`,
        argv: ["ssh", "-t", target, `srun --jobid=${det.id} --overlap --pty bash -l`],
      }),
      onAgent: () => {
        const d = det.data ?? {};
        run("tab.new", {
          name: `log ${det.id}`,
          argv: [props()?.agent ?? "claude"],
          task: agentPrompt({ machine: m.name, ssh_target: target, id: det.id, state: d.state || det.job.state,
            name: det.job.name, stdout: d.stdout, stderr: d.stderr, out_tail: d.out_tail, err_tail: d.err_tail }),
        });
      },
    });
  }

  note(t, text, color) {
    return div().px(12).py(6).text_size(12).text_color(color ?? t.muted).child(text);
  }

  jobRow(t, m, j, mc) {
    const w = this.width;
    const open = this.open[m.name] === String(j.id);
    const col = (text, width, color) =>
      div().w(width).flex_none().truncate().text_color(color ?? t.foreground).child(text || "-");
    const reason = pendingStart(mc, j.id, j.state, new Date()) ?? (j.reason || j.where || "");
    const row = h_flex().id(`job-${m.name}-${j.id}`).px(12).py(3).gap(10).text_size(12)
      .when(open, (el) => el.bg(t.selected))
      .hover((s) => s.bg(t.hover))
      .on_click((_e, cx) => this.toggleJob(m, j, cx))
      .child(col(j.id, 84, t.muted))
      .child(col(j.state, 92, stateColor(j.state, t)));
    if (w >= 640) row.child(div().flex_1().truncate().child(j.name || "-")).child(col(j.account, 90, t.muted));
    else row.child(div().flex_1().truncate().child(j.name || "-"));
    row.child(col(j.elapsed, 72, t.muted));
    if (w >= 640) row.child(col(j.nodes, 36, t.muted)).child(div().w(200).flex_none().truncate().text_color(t.dim).child(reason));
    else if (w >= 420) row.child(div().w(120).flex_none().truncate().text_color(t.dim).child(reason));
    return row;
  }

  // Job rows with the open job's block under its row. An open job that left the snapshot
  // keeps its row and last detail at the end until closed.
  jobRows(t, m, mc) {
    const det = this.details[m.name];
    const out = [];
    let found = false;
    for (const j of m.error ? [] : m.jobs) {
      out.push(this.jobRow(t, m, j, mc));
      if (det && det.id === String(j.id)) {
        found = true;
        det.job = j;
        out.push(this.detail(t, m));
      }
    }
    if (det && !found) out.push(this.jobRow(t, m, det.job, mc), this.detail(t, m));
    return out;
  }

  machine(t, m, mc) {
    const open = !this.collapsed[m.name];
    const counts = {};
    for (const j of m.jobs) counts[j.state] = (counts[j.state] ?? 0) + 1;
    const summary = m.error ? "unreachable" : Object.entries(counts).map(([k, v]) => `${v} ${k.toLowerCase()}`).join(", ") || "no jobs";
    const head = h_flex().id(`m-${m.name}`).px(12).py(6).gap(8).items_center().bg(t.raised)
      .hover((s) => s.bg(t.hover))
      .on_click((_e, cx) => { this.collapsed[m.name] = open; cx.notify(); })
      .child(div().text_color(t.muted).text_size(12).child(open ? "▾" : "▸"))
      .child(div().font_semibold().text_size(13).text_color(t.foreground).child(m.name))
      .child(div().text_size(11).text_color(t.dim).child(`${m.scheduler} · ${m.user}`))
      .child(div().flex_1())
      .child(div().text_size(11).text_color(m.error ? t.blocked : t.muted).child(summary));
    const strip = open ? clusterStrip(t, mc) : null;
    const rows = open ? this.jobRows(t, m, mc) : [];
    const body = !open ? [] : [
      ...(strip ? [strip] : []),
      ...(m.error ? [this.note(t, `✗ ${m.error}`, t.blocked)] : []),
      ...(!m.error && rows.length === 0 ? [this.note(t, "no jobs in queue")] : []),
      ...rows,
    ];
    return v_flex().child(head).children(body);
  }

  render(cx) {
    if (this.watch) return div();
    const t = theme();
    const st = shared(STATE);
    const d = st?.snapshot ?? null;
    const cl = st?.cluster ?? null;
    const failure = st?.failure ?? null;
    const stamp = d?.machines?.[0]?.fetched_at;
    const refreshing = (Number(shared(REFRESH)) || 0) > (st?.updated_at ?? 0);
    const header = h_flex().px(12).py(8).gap(8).items_center().border_b(1).border_color(t.border)
      .child(div().font_semibold().text_size(14).text_color(t.foreground).child("Jobs"))
      .child(div().flex_1())
      .child(div().text_size(11).text_color(t.dim).child(refreshing ? "refreshing…" : ago(stamp)))
      .child(div().id("refresh").px(8).py(2).rounded(6).text_size(12).text_color(t.muted)
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_click((_e, cx) => { setShared(REFRESH, Date.now()); if (this.leading) this.tick(cx); })
        .child("↻ Refresh"));

    let body;
    if (failure && !d) {
      body = [this.note(t, `Cannot run "${this.bin}": ${failure}`, t.blocked),
        this.note(t, "Build herdr-slurm (cargo build --release) and put it on PATH, or pass props {\"bin\": \"<path>\"} to this mod.")];
    } else if (!d) {
      body = [this.note(t, "Loading…")];
    } else if (d.error) {
      body = [this.note(t, d.error, t.blocked)];
    } else {
      body = [
        ...(d.warnings ?? []).map((w) => this.note(t, w, t.done)),
        ...(failure ? [this.note(t, `refresh failed: ${failure}`, t.blocked)] : []),
        ...d.machines.map((m) => this.machine(t, m, cl?.machines?.find((c) => c.name === m.name) ?? null)),
      ];
    }

    return v_flex().id("slurm-root").size_full().bg(t.surface)
      .on_size_change((size, cx) => { this.width = size.width; cx.notify(); })
      .child(header)
      .child(v_flex().id("slurm-scroll").flex_1().overflow_y_scrollbar().children(body));
  }
}
