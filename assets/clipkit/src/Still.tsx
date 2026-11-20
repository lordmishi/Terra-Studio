import React from "react";
import { AbsoluteFill, Img, interpolate, staticFile, useCurrentFrame, useVideoConfig } from "remotion";
import { clamp, inOut } from "./kit";

export type StillProps = { seconds: number; src: string; move?: "in" | "out" | "left" | "right" };

// A slow camera move over one picture, full frame, no AI.
export const Still: React.FC<StillProps> = ({ src, move = "in" }) => {
  const frame = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const t = interpolate(frame, [0, durationInFrames - 1], [0, 1], { ...clamp, easing: inOut });
  const scale = move === "out" ? 1.13 - 0.1 * t : move === "in" ? 1.03 + 0.1 * t : 1.12;
  const x = move === "left" ? 4 - 8 * t : move === "right" ? -4 + 8 * t : 0;
  return (
    <AbsoluteFill style={{ backgroundColor: "black", overflow: "hidden" }}>
      <Img src={staticFile(src)} style={{ width: "100%", height: "100%", objectFit: "cover", transform: `scale(${scale}) translateX(${x}%)` }} />
    </AbsoluteFill>
  );
};
