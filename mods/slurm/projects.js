import { div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { projectBars, mineLine, mergedStorageLabel, rawFallbacks, storageColorKey } from "./logic.js";

const BAR_W = 48;

function bar(t, label, pct) {
  const color = pct == null ? t.muted : t[storageColorKey(pct)];
  const fill = pct == null ? 0 : Math.round((BAR_W * Math.min(100, Math.max(0, pct))) / 100);
  return h_flex().gap(4).items_center().text_size(11)
    .child(div().text_color(color).child(label))
    .child(div().w(BAR_W).h(4).flex_none().rounded(2).bg(t.border)
      .child(div().w(fill).h(4).rounded(2).bg(color)));
}

const dimLine = (t, text) => div().text_size(11).text_color(t.dim).child(text);

function projectRow(t, p) {
  const bars = projectBars(p);
  const mine = mineLine(p);
  const storage = p.storage?.pct != null
    ? bar(t, mergedStorageLabel(p.storage.path, { pct: p.storage.pct, quota: false }), p.storage.pct)
    : null;
  return v_flex().gap(2)
    .child(h_flex().gap(8).items_center().text_size(12)
      .child(div().flex_none().text_color(t.foreground).child(p.account))
      .when(Boolean(p.title), (el) => el.child(div().flex_1().truncate().text_size(11).text_color(t.dim).child(p.title))))
    .child(h_flex().flex_wrap().gap_x(12).gap_y(2).children(bars.map((b) => bar(t, b.label, b.pct))))
    .when(mine != null, (el) => el.child(dimLine(t, mine)))
    .children(bars.filter((b) => b.burn).map((b) => dimLine(t, `${b.kind} ${b.burn}`)))
    .when(storage != null, (el) => el.child(storage));
}

// Active projects of one machine under its cluster strip; `machineProjects` is the machine's
// entry from `herdr-slurm projects`. Null (no data yet, or a PBS machine) draws nothing.
export function projectsBlock(t, machineProjects, _now) {
  const mp = machineProjects;
  if (!mp || mp.unsupported) return null;
  const box = v_flex().px(12).py(4).gap(6).border_b(1).border_color(t.border)
    .child(div().text_size(11).font_semibold().text_color(t.muted).child("Projects"));
  if (mp.error) return box.child(dimLine(t, `projects: ${mp.error}`));
  const projects = mp.projects ?? [];
  return box
    .children(projects.map((p) => projectRow(t, p)))
    .when(projects.length === 0, (el) => el.child(dimLine(t, "no active projects")))
    .children(rawFallbacks(mp).map((r) =>
      v_flex().child(dimLine(t, r.title)).children(r.lines.map((l) => dimLine(t, l)))));
}
