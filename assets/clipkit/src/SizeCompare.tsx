import React from "react";
import { interpolate, useCurrentFrame } from "remotion";
import { AMBER, Base, Headline, Label, MUTED, Stage, TEXT, clamp, out, shadow } from "./kit";
import { SHAPES, ShapeKey, manArm, manBody } from "./shapes";

export type Animal = { shape: ShapeKey; name: string; lines?: string[]; sizeFt: number; highlight?: boolean };
export type SizeCompareProps = Base & { animals: Animal[]; person?: boolean; personFt?: number };

const GROUND = 780;
const GAP = 90;
// Narrow drawings still get room for their label.
const SLOT = 300;
const PSLOT = 200;

export const SizeCompare: React.FC<SizeCompareProps> = ({ background, kicker, title, animals, person = true, personFt = 5.75 }) => {
  const frame = useCurrentFrame();
  const list = (animals || []).filter((a) => SHAPES[a.shape] && a.sizeFt > 0).slice(0, 3);
  // Scale each drawing so its measured size (shoulder, length or height) matches sizeFt at a shared px per foot.
  const box = (a: Animal, ppf: number) => {
    const sh = SHAPES[a.shape];
    const s = (a.sizeFt * ppf) / sh.size;
    const h = a.shape === "whale" ? 70 * s : (200 - sh.top) * s;
    return { s, w: sh.width * s, h };
  };
  const fit = (ppf: number) => {
    const items = list.map((a) => box(a, ppf));
    const pw = person ? Math.max(0.17 * personFt * ppf, PSLOT) : 0;
    const width = items.reduce((t, b) => t + Math.max(b.w, SLOT), 0) + pw + GAP * (items.length + (person ? 1 : 0) - 1);
    const height = Math.max(person ? personFt * ppf : 0, ...items.map((b) => b.h));
    return { items, pw, width, height };
  };
  let ppf = 200;
  for (let i = 0; i < 40; i++) {
    const f = fit(ppf);
    if (f.width <= 1560 && f.height <= 470) break;
    ppf *= 0.92;
  }
  const f = fit(ppf);
  let x = (1920 - f.width) / 2;
  const groundDraw = interpolate(frame, [0, 22], [0, 1], { ...clamp, easing: out });
  const slots: React.ReactNode[] = [];
  const labels: React.ReactNode[] = [];
  let order = 0;
  if (person) {
    const h = personFt * ppf;
    const start = 10;
    const px = x + f.pw / 2 - 0.03 * h;
    slots.push(
      <g key="p" style={{ filter: shadow, opacity: interpolate(frame, [start, start + 18], [0, 1], clamp) }} transform={`translate(${px} ${GROUND - h}) scale(${h / 1000})`}>
        <path d={manBody} fill={TEXT} fillOpacity={0.88} />
        <path d={manArm} fill="#CFC7BA" />
      </g>,
    );
    labels.push(<Label key="pl" x={px} y={GROUND + 28} title="Person" lines={[`${Math.floor(personFt)} ft ${Math.round((personFt % 1) * 12)} in`]} start={start + 12} align="center" width={f.pw + GAP - 20} size={40} />);
    x += f.pw + GAP;
    order++;
  }
  list.forEach((a, i) => {
    const b = f.items[i];
    const sh = SHAPES[a.shape];
    const sw = Math.max(b.w, SLOT);
    x += (sw - b.w) / 2;
    const start = 16 + order * 22;
    const grow = interpolate(frame, [start, start + 34], [0.6, 1], { ...clamp, easing: out });
    const color = a.highlight ? AMBER : MUTED;
    const yTop = a.shape === "whale" ? GROUND - 148 * b.s : GROUND - 200 * b.s;
    slots.push(
      <g key={i} style={{ filter: shadow, opacity: interpolate(frame, [start, start + 14], [0, 1], clamp) }} transform={`translate(${x + b.w / 2} ${GROUND}) scale(${grow}) translate(${-b.w / 2} ${yTop - GROUND}) scale(${b.s})`}>
        {sh.far ? <path d={sh.far} fill={a.highlight ? "#A8691C" : "#6E675F"} /> : null}
        <path d={sh.near} fill={color} />
        {sh.extra ? <path d={sh.extra} fill={TEXT} fillOpacity={0.85} /> : null}
        {sh.eye ? <circle cx={sh.eye[0]} cy={sh.eye[1]} r={2.2} fill={a.highlight ? "#5A3608" : "#4A4540"} /> : null}
      </g>,
    );
    labels.push(<Label key={"l" + i} x={x + b.w / 2} y={GROUND + 28} title={a.name} lines={a.lines} color={a.highlight ? AMBER : TEXT} start={start + 20} align="center" width={sw + GAP - 20} />);
    x += b.w + (sw - b.w) / 2 + GAP;
    order++;
  });
  return (
    <Stage background={background}>
      <Headline kicker={kicker} title={title} />
      <svg width={1920} height={1080} style={{ position: "absolute", overflow: "visible" }}>
        <line x1={120} x2={120 + 1680 * groundDraw} y1={GROUND} y2={GROUND} stroke={TEXT} strokeOpacity={0.45} strokeWidth={3} strokeLinecap="round" />
        {slots}
      </svg>
      {labels}
    </Stage>
  );
};
