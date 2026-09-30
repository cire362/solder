"use client";

import { useRef } from "react";
import { bezier, type FlowerSpec, type Pt } from "@/lib/flora";
import { gsap, useGSAP } from "@/lib/gsap";
import { Flower, growFlower, growFlowers, seedFlowers } from "./flora";

const MOTION_OK = "(prefers-reduced-motion: no-preference)";

/* ------------------------------------------------------------------ hero */

const HERO: FlowerSpec[] = [
  { kind: "tulip", x: 34, y: 260, h: 112, lean: 10, size: 16, leaves: [{ t: 0.32, side: 1, len: 30 }] },
  { kind: "daisy", x: 86, y: 260, h: 196, lean: 20, size: 25, leaves: [{ t: 0.3, side: -1, len: 32 }, { t: 0.55, side: 1, len: 26 }] },
  { kind: "lavender", x: 150, y: 260, h: 232, lean: -12, size: 8, leaves: [{ t: 0.22, side: 1, len: 24 }] },
  { kind: "cosmos", x: 214, y: 260, h: 150, lean: -24, size: 21, leaves: [{ t: 0.4, side: -1, len: 26 }] },
  { kind: "daisy", x: 268, y: 260, h: 92, lean: -10, size: 13 },
];

/** A small cluster that grows in the hero's corner once the page has settled. */
export function HeroFlora({ className = "" }: { className?: string }) {
  const svg = useRef<SVGSVGElement>(null);

  useGSAP(
    () => {
      const mm = gsap.matchMedia();
      mm.add(MOTION_OK, () => {
        seedFlowers(svg.current);
        gsap.set(svg.current, { opacity: 1 });
        const tl = gsap.timeline({ delay: 1.1 });
        growFlowers(tl, svg.current, 0, 0.28);
      });
      return () => mm.revert();
    },
    { scope: svg },
  );

  return (
    <svg ref={svg} viewBox="0 0 320 262" aria-hidden className={`hero-flora overflow-visible ${className}`}>
      {HERO.map((s, i) => (
        <Flower key={i} spec={s} index={i} />
      ))}
    </svg>
  );
}

/* ------------------------------------------------------------------ final CTA */

const CTA_LEFT: FlowerSpec[] = [
  { kind: "lavender", x: 60, y: 440, h: 330, lean: 18, size: 10, leaves: [{ t: 0.2, side: -1, len: 34 }, { t: 0.38, side: 1, len: 30 }] },
  { kind: "daisy", x: 130, y: 440, h: 380, lean: 36, size: 38, leaves: [{ t: 0.25, side: 1, len: 44 }, { t: 0.48, side: -1, len: 40 }] },
  { kind: "tulip", x: 210, y: 440, h: 230, lean: -14, size: 26, leaves: [{ t: 0.3, side: -1, len: 46 }] },
  { kind: "cosmos", x: 272, y: 440, h: 160, lean: 20, size: 22, leaves: [{ t: 0.4, side: 1, len: 28 }] },
];

const CTA_RIGHT: FlowerSpec[] = [
  { kind: "cosmos", x: 90, y: 440, h: 300, lean: -30, size: 34, leaves: [{ t: 0.3, side: -1, len: 42 }, { t: 0.55, side: 1, len: 34 }] },
  { kind: "daisy", x: 170, y: 440, h: 210, lean: 14, size: 24, leaves: [{ t: 0.35, side: 1, len: 34 }] },
  { kind: "lavender", x: 240, y: 440, h: 350, lean: -20, size: 10, leaves: [{ t: 0.22, side: 1, len: 32 }] },
  { kind: "tulip", x: 300, y: 440, h: 170, lean: 8, size: 22, leaves: [{ t: 0.35, side: -1, len: 36 }] },
];

/**
 * Two sprigs that grow up the sides of the final CTA as it scrolls in and bloom as the
 * logo halves join (same scroll range as SolderJoin).
 */
export function CtaFlora() {
  const root = useRef<HTMLDivElement>(null);

  useGSAP(
    () => {
      const mm = gsap.matchMedia();
      mm.add(MOTION_OK, () => {
        const [left, right] = gsap.utils.toArray<SVGSVGElement>("svg", root.current);
        seedFlowers(root.current);
        const tl = gsap.timeline({
          scrollTrigger: { trigger: root.current, start: "top 95%", end: "top 25%", scrub: 0.8 },
        });
        growFlowers(tl, left, 0, 0.3);
        growFlowers(tl, right, 0.15, 0.3);
      });
      return () => mm.revert();
    },
    { scope: root },
  );

  const svgClass = "absolute bottom-0 h-[300px] w-auto overflow-visible md:h-[380px] xl:h-[440px]";
  return (
    <div ref={root} aria-hidden className="pointer-events-none absolute inset-0 hidden md:block">
      <svg viewBox="0 0 360 442" className={`${svgClass} -left-6 lg:left-[2%]`}>
        {CTA_LEFT.map((s, i) => (
          <Flower key={i} spec={s} index={i} />
        ))}
      </svg>
      <svg viewBox="0 0 360 442" className={`${svgClass} -right-6 lg:right-[2%]`}>
        {CTA_RIGHT.map((s, i) => (
          <Flower key={i} spec={s} index={i} />
        ))}
      </svg>
    </div>
  );
}

