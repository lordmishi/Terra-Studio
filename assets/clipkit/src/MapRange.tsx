import { geoNaturalEarth1, geoPath } from "d3-geo";
import React from "react";
import { feature } from "topojson-client";
import landTopo from "world-atlas/land-50m.json";
import { interpolate, spring, useCurrentFrame, useVideoConfig } from "remotion";
import { AMBER, Base, FONT, Headline, LAND, Stage, TEXT, W, H, clamp, inOut, textShadow } from "./kit";

export type MapProps = Base & { regions?: string[]; pins?: { name: string; lat: number; lon: number }[]; caption?: string };

type Ring = [number, number][];
// Rough outlines; the land shape clips them, so only their inner edges matter.
const REGIONS: Record<string, Ring> = {
  africa: [[-18, 35], [12, 38], [34, 33], [44, 12], [52, 12], [42, -2], [41, -16], [34, -26], [26, -36], [17, -35], [12, -18], [8, -2], [-10, 4], [-18, 14]],
  europe: [[-11, 36], [-11, 44], [-10, 58], [5, 62], [20, 71], [40, 70], [55, 60], [50, 46], [40, 40], [28, 36], [14, 36], [2, 35]],
  "middle-east": [[28, 36], [40, 40], [50, 46], [62, 40], [64, 26], [57, 20], [50, 15], [43, 12], [38, 20], [34, 28]],
  asia: [[50, 46], [55, 60], [70, 72], [110, 76], [180, 70], [180, 60], [145, 50], [124, 41], [123, 30], [116, 22], [106, 10], [100, 2], [92, 21], [82, 8], [74, 18], [62, 25], [64, 40]],
  "east-asia": [[96, 46], [108, 50], [122, 54], [135, 48], [124, 41], [123, 30], [116, 22], [106, 18], [100, 22], [96, 30]],
  "north-america": [[-168, 66], [-140, 72], [-95, 76], [-60, 62], [-52, 47], [-80, 25], [-97, 16], [-118, 30], [-128, 48], [-150, 58]],
  "south-america": [[-80, 10], [-60, 12], [-35, -6], [-40, -22], [-58, -40], [-70, -56], [-76, -40], [-81, -6]],
  australia: [[113, -22], [130, -11], [143, -10], [154, -26], [150, -38], [140, -39], [130, -32], [115, -35]],
};
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const land = feature(landTopo as any, (landTopo as any).objects.land) as unknown as GeoJSON.FeatureCollection;
const poly = (r: Ring): GeoJSON.Feature => ({ type: "Feature", properties: {}, geometry: { type: "Polygon", coordinates: [[...r, r[0]]] } });

export const MapRange: React.FC<MapProps> = ({ background, kicker, title, regions = [], pins = [], caption }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const rs = regions.map((k) => REGIONS[k]).filter(Boolean);
  const pts: [number, number][] = [...rs.flat(), ...pins.map((p) => [p.lon, p.lat] as [number, number])];
  const lons = pts.map((p) => p[0]), lats = pts.map((p) => p[1]);
  const box = pts.length ? [Math.min(...lons) - 8, Math.max(...lats) + 6, Math.max(...lons) + 8, Math.min(...lats) - 6] : [-20, 66, 145, -34];
  const extent: GeoJSON.Feature = { type: "Feature", properties: {}, geometry: { type: "MultiPoint", coordinates: [[box[0], box[1]], [box[2], box[1]], [box[0], box[3]], [box[2], box[3]]] } };
  const top = title || kicker ? 260 : 60;
  const projection = geoNaturalEarth1().fitExtent([[120, top], [W - 120, H - 60]], extent);
  const path = geoPath(projection);
  const landD = path(land) ?? "";
  const xs = rs.flat().map((p) => (projection(p) as [number, number])[0]);
  const x0 = xs.length ? Math.min(...xs) : 0, x1 = xs.length ? Math.max(...xs) : W;
  const sweep = interpolate(frame, [20, 70], [0, 1], { ...clamp, easing: inOut });
  return (
    <Stage background={background}>
      <Headline kicker={kicker} title={title} align="left" />
      <svg width={W} height={H} style={{ position: "absolute" }}>
        <defs>
          <clipPath id="land"><path d={landD} /></clipPath>
          <clipPath id="sweep"><rect x={x0 - 20} y={0} width={(x1 - x0 + 40) * sweep} height={H} /></clipPath>
        </defs>
        <path d={landD} fill={LAND} fillOpacity={0.92} stroke={TEXT} strokeOpacity={0.18} strokeWidth={1.5} opacity={interpolate(frame, [0, 18], [0, 1], clamp)} />
        <g clipPath="url(#land)">
          <g clipPath="url(#sweep)">
            {rs.map((r, i) => <path key={i} d={path(poly(r)) ?? ""} fill={AMBER} fillOpacity={0.55} />)}
          </g>
        </g>
        {pins.slice(0, 6).map((p, i) => {
          const xy = projection([p.lon, p.lat]);
          if (!xy) return null;
          const s = spring({ frame: frame - 70 - i * 10, fps, config: { damping: 12 } });
          const pulse = interpolate((frame - 80 - i * 10) % 40, [0, 40], [0, 1], clamp);
          return (
            <g key={i} transform={`translate(${xy[0]} ${xy[1]})`}>
              {frame > 80 + i * 10 ? <circle r={14 + 30 * pulse} fill="none" stroke={AMBER} strokeWidth={3} opacity={1 - pulse} /> : null}
              <circle r={14 * s} fill={AMBER} stroke="#1B1916" strokeWidth={3} />
            </g>
          );
        })}
      </svg>
      {pins.slice(0, 6).map((p, i) => {
        const xy = projection([p.lon, p.lat]);
        if (!xy) return null;
        return <div key={i} style={{ position: "absolute", left: xy[0] - 220, width: 440, top: xy[1] + 22, textAlign: "center", fontFamily: FONT, fontWeight: 700, fontSize: 36, color: TEXT, textTransform: "uppercase", letterSpacing: 1, textShadow, opacity: interpolate(frame, [80 + i * 10, 94 + i * 10], [0, 1], clamp) }}>{p.name}</div>;
      })}
      {caption ? <div style={{ position: "absolute", left: 120, right: 120, bottom: 70, textAlign: "center", fontFamily: FONT, fontWeight: 500, fontSize: 38, color: TEXT, textTransform: "uppercase", letterSpacing: 2, textShadow, opacity: interpolate(frame, [100, 116], [0, 1], clamp) }}>{caption}</div> : null}
    </Stage>
  );
};
