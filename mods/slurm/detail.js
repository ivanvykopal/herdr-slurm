import { div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { jobButtons, confirmText, factsLine, gpuChip, gpuUnderuse, logTabs, logLines } from "./logic.js";

const LABELS = {
  cancel: "Cancel", requeue: "Requeue", hold: "Hold", release: "Release",
  terminal: "Terminal on node", agent: "Open log in agent", gpu: "GPU usage",
};
const LOG_MAX_H = 220;

function button(t, id, label, onClick, disabled) {
  const b = div().id(id).px(8).py(2).rounded(4).border(1).border_color(t.border).text_size(11);
  if (disabled) return b.text_color(t.dim).child(label);
  return b.text_color(t.foreground).hover((s) => s.bg(t.hover)).on_click(onClick).child(label);
}

// Expanded block under an open job row.
// ctx: { m (snapshot machine), det (detail state from main.js), font, scroll (ScrollHandle),
//        onVerb(verb), onConfirm(verb|null), onTerminal(), onAgent(), onGpu(), onTab(tab) }
export function detailBlock(t, ctx) {
  const { m, det } = ctx;
  const d = det.data;
  const key = `${m.name}-${det.id}`;
  const state = d?.state || det.job.state;
  const keys = jobButtons({ scheduler: m.scheduler, state, allocGpus: d?.alloc?.gpus ?? 0 });
  const busy = Boolean(det.busy);
  const bar = h_flex().flex_wrap().gap(6).children(keys.map((k) => {
    if (k === "terminal") return button(t, `t-${key}`, LABELS[k], () => ctx.onTerminal(), false);
    if (k === "agent") return button(t, `a-${key}`, LABELS[k], () => ctx.onAgent(), false);
    if (k === "gpu") return button(t, `g-${key}`, det.gpuBusy ? "GPU usage…" : LABELS[k], () => ctx.onGpu(), det.gpuBusy);
    const label = det.busy === k ? `${LABELS[k]}…` : LABELS[k];
    const click = () => (confirmText(k, det.id) ? ctx.onConfirm(k) : ctx.onVerb(k));
    return button(t, `v-${k}-${key}`, label, click, busy);
  }));

  const block = v_flex().mx(12).mb(6).p(8).gap(6).rounded(6).border(1).border_color(t.border).bg(t.raised)
    .text_size(12).child(bar);

  const ask = det.confirm && confirmText(det.confirm, det.id);
  if (ask) {
    block.child(h_flex().flex_wrap().gap(6).items_center()
      .child(div().text_color(t.foreground).child(ask))
      .child(button(t, `y-${key}`, "Yes", () => ctx.onVerb(det.confirm), busy))
      .child(button(t, `n-${key}`, "No", () => ctx.onConfirm(null), busy)));
  }
  if (det.error) block.child(div().text_size(11).text_color(t.blocked).child(det.error));
  if (!d) {
    if (!det.error) block.child(div().text_size(11).text_color(t.dim).child("loading…"));
    return block;
  }

  const facts = factsLine(d, new Date());
  if (facts) block.child(div().text_size(11).text_color(t.muted).child(facts));

  if (det.gpu) {
    const flag = gpuUnderuse(d.alloc?.gpus ?? 0, det.gpu.gpus);
    block.child(h_flex().flex_wrap().gap(4).items_center().text_size(11)
      .children(det.gpu.gpus.map((g) => div().px(6).py(1).rounded(4).border(1).border_color(t.border)
        .text_color(t.foreground).child(gpuChip(g))))
      .when(flag, (el) => el.child(div().text_color(t.ansi3).child(flag)))
      .when(det.gpu.gpu_error, (el) => el.child(div().text_color(t.blocked).child(det.gpu.gpu_error))));
  }

  const tabs = logTabs(d);
  const tab = tabs.includes(det.tab) ? det.tab : "out";
  const lines = logLines(d, tab);
  block.child(h_flex().gap(4).text_size(11).children(tabs.map((name) =>
    div().id(`tab-${name}-${key}`).px(6).py(1).rounded(4)
      .bg(name === tab ? t.selected : t.raised)
      .text_color(name === tab ? t.foreground : t.muted)
      .hover((s) => s.bg(t.hover))
      .on_click(() => ctx.onTab(name))
      .child(name))));
  block.child(v_flex().id(`log-${key}`).max_h(LOG_MAX_H).overflow_y_scrollbar().track_scroll(ctx.scroll)
    .p(4).rounded(4).bg(t.background).font_family(ctx.font).text_size(11).text_color(t.foreground)
    .children(lines.length ? lines.map((l) => div().child(l || " ")) : [div().text_color(t.dim).child("(empty)")]));
  // Newest line in view when the log changes; a log the reader scrolled up stays put otherwise.
  const logKey = `${tab}\n${lines.join("\n")}`;
  if (logKey !== det.logKey) {
    det.logKey = logKey;
    if (lines.length) ctx.scroll.scroll_to_item(lines.length - 1);
  }
  return block;
}
