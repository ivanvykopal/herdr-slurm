import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { exec, theme, props } from "herdr";

const POLL_MS = 20000;

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
    this.data = null;
    this.failure = null;
    this.loading = false;
    this.width = 800;
    this.collapsed = {};
    this.refresh(cx);
    cx.timer.every(POLL_MS, (cx) => this.refresh(cx));
  }

  refresh(cx) {
    if (this.loading) return;
    this.loading = true;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        // Each machine is queried over ssh with a 15 s cap in the binary.
        const r = await exec(this.bin, ["snapshot"], { timeout: 60000 });
        this.data = JSON.parse(r.stdout);
        this.failure = null;
      } catch (e) {
        this.failure = String(e?.message ?? e);
      }
      this.loading = false;
      cx.notify();
    });
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
    const t = theme();
    const d = this.data;
    const stamp = d?.machines?.[0]?.fetched_at;
    const header = h_flex().px(12).py(8).gap(8).items_center().border_b(1).border_color(t.border)
      .child(div().font_semibold().text_size(14).text_color(t.foreground).child("Jobs"))
      .child(div().flex_1())
      .child(div().text_size(11).text_color(t.dim).child(this.loading ? "refreshing…" : ago(stamp)))
      .child(div().id("refresh").px(8).py(2).rounded(6).text_size(12).text_color(t.muted)
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_click((_e, cx) => this.refresh(cx)).child("↻ Refresh"));

    let body;
    if (this.failure && !d) {
      body = [this.note(t, `Cannot run "${this.bin}": ${this.failure}`, t.blocked),
        this.note(t, "Build herdr-slurm (cargo build --release) and put it on PATH, or pass props {\"bin\": \"<path>\"} to this mod.")];
    } else if (!d) {
      body = [this.note(t, "Loading…")];
    } else if (d.error) {
      body = [this.note(t, d.error, t.blocked)];
    } else {
      body = [
        ...(d.warnings ?? []).map((w) => this.note(t, w, t.done)),
        ...(this.failure ? [this.note(t, `refresh failed: ${this.failure}`, t.blocked)] : []),
        ...d.machines.map((m) => this.machine(t, m, cx)),
      ];
    }

    return v_flex().id("slurm-root").size_full().bg(t.surface)
      .on_size_change((size, cx) => { this.width = size.width; cx.notify(); })
      .child(header)
      .child(v_flex().id("slurm-scroll").flex_1().overflow_y_scrollbar().children(body));
  }
}
