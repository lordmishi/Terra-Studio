import React from "react";
import { AbsoluteFill, OffthreadVideo, staticFile, useCurrentFrame, useVideoConfig } from "remotion";
import { AMBER, FONT, TEXT, fadeUp } from "./kit";

export type Word = { text: string; start: number; end: number };
export type ShortProps = { seconds: number; src: string; words?: Word[]; title?: string };

// Words grouped into short pages, so at most 3 words sit on screen at a time.
const pages = (words: Word[]) => {
  const out: Word[][] = [];
  let cur: Word[] = [];
  for (const w of words) {
    const t = w.text.trim();
    if (!t) continue;
    cur.push({ ...w, text: t });
    if (cur.length >= 3 || /[.!?,]$/.test(t)) {
      out.push(cur);
      cur = [];
    }
  }
  if (cur.length) out.push(cur);
  return out;
};

// 1080x1920: the film in the middle over a blurred copy of itself, big captions below.
export const Short: React.FC<ShortProps> = ({ src, words = [], title }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const now = frame / fps;
  const ps = pages(words);
  const page = ps.find((p, i) => now >= p[0].start && now < (ps[i + 1]?.[0].start ?? p[p.length - 1].end + 0.6));
  const video = staticFile(src);
  return (
    <AbsoluteFill style={{ backgroundColor: "black" }}>
      <AbsoluteFill style={{ overflow: "hidden" }}>
        <OffthreadVideo src={video} muted style={{ width: "100%", height: "100%", objectFit: "cover", filter: "blur(36px) brightness(0.45)", transform: "scale(1.15)" }} />
      </AbsoluteFill>
      {title ? (
        <div style={{ position: "absolute", top: 230, left: 70, right: 70, textAlign: "center", fontFamily: FONT, fontWeight: 700, fontSize: 64, lineHeight: 1.05, color: TEXT, textTransform: "uppercase", textShadow: "0 3px 14px rgba(0,0,0,.8)", ...fadeUp(frame, 0) }}>
          {title}
        </div>
      ) : null}
      <div style={{ position: "absolute", top: 656, left: 0, width: 1080, height: 608, boxShadow: "0 20px 60px rgba(0,0,0,.6)" }}>
        <OffthreadVideo src={video} style={{ width: "100%", height: "100%", objectFit: "cover" }} />
      </div>
      {page ? (
        <div style={{ position: "absolute", top: 1360, left: 60, right: 60, textAlign: "center", fontFamily: FONT, fontWeight: 700, fontSize: 96, lineHeight: 1.05, textTransform: "uppercase", textShadow: "0 4px 18px rgba(0,0,0,.9)" }}>
          {page.map((w, i) => (
            <span key={i} style={{ color: now >= w.start && now < w.end + 0.05 ? AMBER : TEXT }}>
              {w.text}{" "}
            </span>
          ))}
        </div>
      ) : null}
    </AbsoluteFill>
  );
};
