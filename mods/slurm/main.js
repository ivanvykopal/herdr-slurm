import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { exec, theme, props, shared, setShared, notify, playSound, checking } from "herdr";
import { endedJobs, storageAlerts, shouldLead, mustAdopt, jobNotice, storageNotice, adoptState } from "./logic.js";

const TICK_MS = 5000;
const SNAPSHOT_MS = 20000;
const CLUSTER_MS = 120000;
const FINAL_MAX_IDS = 50;
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

  note(t, text, color) {
    return div().px(12).py(6).text_size(12).text_color(color ?? t.muted).child(text);
  }

  jobRow(t, j) {
    const w = this.width;
    const col = (text, width, color) =>
      div().w(width).flex_none().truncate().text_color(color ?? t.foreground).child(text || "-");
    const row = h_flex().id(`job-${j.id}`).px(12).py(3).gap(10).text_size(12)
      .hover((s) => s.bg(t.hover))
      .child(col(j.id, 84, t.muted))
      .child(col(j.state, 92, stateColor(j.state, t)));
    if (w >= 640) row.child(div().flex_1().truncate().child(j.name || "-")).child(col(j.account, 90, t.muted));
    else row.child(div().flex_1().truncate().child(j.name || "-"));
    row.child(col(j.elapsed, 72, t.muted));
    if (w >= 640) row.child(col(j.nodes, 36, t.muted)).child(div().w(200).flex_none().truncate().text_color(t.dim).child(j.reason || j.where || ""));
    else if (w >= 420) row.child(div().w(120).flex_none().truncate().text_color(t.dim).child(j.reason || j.where || ""));
    return row;
  }

  machine(t, m, cx) {
    const open = !this.collapsed[m.name];
    const counts = {};
    for (const j of m.jobs) counts[j.state] = (counts[j.state] ?? 0) + 1;
    const summary = m.error ? "unreachable" : Object.entries(counts).map(([k, v]) => `${v} ${k.toLowerCase()}`).join(", ") || "no jobs";
    const head = h_flex().id(`m-${m.name}`).px(12).py(6).gap(8).items_center().bg(t.raised)
      .hover((s) => s.bg(t.hover))
      .on_click(() => { this.collapsed[m.name] = open; cx.notify(); })
      .child(div().text_color(t.muted).text_size(12).child(open ? "▾" : "▸"))
      .child(div().font_semibold().text_size(13).text_color(t.foreground).child(m.name))
      .child(div().text_size(11).text_color(t.dim).child(`${m.scheduler} · ${m.user}`))
      .child(div().flex_1())
      .child(div().text_size(11).text_color(m.error ? t.blocked : t.muted).child(summary));
    const body = !open ? [] : m.error
      ? [this.note(t, `✗ ${m.error}`, t.blocked)]
      : m.jobs.length === 0 ? [this.note(t, "no jobs in queue")] : m.jobs.map((j) => this.jobRow(t, j));
    return v_flex().child(head).children(body);
  }

  render(cx) {
    if (this.watch) return div();
    const t = theme();
    const st = shared(STATE);
    const d = st?.snapshot ?? null;
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
        ...d.machines.map((m) => this.machine(t, m, cx)),
      ];
    }

    return v_flex().id("slurm-root").size_full().bg(t.surface)
      .on_size_change((size, cx) => { this.width = size.width; cx.notify(); })
      .child(header)
      .child(v_flex().id("slurm-scroll").flex_1().overflow_y_scrollbar().children(body));
  }
}
