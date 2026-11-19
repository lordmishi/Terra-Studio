// Hand-drawn silhouettes. Points traced by hand, smoothed with Catmull-Rom.
type P = [number, number];

// Converts a closed loop of points into a smooth SVG path.
// Points tagged with a trailing `true` stay sharp (feet, ear tips).
type Pt = P | [number, number, true];

export const smooth = (pts: Pt[], tension = 0.5): string => {
  const n = pts.length;
  const get = (i: number) => pts[(i + n) % n];
  let d = `M ${get(0)[0]} ${get(0)[1]}`;
  for (let i = 0; i < n; i++) {
    const p0 = get(i - 1);
    const p1 = get(i);
    const p2 = get(i + 1);
    const p3 = get(i + 2);
    const s1 = p1.length === 3 ? 0 : tension / 3;
    const s2 = p2.length === 3 ? 0 : tension / 3;
    const c1x = p1[0] + (p2[0] - p0[0]) * s1;
    const c1y = p1[1] + (p2[1] - p0[1]) * s1;
    const c2x = p2[0] - (p3[0] - p1[0]) * s2;
    const c2y = p2[1] - (p3[1] - p1[1]) * s2;
    d += ` C ${c1x.toFixed(1)} ${c1y.toFixed(1)} ${c2x.toFixed(1)} ${c2y.toFixed(1)} ${p2[0]} ${p2[1]}`;
  }
  return d + " Z";
};

// Units: ground at y=200, shoulder top at y=100 (shoulder height = 100 units).
// Facing left, nose near x=0.

// Modern spotted hyena: lean, long legs, sloping back, big rounded ears, hanging tail.
export const spottedNear = smooth([
  [1, 122, true], [3, 116], [12, 112], [21, 108], [27, 101], [32, 97],
  [31, 89], [33, 80], [39, 76], [45, 79], [47, 88], [48, 96],
  [56, 97], [66, 96], [78, 99],
  [100, 106], [122, 114], [140, 121], [150, 126],
  [155, 129], [160, 136], [163, 147], [162, 160, true], [156, 156], [154, 145], [151, 138],
  [152, 152], [149, 167], [150, 178], [147, 194], [149, 200, true], [137, 200, true],
  [139, 190], [140, 178], [137, 160], [130, 148],
  [120, 147], [106, 150], [92, 153],
  [86, 156], [83, 172], [82, 194], [84, 200, true], [71, 200, true],
  [73, 192], [74, 175], [72, 160],
  [67, 153], [59, 147], [50, 140], [43, 138],
  [35, 139], [24, 136], [13, 132], [4, 128],
]);

// Far-side legs, drawn behind for depth.
export const spottedFar = smooth([
  [86, 148], [91, 160], [92, 195], [94, 200, true], [82, 200, true],
  [82, 192], [82, 172], [79, 152],
]) + " " + smooth([
  [124, 142], [133, 158], [136, 176], [134, 195], [136, 200, true], [125, 200, true],
  [126, 190], [126, 178], [119, 158],
]);

// Pachycrocuta: heavier head and jaw, thick maned neck, deeper chest, stockier limbs, short tail.
export const pachyNear = smooth([
  [0, 124, true], [2, 116], [12, 111], [21, 106], [27, 99], [32, 95],
  [31, 88], [33, 81], [39, 78], [45, 80], [47, 88], [49, 94],
  [57, 94], [68, 93], [81, 98],
  [104, 104], [126, 113], [144, 121], [154, 127],
  [159, 131], [163, 140], [164, 153, true], [157, 150], [155, 142], [152, 137],
  [155, 151], [154, 164], [153, 178], [150, 196], [153, 200, true], [136, 200, true],
  [138, 190], [139, 177], [136, 163], [128, 154],
  [114, 155], [100, 158],
  [90, 162], [87, 180], [87, 196], [90, 200, true], [69, 200, true],
  [71, 192], [72, 178], [69, 165],
  [63, 159], [55, 152], [47, 145], [41, 143],
  [33, 145], [21, 141], [10, 136], [3, 131],
]);

export const pachyFar = smooth([
  [86, 152], [94, 165], [95, 196], [98, 200, true], [80, 200, true],
  [80, 190], [80, 174], [78, 156],
]) + " " + smooth([
  [122, 148], [132, 163], [135, 180], [133, 197], [136, 200, true], [120, 200, true],
  [121, 190], [122, 178], [115, 160],
]);

// Eye position and mouth line, drawn in a darker shade by the caller.
export const spottedEye: P = [22, 113];
export const spottedMouth = "M 4 127 Q 14 128 24 127";
export const pachyEye: P = [21, 111];
export const pachyMouth = "M 3 130 Q 15 131 27 129";

// Adult man in profile facing right. Units: feet at y=1000, crown at y=0.
export const manBody = smooth([
  [6, 0], [38, 12], [56, 42], [60, 78], [74, 100], [62, 112], [62, 128], [56, 146],
  [40, 160], [26, 172], [30, 190],
  [58, 220], [70, 290], [64, 380], [58, 460],
  [62, 560], [58, 700], [50, 820], [46, 930], [48, 960],
  [104, 978], [108, 1000, true], [-28, 1000, true], [-30, 970],
  [-28, 880], [-20, 760], [-28, 690], [-44, 580], [-56, 490],
  [-46, 400], [-54, 300], [-54, 225],
  [-36, 190], [-28, 168], [-34, 150], [-54, 104], [-52, 50], [-30, 12],
]);

