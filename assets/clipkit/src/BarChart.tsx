import React from "react";
import { interpolate, useCurrentFrame } from "remotion";
import { AMBER, Base, FONT, Headline, MUTED, Stage, TEXT, clamp, fmtNum, out, textShadow } from "./kit";

export type BarChartProps = Base & { unit?: string; bars: { label: string; value: number }[]; highlight?: number };

export const BarChart: React.FC<BarChartProps> = ({ background, kicker, title, unit = "", bars, highlight = 0 }) => {
  const frame = useCurrentFrame();
  const list = (bars || []).filter((b) => b && Number.isFinite(b.value) && b.value >= 0).slice(0, 5);
  const max = Math.max(1, ...list.map((b) => b.value));
  const base = 860, top = 360, bw = Math.min(220, 1200 / Math.max(1, list.length) - 60);
  const total = list.length * bw + (list.length - 1) * 90;
  const x0 = (1920 - total) / 2;
  return (
    <Stage background={background}>
      <Headline kicker={kicker} title={title} />
      <svg width={1920} height={1080} style={{ position: "absolute" }}>
        <line x1={x0 - 60} x2={x0 + total + 60} y1={base} y2={base} stroke={TEXT} strokeOpacity={0.45} strokeWidth={3} />
        {list.map((b, i) => {
          const st = 14 + i * 10;
          const h = ((base - top) * b.value) / max * interpolate(frame, [st, st + 36], [0, 1], { ...clamp, easing: out });
          return <rect key={i} x={x0 + i * (bw + 90)} y={base - h} width={bw} height={h} rx={10} fill={i === highlight ? AMBER : MUTED} />;
        })}
      </svg>
      {list.map((b, i) => {
        const st = 40 + i * 10;
        const x = x0 + i * (bw + 90) + bw / 2;
        const h = ((base - top) * b.value) / max;
        const o = interpolate(frame, [st, st + 14], [0, 1], clamp);
        return (
          <React.Fragment key={i}>
            <div style={{ position: "absolute", left: x - 200, width: 400, top: base - h - 78, textAlign: "center", fontFamily: FONT, fontWeight: 700, fontSize: 54, color: i === highlight ? AMBER : TEXT, textShadow, opacity: o }}>{fmtNum(b.value)}{unit ? <span style={{ fontWeight: 500, fontSize: 34 }}> {unit}</span> : null}</div>
            <div style={{ position: "absolute", left: x - 200, width: 400, top: base + 22, textAlign: "center", fontFamily: FONT, fontWeight: 500, fontSize: 36, textTransform: "uppercase", color: TEXT, letterSpacing: 1, textShadow, opacity: o }}>{b.label}</div>
          </React.Fragment>
        );
      })}
    </Stage>
  );
};
