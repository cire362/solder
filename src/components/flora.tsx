"use client";

import { flower, type FlowerSpec } from "@/lib/flora";
import { gsap } from "@/lib/gsap";

/*
  Botanical line art rendered from specs (see lib/flora). Markup is the fully bloomed
  frame, which is also the reduced-motion and no-JS state. `growFlowers` builds the GSAP
  choreography: stem draws, leaves unfold along it, petals open around the head.
*/

export function Flower({ spec, index = 0 }: { spec: FlowerSpec; index?: number }) {
  const g = flower(spec);
  return (
    <g data-flower data-index={index}>
      <path
        data-stem
        d={g.stem}
        fill="none"
        stroke="var(--fg-subtle)"
        strokeWidth={1.4}
        strokeLinecap="round"
      />
      {g.leaves.map((l, i) => (
        <g key={i} data-leaf data-ox={l.origin[0]} data-oy={l.origin[1]} data-t={l.t}>
          <path d={l.d} fill="var(--bg)" stroke="var(--fg-subtle)" strokeWidth={1.2} strokeLinejoin="round" />
          <path d={l.rib} fill="none" stroke="var(--fg-subtle)" strokeWidth={0.8} strokeLinecap="round" opacity={0.7} />
        </g>
      ))}
      {g.petals.map((pt, i) => (
        <path
          key={i}
          data-petal
          data-ox={pt.origin[0]}
          data-oy={pt.origin[1]}
          d={pt.d}
          fill="var(--accent-soft)"
          stroke="var(--accent)"
          strokeWidth={1.2}
          strokeLinejoin="round"
        />
      ))}
      {g.center && (
        <g data-center data-ox={g.center.at[0]} data-oy={g.center.at[1]}>
          <circle cx={g.center.at[0]} cy={g.center.at[1]} r={g.center.r} fill="var(--accent)" />
          <circle
            cx={g.center.at[0]}
            cy={g.center.at[1]}
            r={g.center.r * 0.45}
            fill="none"
            stroke="var(--accent-fg)"
            strokeWidth={0.8}
            opacity={0.5}
          />
        </g>
      )}
    </g>
  );
}

const origin = (el: Element) => `${el.getAttribute("data-ox")} ${el.getAttribute("data-oy")}`;

/** Collapse every flower inside `scope` to its seed state. */
export function seedFlowers(scope: Element | null) {
  if (!scope) return;
  scope.querySelectorAll<SVGPathElement>("[data-stem]").forEach((stem) => {
    const len = stem.getTotalLength();
    // Hidden too: a round line cap would otherwise leave a dot at the seed point.
    gsap.set(stem, { strokeDasharray: len, strokeDashoffset: len, opacity: 0 });
  });
  scope.querySelectorAll("[data-leaf], [data-petal], [data-center]").forEach((el) => {
    gsap.set(el, { svgOrigin: origin(el), scale: 0, rotation: el.hasAttribute("data-petal") ? -28 : 0 });
  });
}

/** Add one flower's growth to `tl` starting at `at`. Returns the time it finishes blooming. */
export function growFlower(tl: gsap.core.Timeline, fl: Element, at: number, speed = 1) {
  const stem = fl.querySelector("[data-stem]");
  const draw = 1.1 * speed;
  tl.set(stem, { opacity: 1 }, at).to(stem, { strokeDashoffset: 0, duration: draw, ease: "power2.inOut" }, at);
  fl.querySelectorAll("[data-leaf]").forEach((leaf) => {
    const t = Number(leaf.getAttribute("data-t") ?? 0.5);
    tl.to(leaf, { scale: 1, duration: 0.5 * speed, ease: "back.out(1.7)" }, at + draw * t * 0.9);
  });
  const petals = fl.querySelectorAll("[data-petal]");
  const open = at + draw * 0.92;
  tl.to(petals, { scale: 1, rotation: 0, duration: 0.7 * speed, ease: "back.out(1.4)", stagger: 0.035 * speed }, open);
  const center = fl.querySelector("[data-center]");
  if (center) tl.to(center, { scale: 1, duration: 0.4 * speed, ease: "back.out(2.2)" }, open + 0.25 * speed);
  return open + 0.7 * speed + petals.length * 0.035 * speed;
}

/** Grow every flower in `scope`, staggered by `gap`. */
export function growFlowers(tl: gsap.core.Timeline, scope: Element | null, at = 0, gap = 0.22) {
  if (!scope) return;
  scope.querySelectorAll("[data-flower]").forEach((fl, i) => growFlower(tl, fl, at + i * gap));
}