export const manArm = smooth([
  [-34, 215], [6, 212], [20, 260], [24, 360], [30, 470], [34, 530],
  [22, 560], [2, 550], [-4, 480], [-14, 370], [-30, 270],
]);

// Land animals: ground y=200, shoulder y≈100, so 100 units = shoulder height. Facing left.
export const bigcat = smooth([
  [0, 118, true], [4, 110], [12, 104], [22, 100], [28, 92], [31, 86, true], [36, 92], [42, 96],
  [55, 98], [70, 100], [85, 98], [110, 101], [135, 103], [150, 104], [160, 108],
  [168, 112], [180, 120], [190, 135], [196, 152], [198, 168, true], [192, 164], [188, 148], [181, 132], [170, 122], [165, 122],
  [163, 135], [160, 150], [158, 170], [158, 194], [161, 200, true], [146, 200, true], [147, 190], [146, 172], [140, 155],
  [128, 148], [110, 146], [92, 146],
  [88, 152], [86, 175], [86, 194], [89, 200, true], [74, 200, true], [75, 190], [74, 170], [70, 152],
  [62, 142], [52, 136], [40, 132], [30, 130], [20, 130], [10, 128], [4, 124],
]);
export const bear = smooth([
  [0, 140, true], [6, 132], [14, 126], [24, 118], [30, 110], [34, 102, true], [40, 108], [48, 108],
  [62, 96], [80, 92], [98, 96], [120, 100], [142, 104], [160, 110], [168, 122], [172, 126, true], [168, 132],
  [166, 148], [164, 170], [164, 194], [168, 200, true], [146, 200, true], [146, 188], [144, 170], [138, 156],
  [120, 156], [100, 158],
  [92, 162], [90, 180], [90, 194], [94, 200, true], [70, 200, true], [70, 188], [70, 170], [66, 156],
  [56, 150], [44, 148], [32, 148], [20, 146], [10, 144], [3, 142],
]);
export const elephant = smooth([
  [6, 196, true], [2, 180], [0, 160], [2, 140], [6, 124], [12, 104], [20, 90], [32, 82], [44, 84], [52, 90],
  [70, 92], [95, 96], [120, 100], [140, 106], [150, 116], [153, 126], [156, 150, true], [151, 128], [148, 124],
  [148, 140], [146, 170], [146, 196], [148, 200, true], [124, 200, true], [124, 190], [124, 168], [120, 150],
  [104, 152], [88, 152],
  [84, 156], [82, 180], [82, 196], [84, 200, true], [58, 200, true], [58, 190], [58, 168], [54, 150],
  [44, 142], [34, 140], [26, 146], [21, 160], [18, 180], [16, 196], [14, 200, true],
]);
export const elephantTusk = "M 14 132 Q 6 156 28 168 Q 14 152 22 136 Z";
// Whale: measured by length (200 units, nose at x=0). Bird and person: measured by full height (200 units).
export const whale = smooth([
  [0, 112, true], [10, 100], [30, 92], [60, 88], [100, 88], [130, 92], [140, 86], [146, 78, true], [150, 92], [165, 100],
  [180, 106], [190, 96], [200, 86, true], [196, 106], [200, 126, true], [188, 116], [178, 112],
  [160, 118], [130, 126], [100, 130], [80, 132], [72, 148, true], [66, 132], [50, 130], [30, 126], [12, 120], [4, 116],
]);
export const bird = smooth([
  [0, 34, true], [6, 26], [14, 20], [24, 14], [34, 12], [44, 16], [50, 24], [54, 40], [60, 60], [76, 74], [100, 80], [126, 84],
  [146, 80], [158, 78, true], [150, 92], [132, 104], [118, 112], [110, 124], [104, 138], [100, 160], [100, 196], [104, 200, true],
  [86, 200, true], [92, 196], [92, 170], [90, 150], [80, 132], [66, 120], [52, 104], [42, 86], [38, 66], [36, 50], [30, 42], [18, 40], [8, 38],
]);

export type ShapeKey = "hyena" | "hyena-heavy" | "bigcat" | "bear" | "elephant" | "whale" | "bird";
// size: units that equal the measured size; top/bottom/width: the drawing's box in units.
export const SHAPES: Record<ShapeKey, { near: string; far?: string; extra?: string; eye?: [number, number]; size: number; top: number; width: number; measure: "shoulder" | "length" | "height" }> = {
  hyena: { near: spottedNear, far: spottedFar, eye: spottedEye, size: 100, top: 76, width: 163, measure: "shoulder" },
  "hyena-heavy": { near: pachyNear, far: pachyFar, eye: pachyEye, size: 100, top: 78, width: 164, measure: "shoulder" },
  bigcat: { near: bigcat, eye: [16, 110], size: 100, top: 86, width: 198, measure: "shoulder" },
  bear: { near: bear, eye: [20, 124], size: 100, top: 92, width: 172, measure: "shoulder" },
  elephant: { near: elephant, extra: elephantTusk, eye: [24, 106], size: 100, top: 82, width: 156, measure: "shoulder" },
  whale: { near: whale, eye: [16, 106], size: 200, top: 78, width: 200, measure: "length" },
  bird: { near: bird, eye: [22, 24], size: 200, top: 12, width: 158, measure: "height" },
};
