import { loadFont } from "@remotion/fonts";
import React from "react";
import { AbsoluteFill, Easing, interpolate, staticFile, useCurrentFrame } from "remotion";

export const AMBER = "#F2A230";
export const TEXT = "#EDE6DA";
export const STONE = "#1B1916";
export const MUTED = "#9A9187";
export const LAND = "#2B2722";
export const FONT = "Oswald";
export const W = 1920;
export const H = 1080;

loadFont({ family: FONT, url: staticFile("oswald-latin-700-normal.woff2"), weight: "700" });
loadFont({ family: FONT, url: staticFile("oswald-latin-500-normal.woff2"), weight: "500" });

export const clamp = { extrapolateLeft: "clamp", extrapolateRight: "clamp" } as const;
export const out = Easing.bezier(0.16, 1, 0.3, 1);
export const inOut = Easing.bezier(0.45, 0, 0.2, 1);
export const shadow = "drop-shadow(0 4px 14px rgba(0,0,0,0.55))";
export const textShadow = "0 2px 10px rgba(0,0,0,0.7)";

export type Base = { seconds: number; background?: boolean; title?: string; kicker?: string };

// Transparent by default; background only for the preview picture.
export const Stage: React.FC<{ background?: boolean; children: React.ReactNode }> = ({ background, children }) => (
  <AbsoluteFill style={{ backgroundColor: background ? STONE : "transparent" }}>
    {background ? (
      <AbsoluteFill
        style={{
          background:
            "radial-gradient(ellipse at 50% 45%, rgba(242,162,48,0.07), transparent 60%), radial-gradient(ellipse at 50% 120%, #2a2621, transparent 70%)",
        }}
      />
    ) : null}
    {children}
  </AbsoluteFill>
);

export const fadeUp = (frame: number, start: number, dist = 18) => ({
  opacity: interpolate(frame, [start, start + 14], [0, 1], clamp),
  translate: interpolate(frame, [start, start + 18], [`0px ${dist}px`, "0px 0px"], { ...clamp, easing: out }),
});

export const Headline: React.FC<{ kicker?: string; title?: string; align?: "left" | "right" | "center" }> = ({ kicker, title, align = "right" }) => {
  const frame = useCurrentFrame();
  if (!kicker && !title) return null;
  const pos: React.CSSProperties = align === "right" ? { right: 120, textAlign: "right" } : align === "left" ? { left: 120, textAlign: "left" } : { left: 0, right: 0, textAlign: "center" };
  return (
    <div style={{ position: "absolute", top: 96, ...pos, fontFamily: FONT, textTransform: "uppercase", textShadow, ...fadeUp(frame, 4, -16) }}>
      {kicker ? <div style={{ fontWeight: 500, fontSize: 34, letterSpacing: 6, color: AMBER }}>{kicker}</div> : null}
      {title ? <div style={{ fontWeight: 700, fontSize: 84, lineHeight: 1.0, color: TEXT, letterSpacing: 1, maxWidth: 1500 }}>{title}</div> : null}
    </div>
  );
};

export const Label: React.FC<{
  x: number; y: number; title: string; lines?: string[]; color?: string; start: number;
  align?: "left" | "center" | "right"; width?: number; size?: number;
}> = ({ x, y, title, lines = [], color = TEXT, start, align = "left", width = 520, size = 46 }) => {
  const frame = useCurrentFrame();
  const left = align === "center" ? x - width / 2 : align === "right" ? x - width : x;
  return (
    <div style={{ position: "absolute", left, top: y, width, textAlign: align, fontFamily: FONT, textTransform: "uppercase", textShadow, ...fadeUp(frame, start) }}>
      <div style={{ fontWeight: 700, fontSize: size, color, letterSpacing: 1.5, lineHeight: 1.05 }}>{title}</div>
      {lines.filter(Boolean).map((l, i) => (
        <div key={i} style={{ fontWeight: 500, fontSize: Math.round(size * 0.74), color: TEXT, letterSpacing: 1, lineHeight: 1.25, marginTop: i === 0 ? 6 : 0, opacity: interpolate(frame, [start + 6 + i * 8, start + 20 + i * 8], [0, 1], clamp) }}>
          {l}
        </div>
      ))}
    </div>
  );
};

export const fmtNum = (n: number) => {
  const a = Math.abs(n);
  const d = a >= 100 || Number.isInteger(n) ? 0 : a >= 10 ? 1 : 2;
  return n.toLocaleString("en-US", { maximumFractionDigits: d, minimumFractionDigits: 0 });
};
