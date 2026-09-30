"use client";

import { useRef } from "react";
import type { FlowerSpec } from "@/lib/flora";
import { gsap, useGSAP } from "@/lib/gsap";
import { Flower, growFlowers, seedFlowers } from "./flora";

/*
  Reusable bouquets for secondary pages. Each preset is drawn on a 300 x 240 ground
  (flowers stand on y = 240) except the fleuron, a small symmetric divider ornament.
  trigger "load": grows once the page settles (page headers, sidebars).
  trigger "view": grows when it scrolls into view (dividers further down a page).
*/

const L = (t: number, side: 1 | -1, len: number) => ({ t, side, len });

const PRESETS = {
  meadow: {
    box: [300, 240],
    flowers: [
      { kind: "daisy", x: 60, y: 240, h: 170, lean: 14, size: 22, leaves: [L(0.3, -1, 28), L(0.55, 1, 24)] },
      { kind: "lavender", x: 110, y: 240, h: 210, lean: -8, size: 7, leaves: [L(0.22, 1, 22)] },
      { kind: "daisy", x: 165, y: 240, h: 120, lean: -12, size: 16, leaves: [L(0.4, 1, 20)] },
      { kind: "cosmos", x: 220, y: 240, h: 165, lean: -20, size: 19, leaves: [L(0.35, -1, 24)] },
      { kind: "tulip", x: 270, y: 240, h: 95, lean: 6, size: 13, leaves: [L(0.35, 1, 22)] },
    ],
  },
  tulips: {
    box: [300, 240],
    flowers: [
      { kind: "tulip", x: 70, y: 240, h: 180, lean: 12, size: 20, leaves: [L(0.3, -1, 34)] },
      { kind: "tulip", x: 130, y: 240, h: 140, lean: -6, size: 17, leaves: [L(0.35, 1, 30)] },
      { kind: "tulip", x: 185, y: 240, h: 205, lean: -14, size: 22, leaves: [L(0.25, 1, 36), L(0.5, -1, 28)] },
      { kind: "lavender", x: 245, y: 240, h: 150, lean: 8, size: 6, leaves: [L(0.3, -1, 18)] },
    ],
  },
  lavender: {
    box: [300, 240],
    flowers: [
      { kind: "lavender", x: 60, y: 240, h: 200, lean: 14, size: 8, leaves: [L(0.25, -1, 22)] },
      { kind: "lavender", x: 100, y: 240, h: 230, lean: 4, size: 8, leaves: [L(0.2, 1, 22)] },
      { kind: "lavender", x: 140, y: 240, h: 175, lean: -8, size: 7 },
      { kind: "daisy", x: 200, y: 240, h: 115, lean: -10, size: 15, leaves: [L(0.4, 1, 20)] },
      { kind: "lavender", x: 250, y: 240, h: 160, lean: -16, size: 7, leaves: [L(0.3, 1, 18)] },
    ],
  },
  wild: {
    box: [300, 240],
    flowers: [
      { kind: "cosmos", x: 60, y: 240, h: 150, lean: 16, size: 20, leaves: [L(0.35, -1, 26)] },
      { kind: "daisy", x: 120, y: 240, h: 210, lean: -6, size: 24, leaves: [L(0.25, 1, 30), L(0.5, -1, 26)] },
      { kind: "tulip", x: 180, y: 240, h: 120, lean: 8, size: 15, leaves: [L(0.35, 1, 24)] },
      { kind: "cosmos", x: 240, y: 240, h: 185, lean: -18, size: 17, leaves: [L(0.3, -1, 24)] },
    ],
  },
  cosmos: {
    box: [300, 240],
    flowers: [
      { kind: "cosmos", x: 80, y: 240, h: 190, lean: 14, size: 24, leaves: [L(0.3, -1, 30), L(0.52, 1, 24)] },
      { kind: "cosmos", x: 160, y: 240, h: 140, lean: -10, size: 18, leaves: [L(0.4, 1, 22)] },
      { kind: "daisy", x: 225, y: 240, h: 100, lean: -12, size: 13 },
      { kind: "lavender", x: 272, y: 240, h: 170, lean: -10, size: 6, leaves: [L(0.3, -1, 18)] },
    ],
  },
  sprig: {
    box: [300, 240],
    flowers: [
      { kind: "daisy", x: 110, y: 240, h: 150, lean: 12, size: 18, leaves: [L(0.3, -1, 24), L(0.55, 1, 20)] },
      { kind: "lavender", x: 165, y: 240, h: 190, lean: -8, size: 7, leaves: [L(0.25, 1, 18)] },
      { kind: "tulip", x: 215, y: 240, h: 100, lean: -6, size: 13, leaves: [L(0.4, -1, 20)] },
    ],
  },
  fleuron: {
    box: [240, 110],
    flowers: [
      { kind: "tulip", x: 120, y: 108, h: 58, lean: -30, size: 11, leaves: [L(0.45, -1, 14)] },
      { kind: "tulip", x: 120, y: 108, h: 58, lean: 30, size: 11, leaves: [L(0.45, 1, 14)] },
      { kind: "daisy", x: 120, y: 108, h: 82, lean: 0, size: 15 },
    ],
  },
} satisfies Record<string, { box: [number, number]; flowers: FlowerSpec[] }>;

export type FloraPreset = keyof typeof PRESETS;

export function FloraCluster({
  preset,
  trigger = "load",
  flip = false,
  delay = 0.5,
  className = "",
}: {
  preset: FloraPreset;
  trigger?: "load" | "view";
  flip?: boolean;
  delay?: number;
  className?: string;
}) {
  const svg = useRef<SVGSVGElement>(null);
  const { box, flowers } = PRESETS[preset];

  useGSAP(
    () => {
      const mm = gsap.matchMedia();
      mm.add("(prefers-reduced-motion: no-preference)", () => {
        seedFlowers(svg.current);
        gsap.set(svg.current, { opacity: 1 });
        const tl = gsap.timeline(
          trigger === "view"
            ? { scrollTrigger: { trigger: svg.current, start: "top 90%", once: true } }
            : { delay },
        );
        growFlowers(tl, svg.current, 0, preset === "fleuron" ? 0.12 : 0.26);
      });
      return () => mm.revert();
    },
    { scope: svg },
  );

  return (
    <svg
      ref={svg}
      viewBox={`0 0 ${box[0]} ${box[1] + 2}`}
      aria-hidden
      className={`flora-anim pointer-events-none overflow-visible ${flip ? "-scale-x-100" : ""} ${className}`}
    >
      {flowers.map((s, i) => (
        <Flower key={i} spec={s} index={i} />
      ))}
    </svg>
  );
}
