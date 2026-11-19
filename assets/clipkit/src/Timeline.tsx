import React from "react";
import { interpolate, useCurrentFrame } from "remotion";
import { AMBER, Base, FONT, Headline, MUTED, Stage, TEXT, clamp, fmtNum, out, textShadow } from "./kit";

export type Row = { label: string; from: number; to: number; highlight?: boolean };
export type TimelineProps = Base & { unit?: "years ago" | "year"; rows?: Row[]; marks?: { at: number; label: string }[] };

const nice = (span: number) => {
  const p = Math.pow(10, Math.floor(Math.log10(span / 4)));
  return [1, 2, 2.5, 5, 10].map((m) => m * p).find((s) => span / s <= 6) || p * 10;
};
const fmtT = (v: number, unit: string) => {
  if (unit !== "years ago") return String(Math.round(v));
  if (v >= 1e6) return `${fmtNum(v / 1e6)} million`;
  return fmtNum(v);
};

export const Timeline: React.FC<TimelineProps> = ({ background, kicker, title, unit = "years ago", rows = [], marks = [] }) => {
  const frame = useCurrentFrame();
  const ago = unit === "years ago";
  const vals = [...rows.flatMap((r) => [r.from, r.to]), ...marks.map((m) => m.at)].filter(Number.isFinite);
  let lo = Math.min(...vals), hi = Math.max(...vals);
  if (!vals.length) { lo = 0; hi = 1; }
  const pad = (hi - lo || Math.abs(hi) || 1) * 0.08;
  lo -= pad; hi += pad;
  if (ago && lo < 0) lo = 0;
  const L = 200, R = 1720;
  // Older on the left: for "years ago" bigger numbers sit left.
  const X = (v: number) => (ago ? L + ((hi - v) / (hi - lo)) * (R - L) : L + ((v - lo) / (hi - lo)) * (R - L));
  const axisY = 380 + Math.max(1, rows.length) * 110;
  const step = nice(hi - lo);
  const ticks: number[] = [];
  for (let t = Math.ceil(lo / step) * step; t <= hi; t += step) ticks.push(t);
  const draw = interpolate(frame, [0, 24], [0, 1], { ...clamp, easing: out });
  return (
    <Stage background={background}>
      <Headline kicker={kicker} title={title} />
      <svg width={1920} height={1080} style={{ position: "absolute" }}>
        <line x1={L} x2={L + (R - L) * draw} y1={axisY} y2={axisY} stroke={TEXT} strokeOpacity={0.45} strokeWidth={3} />
        {ticks.map((t, i) => <line key={i} x1={X(t)} x2={X(t)} y1={axisY} y2={axisY + 14} stroke={TEXT} strokeOpacity={0.45 * draw} strokeWidth={3} />)}
        {rows.slice(0, 4).map((r, i) => {
          const st = 14 + i * 14;
          const a = X(ago ? Math.max(r.from, r.to) : Math.min(r.from, r.to));
          const b = X(ago ? Math.min(r.from, r.to) : Math.max(r.from, r.to));
          const w = Math.max(8, (b - a) * interpolate(frame, [st, st + 40], [0, 1], { ...clamp, easing: out }));
          return <rect key={i} x={a} y={360 + i * 110} width={w} height={44} rx={8} fill={r.highlight || rows.length === 1 ? AMBER : MUTED} />;
        })}
        {marks.slice(0, 5).map((m, i) => {
          const o = interpolate(frame, [40 + i * 10, 54 + i * 10], [0, 1], clamp);
          return <line key={i} x1={X(m.at)} x2={X(m.at)} y1={320} y2={axisY} stroke={AMBER} strokeWidth={3} strokeDasharray="10 8" opacity={o} />;
        })}
      </svg>
      {rows.slice(0, 4).map((r, i) => (
        <div key={i} style={{ position: "absolute", left: X(ago ? Math.max(r.from, r.to) : Math.min(r.from, r.to)), top: 360 + i * 110 - 46, fontFamily: FONT, fontWeight: 700, fontSize: 36, textTransform: "uppercase", color: r.highlight || rows.length === 1 ? AMBER : TEXT, letterSpacing: 1, textShadow, opacity: interpolate(frame, [20 + i * 14, 34 + i * 14], [0, 1], clamp), whiteSpace: "nowrap" }}>{r.label}</div>
      ))}
      {ticks.map((t, i) => (
        <div key={i} style={{ position: "absolute", left: X(t) - 150, width: 300, top: axisY + 22, textAlign: "center", fontFamily: FONT, fontWeight: 500, fontSize: 28, color: TEXT, opacity: 0.8 * draw, textShadow }}>{fmtT(t, unit)}</div>
      ))}
      {ago ? <div style={{ position: "absolute", right: 200, top: axisY + 64, fontFamily: FONT, fontWeight: 500, fontSize: 28, color: TEXT, opacity: 0.6 * draw, textTransform: "uppercase", letterSpacing: 2 }}>years ago</div> : null}
      {marks.slice(0, 5).map((m, i) => (
        <div key={i} style={{ position: "absolute", left: X(m.at) - 220, width: 440, top: 268 - (i % 2) * 0, textAlign: "center", fontFamily: FONT, fontWeight: 700, fontSize: 32, color: AMBER, textTransform: "uppercase", textShadow, opacity: interpolate(frame, [46 + i * 10, 60 + i * 10], [0, 1], clamp) }}>{m.label}</div>
      ))}
    </Stage>
  );
};
