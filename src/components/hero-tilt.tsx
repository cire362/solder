"use client";

import { useRef } from "react";
import { MQ, gsap, useGSAP } from "@/lib/gsap";

/*
  3D presentation for the hero editor. Three nested layers, one job each, so tweens never
  fight over the same transform:
    scroll layer: tilts back as the hero scrolls away, handing off to the 3D stack section
    intro layer:  rises out of depth on load
    tilt layer:   follows the pointer by a few degrees (fine pointers only)
  The children (the interactive editor) keep their own Motion animations on inner elements.
*/
export function HeroTilt({ children }: { children: React.ReactNode }) {
  const root = useRef<HTMLDivElement>(null);
  const scrollLayer = useRef<HTMLDivElement>(null);
  const introLayer = useRef<HTMLDivElement>(null);
  const tiltLayer = useRef<HTMLDivElement>(null);

  useGSAP(
    () => {
      const mm = gsap.matchMedia();

      mm.add("(prefers-reduced-motion: no-preference)", () => {
        gsap.fromTo(
          introLayer.current,
          { opacity: 0, rotationX: 24, rotationY: -16, z: -160, y: 48, transformPerspective: 1600 },
          { opacity: 1, rotationX: 0, rotationY: 0, z: 0, y: 0, duration: 1.6, delay: 0.2, ease: "expo.out", clearProps: "transform" },
        );
      });

      mm.add(MQ.full, () => {
        // Gentle hand-off as the hero leaves. The accelerating ease keeps the first
        // stretch of scrolling almost still, so small scrolls do not move the window.
        gsap.to(scrollLayer.current, {
          rotationX: 6,
          scale: 0.97,
          transformPerspective: 2000,
          transformOrigin: "50% 60%",
          ease: "power2.in",
          scrollTrigger: {
            trigger: root.current?.closest("section") ?? root.current,
            start: "top top",
            end: "bottom top",
            scrub: 1.2,
          },
        });
      });

      mm.add(`${MQ.finePointer} and (min-width: 1024px)`, () => {
        const section = root.current?.closest("section");
        const el = tiltLayer.current;
        if (!section || !el) return;
        gsap.set(el, { transformPerspective: 1600 });
        const rx = gsap.quickTo(el, "rotationX", { duration: 0.9, ease: "power3" });
        const ry = gsap.quickTo(el, "rotationY", { duration: 0.9, ease: "power3" });
        const clamp = gsap.utils.clamp(-1, 1);

        const onMove = (e: PointerEvent) => {
          const r = el.getBoundingClientRect();
          const px = clamp(((e.clientX - r.left) / r.width - 0.5) * 2);
          const py = clamp(((e.clientY - r.top) / r.height - 0.5) * 2);
          ry(px * 3);
          rx(-py * 2.5);
        };
        const onLeave = () => {
          rx(0);
          ry(0);
        };
        section.addEventListener("pointermove", onMove);
        section.addEventListener("pointerleave", onLeave);
        return () => {
          section.removeEventListener("pointermove", onMove);
          section.removeEventListener("pointerleave", onLeave);
        };
      });

      return () => mm.revert();
    },
    { scope: root },
  );

  return (
    <div ref={root}>
      <div ref={scrollLayer}>
        <div ref={introLayer} className="hero-intro">
          <div ref={tiltLayer}>{children}</div>
        </div>
      </div>
    </div>
  );
}