/* ------------------------------------------------------------------ plugin garden */

const VINE: Pt[] = [
  [4, 92],
  [170, 66],
  [390, 108],
  [556, 80],
];

const onVine = (t: number) => bezier(VINE, t).at;

const KINDS = ["daisy", "lavender", "cosmos", "tulip", "daisy", "cosmos", "lavender"] as const;
export const GARDEN_SLOTS = KINDS.length;

const GARDEN: FlowerSpec[] = KINDS.map((kind, i) => {
  const [x, y] = onVine(0.08 + i * 0.14);
  const tall = i % 2 === 0;
  return {
    kind,
    x,
    y,
    h: tall ? 58 : 40,
    lean: i % 3 === 0 ? 8 : -6,
    size: kind === "lavender" ? 5.5 : kind === "tulip" ? 9 : tall ? 13 : 10,
    leaves: [{ t: 0.35, side: i % 2 ? 1 : -1, len: 12 }],
  };
});

/**
 * A vine along the top of the plugin browser. Each installed plugin blooms one flower,
 * uninstalling closes it again: visible feedback that the editor is growing.
 */
export function PluginGarden({ count }: { count: number }) {
  const svg = useRef<SVGSVGElement>(null);
  const shown = useRef(0);
  const target = Math.min(count, GARDEN_SLOTS);

  // Mount: seed everything, draw the vine when it scrolls into view, then bloom what is installed.
  useGSAP(
    () => {
      const flowers = gsap.utils.toArray<Element>("[data-flower]", svg.current);
      const vine = svg.current?.querySelector<SVGPathElement>("[data-vine]");
      seedFlowers(svg.current);
      const mm = gsap.matchMedia();

      mm.add(MOTION_OK, () => {
        if (!vine) return;
        const len = vine.getTotalLength();
        gsap.set(vine, { strokeDasharray: len, strokeDashoffset: len, opacity: 0 });
        const tl = gsap.timeline({
          scrollTrigger: { trigger: svg.current, start: "top 85%", once: true },
        });
        tl.set(vine, { opacity: 1 }).to(vine, { strokeDashoffset: 0, duration: 1.2, ease: "power2.inOut" });
        flowers.slice(0, target).forEach((fl, i) => growFlower(tl, fl, 0.6 + i * 0.25, 0.7));
        shown.current = target;
      });

      mm.add("(prefers-reduced-motion: reduce)", () => {
        flowers.slice(0, target).forEach((fl) => bloomNow(fl));
        shown.current = target;
      });
    },
    { scope: svg },
  );

  // Installs and uninstalls after mount.
  useGSAP(
    () => {
      const flowers = gsap.utils.toArray<Element>("[data-flower]", svg.current);
      const from = shown.current;
      if (target === from) return;
      const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

      if (target > from) {
        const tl = gsap.timeline();
        for (let i = from; i < target; i++) {
          if (reduce) bloomNow(flowers[i]);
          else growFlower(tl, flowers[i], (i - from) * 0.15, 0.6);
        }
      } else {
        for (let i = target; i < from; i++) wilt(flowers[i], reduce);
      }
      shown.current = target;
    },
    { dependencies: [target], scope: svg },
  );

  return (
    <svg ref={svg} viewBox="0 0 560 112" aria-hidden className="block h-auto w-full overflow-visible">
      <path
        data-vine
        d={`M${VINE[0].join(" ")}C${VINE[1].join(" ")} ${VINE[2].join(" ")} ${VINE[3].join(" ")}`}
        fill="none"
        stroke="var(--line-strong)"
        strokeWidth={1.4}
        strokeLinecap="round"
      />
      {GARDEN.map((s, i) => (
        <Flower key={i} spec={s} index={i} />
      ))}
    </svg>
  );
}

function bloomNow(fl: Element) {
  gsap.set(fl.querySelector("[data-stem]"), { strokeDashoffset: 0, opacity: 1 });
  gsap.set(fl.querySelectorAll("[data-leaf], [data-petal], [data-center]"), { scale: 1, rotation: 0 });
}

function wilt(fl: Element, instant: boolean) {
  const stem = fl.querySelector<SVGPathElement>("[data-stem]");
  const parts = fl.querySelectorAll("[data-leaf], [data-petal], [data-center]");
  const len = stem?.getTotalLength() ?? 0;
  if (instant) {
    gsap.set(parts, { scale: 0 });
    gsap.set(stem, { strokeDashoffset: len, opacity: 0 });
    return;
  }
  const tl = gsap.timeline();
  tl.to(parts, { scale: 0, rotation: -20, duration: 0.35, ease: "power2.in", stagger: { each: 0.015, from: "end" } });
  tl.to(stem, { strokeDashoffset: len, duration: 0.45, ease: "power2.in" }, 0.2).set(stem, { opacity: 0 });
}
