"use client";

import { useRef } from "react";
import { gsap, useGSAP } from "@/lib/gsap";

/*
  The logo, assembled in 3D as the final CTA scrolls in: both halves swing in from depth,
  meet, and the solder dot lands at the joint with a short flash ring.
  Geometry is the LogoMark (24 unit grid) at 3x. The final frame is the static markup.
*/
export function SolderJoin() {
  const root = useRef<HTMLDivElement>(null);

  useGSAP(
    () => {
      const mm = gsap.matchMedia();
      mm.add("(prefers-reduced-motion: no-preference)", () => {
        const q = gsap.utils.selector(root);
        const tl = gsap.timeline({
          defaults: { ease: "none" },
          scrollTrigger: { trigger: root.current, start: "top 95%", end: "top 40%", scrub: 0.6 },
        });
        // GSAP writes startAt into the vars object, so each tween gets fresh objects.
        const swingIn = (side: -1 | 1) =>
          [
            { x: side * 60, z: -90, rotationY: side * -75, opacity: 0 },
            { x: 0, z: 0, rotationY: 0, opacity: 1, duration: 0.7, ease: "power1.inOut" },
          ] as const;
        tl.fromTo(q("[data-l]"), ...swingIn(-1), 0)
          .fromTo(q("[data-r]"), ...swingIn(1), 0)
          .fromTo(q("[data-dot]"), { scale: 0 }, { scale: 1, duration: 0.18, ease: "back.out(2.5)" }, 0.72)
          // Flash ring: stays hidden (CSS opacity-0) until the joint lands.
          .set(q("[data-ring]"), { scale: 0.6, opacity: 0.9 }, 0.74)
          .to(q("[data-ring]"), { scale: 2.6, opacity: 0, duration: 0.26, ease: "power2.out" }, 0.74);
      });
      return () => mm.revert();
    },
    { scope: root },
  );

  const half = "absolute top-[18px] h-[36px] w-[24px] rounded-[7.5px] bg-fg";
  const dot = "absolute left-[26.25px] top-[26.25px] size-[19.5px] rounded-full";

  return (
    <div ref={root} aria-hidden className="relative mx-auto size-[72px]" style={{ perspective: 600 }}>
      <span data-l className={`${half} left-[4.5px]`} />
      <span data-r className={`${half} left-[43.5px]`} />
      <span data-ring className={`${dot} border-2 border-accent opacity-0`} />
      <span data-dot className={`${dot} bg-accent`} />
    </div>
  );
}
