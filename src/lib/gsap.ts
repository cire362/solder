"use client";

// Single place where GSAP plugins are registered. Import gsap from here, never directly,
// so ScrollTrigger is always registered before use.
// GSAP only drives dedicated leaf components (hero-tilt, solder-join, flora-scenes).
// It never animates elements that Motion also animates.
import { gsap } from "gsap";
import { ScrollTrigger } from "gsap/ScrollTrigger";
import { useGSAP } from "@gsap/react";

if (typeof window !== "undefined") {
  gsap.registerPlugin(ScrollTrigger, useGSAP);
}

export const MQ = {
  /** Full choreography: desktop, motion allowed. */
  full: "(min-width: 1024px) and (prefers-reduced-motion: no-preference)",
  /** Everything else gets the final static frame. */
  reduced: "(max-width: 1023px), (prefers-reduced-motion: reduce)",
  finePointer: "(pointer: fine) and (prefers-reduced-motion: no-preference)",
};

export { gsap, ScrollTrigger, useGSAP };
