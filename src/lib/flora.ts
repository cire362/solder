// Generative botanical line art. Every flower is computed from a small spec: a cubic stem,
// leaves placed along it by the curve's tangent, and petals rotated around the head.
// Output is plain SVG path data, so drawings stay crisp at any size.

export type Pt = [number, number];
export type FlowerKind = "daisy" | "cosmos" | "tulip" | "lavender";

export type FlowerSpec = {
  kind: FlowerKind;
  /** ground point */
  x: number;
  y: number;
  /** stem height and sideways lean of the head */
  h: number;
  lean: number;
  /** head size (petal length) */
  size: number;
  leaves?: { t: number; side: 1 | -1; len: number }[];
};

export type Petal = { d: string; origin: Pt };
export type Leaf = { d: string; rib: string; origin: Pt; t: number };
export type FlowerGeometry = {
  stem: string;
  leaves: Leaf[];
  petals: Petal[];
  center: { at: Pt; r: number } | null;
};

const rad = (deg: number) => (deg * Math.PI) / 180;
const dir = (deg: number): Pt => [Math.cos(rad(deg)), Math.sin(rad(deg))];
const add = (a: Pt, b: Pt): Pt => [a[0] + b[0], a[1] + b[1]];
const mul = (a: Pt, k: number): Pt => [a[0] * k, a[1] * k];
const n = (v: number) => v.toFixed(1);
// Coordinates that end up in markup are rounded so server and client render identical
// attributes (floating point results can differ in the last digits between engines).
const r2 = (v: number) => Math.round(v * 100) / 100;
const round = (a: Pt): Pt => [r2(a[0]), r2(a[1])];
const p = (a: Pt) => `${n(a[0])} ${n(a[1])}`;

/** Almond outline from `base` towards `angle`: the shape of every petal, bud and leaf. */
function almond(base: Pt, angle: number, len: number, width: number) {
  const d = dir(angle);
  const q = dir(angle + 90);
  const at = (along: number, side: number): Pt => add(add(base, mul(d, len * along)), mul(q, width * side));
  const tip = add(base, mul(d, len));
  return `M${p(base)}C${p(at(0.25, 1))} ${p(at(0.8, 0.7))} ${p(tip)}C${p(at(0.8, -0.7))} ${p(at(0.25, -1))} ${p(base)}Z`;
}

export function bezier(P: Pt[], t: number): { at: Pt; angle: number } {
  // Plain multiplication instead of `**`: Math.pow is not guaranteed to round identically
  // across JS engines, which caused SSR hydration mismatches.
  const u = 1 - t;
  const a = u * u * u;
  const b = 3 * u * u * t;
  const c = 3 * u * t * t;
  const d = t * t * t;
  const at: Pt = [
    a * P[0][0] + b * P[1][0] + c * P[2][0] + d * P[3][0],
    a * P[0][1] + b * P[1][1] + c * P[2][1] + d * P[3][1],
  ];
  const dx = 3 * u * u * (P[1][0] - P[0][0]) + 6 * u * t * (P[2][0] - P[1][0]) + 3 * t * t * (P[3][0] - P[2][0]);
  const dy = 3 * u * u * (P[1][1] - P[0][1]) + 6 * u * t * (P[2][1] - P[1][1]) + 3 * t * t * (P[3][1] - P[2][1]);
  return { at, angle: (Math.atan2(dy, dx) * 180) / Math.PI };
}

export function flower(spec: FlowerSpec): FlowerGeometry {
  const { x, y, h, lean, size, kind } = spec;
  const P: Pt[] = [
    [x, y],
    [x + lean * 0.15, y - h * 0.4],
    [x + lean * 0.85, y - h * 0.78],
    [x + lean, y - h],
  ];
  const stem = `M${p(P[0])}C${p(P[1])} ${p(P[2])} ${p(P[3])}`;
  const head = round(P[3]);
  const top = bezier(P, 1).angle; // direction the stem points at the head

  const leaves: Leaf[] = (spec.leaves ?? []).map(({ t, side, len }) => {
    const { at, angle } = bezier(P, t);
    const a = angle + side * 52;
    return {
      d: almond(at, a, len, len * 0.3),
      rib: `M${p(at)}L${p(add(at, mul(dir(a), len * 0.82)))}`,
      origin: round(at),
      t,
    };
  });

  const petals: Petal[] = [];
  let center: FlowerGeometry["center"] = null;

  if (kind === "daisy" || kind === "cosmos") {
    const count = kind === "daisy" ? 14 : 8;
    const width = kind === "daisy" ? 0.16 : 0.36;
    const inset = kind === "daisy" ? 0.2 : 0.14;
    for (let i = 0; i < count; i++) {
      const a = (360 / count) * i - 90;
      petals.push({ d: almond(add(head, mul(dir(a), size * inset)), a, size, size * width), origin: head });
    }
    center = { at: head, r: r2(size * (kind === "daisy" ? 0.26 : 0.2)) };
  } else if (kind === "tulip") {
    // Side view: three cupped petals opening along the stem direction.
    for (const off of [-26, 26, 0]) {
      petals.push({ d: almond(head, top + off, size * 1.25, size * 0.46), origin: head });
    }
  } else {
    // Lavender: small buds alternating along the top of the stem.
    for (let i = 0; i < 9; i++) {
      const { at, angle } = bezier(P, 0.58 + i * 0.05);
      const a = angle + (i % 2 ? 38 : -38);
      petals.push({ d: almond(at, a, size, size * 0.38), origin: round(at) });
    }
  }

  return { stem, leaves, petals, center };
}
