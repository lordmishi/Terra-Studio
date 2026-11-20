import React from "react";
import { CalculateMetadataFunction, Composition } from "remotion";
import { BarChart } from "./BarChart";
import { BigNumber } from "./BigNumber";
import { Custom } from "./custom/Custom";
import { MapRange } from "./MapRange";
import { Short } from "./Short";
import { SizeCompare } from "./SizeCompare";
import { Still } from "./Still";
import { Timeline } from "./Timeline";

const FPS = 30;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const meta: CalculateMetadataFunction<any> = ({ props }) => ({
  durationInFrames: Math.round(Math.min(20, Math.max(3, Number(props.seconds) || 6)) * FPS),
  defaultCodec: "prores",
  defaultVideoImageFormat: "png",
  defaultPixelFormat: "yuva444p10le",
  defaultProResProfile: "4444",
});
// Full-frame video (moving stills, Shorts) is H.264 and may run up to a minute.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const film: CalculateMetadataFunction<any> = ({ props }) => ({
  durationInFrames: Math.round(Math.min(60, Math.max(3, Number(props.seconds) || 8)) * FPS),
  defaultCodec: "h264",
});
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const C = (id: string, component: React.FC<any>) => (
  <Composition id={id} component={component} durationInFrames={180} fps={FPS} width={1920} height={1080} defaultProps={{ seconds: 6 }} calculateMetadata={meta} />
);
export const Root: React.FC = () => (
  <>
    {C("BigNumber", BigNumber)}
    {C("SizeCompare", SizeCompare)}
    {C("MapRange", MapRange)}
    {C("Timeline", Timeline)}
    {C("BarChart", BarChart)}
    {C("Custom", Custom)}
    <Composition id="Still" component={Still} durationInFrames={240} fps={FPS} width={1920} height={1080} defaultProps={{ seconds: 8, src: "", move: "in" as const }} calculateMetadata={film} />
    <Composition id="Short" component={Short} durationInFrames={900} fps={FPS} width={1080} height={1920} defaultProps={{ seconds: 30, src: "", words: [] }} calculateMetadata={film} />
  </>
);
