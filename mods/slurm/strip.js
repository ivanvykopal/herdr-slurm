import { div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { partitionLabel, partitionColorKey, storageLabel, storagePct, storageColorKey } from "./logic.js";

const BAR_W = 48;

// GPU headroom per partition, then storage use, under an expanded machine header.
// `mc` is the machine's entry from `herdr-slurm cluster`; null draws nothing.
export function clusterStrip(t, mc) {
  if (!mc) return null;
  if (mc.error) {
    return div().px(12).py(4).text_size(11).text_color(t.dim).child(`cluster: ${mc.error}`);
  }
  const chips = (mc.partitions ?? []).map((p) =>
    div().px(6).py(1).rounded(4).border(1).border_color(t.border).text_size(11)
      .text_color(t[partitionColorKey(p)]).child(partitionLabel(p)));
  const bars = (mc.storage ?? []).map((s) => {
    const pct = storagePct(s);
    const color = t[storageColorKey(pct)];
    const fill = Math.round((BAR_W * Math.min(100, Math.max(0, pct))) / 100);
    return h_flex().gap(4).items_center().text_size(11)
      .child(div().text_color(color).child(storageLabel(s)))
      .child(div().w(BAR_W).h(4).flex_none().rounded(2).bg(t.border)
        .child(div().w(fill).h(4).rounded(2).bg(color)));
  });
  return v_flex().px(12).py(4).gap(4).border_b(1).border_color(t.border)
    .when(chips.length > 0, (el) => el.child(h_flex().flex_wrap().gap(4).children(chips)))
    .when(bars.length > 0, (el) => el.child(h_flex().flex_wrap().gap_x(12).gap_y(2).children(bars)));
}
