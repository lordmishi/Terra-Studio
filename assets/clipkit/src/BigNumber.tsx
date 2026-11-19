import React from "react";
import { interpolate, useCurrentFrame } from "remotion";
import { AMBER, Base, FONT, Stage, TEXT, clamp, fadeUp, fmtNum, out, textShadow } from "./kit";

export type BigNumberProps = Base & { value: string; label?: string; caption?: string };

// "~40", "1,100 psi", "2.5 million": the first number counts up, the text around it stays.
const split = (v: string) => {
  const m = /^(.*?)(\d[\d,]*(?:\.\d+)?)(.*)$/.exec(v || "");
  if (!m) return null;
  return { pre: m[1], n: Number(m[2].replace(/,/g, "")), dec: (m[2].split(".")[1] || "").length, post: m[3] };
};

export const BigNumber: React.FC<BigNumberProps> = ({ background, value, label, caption, kicker }) => {
  const frame = useCurrentFrame();
  const s = split(value);
  const t = interpolate(frame, [8, 50], [0, 1], { ...clamp, easing: out });
  const shown = s ? s.pre + (s.dec ? (s.n * t).toFixed(s.dec) : fmtNum(Math.round(s.n * t))) + s.post : value;
  const size = Math.max(150, Math.min(300, 2600 / Math.max(4, (value || "").length)));
  return (
    <Stage background={background}>
      <div style={{ position: "absolute", inset: 0, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", fontFamily: FONT, textTransform: "uppercase", textShadow, textAlign: "center" }}>
        {kicker ? <div style={{ fontWeight: 500, fontSize: 36, letterSpacing: 6, color: AMBER, ...fadeUp(frame, 2, -12) }}>{kicker}</div> : null}
        <div style={{ fontWeight: 700, fontSize: size, lineHeight: 1, color: AMBER, letterSpacing: 2, opacity: interpolate(frame, [4, 14], [0, 1], clamp), scale: String(interpolate(frame, [4, 30], [0.86, 1], { ...clamp, easing: out })) }}>{shown}</div>
        {label ? <div style={{ fontWeight: 700, fontSize: 64, color: TEXT, letterSpacing: 1.5, marginTop: 18, maxWidth: 1500, ...fadeUp(frame, 40) }}>{label}</div> : null}
        {caption ? <div style={{ fontWeight: 500, fontSize: 38, color: TEXT, opacity: 0.8, letterSpacing: 1, marginTop: 14, maxWidth: 1400, ...fadeUp(frame, 56) }}>{caption}</div> : null}
      </div>
    </Stage>
  );
};
