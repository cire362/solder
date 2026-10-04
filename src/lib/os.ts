import { useSyncExternalStore } from "react";
import { AppleLogo, LinuxLogo, WindowsLogo } from "@phosphor-icons/react";
import type { OS } from "@/content/install";

function detectOS(): OS {
  const ua = navigator.userAgent;
  if (/Windows/i.test(ua)) return "Windows";
  if (/Linux|X11/i.test(ua) && !/Android/i.test(ua)) return "Linux";
  return "macOS";
}

const noop = () => () => {};

/** Visitor's OS. Server render and first hydration pass use "macOS", then the real value. */
export function useOS(): OS {
  return useSyncExternalStore(noop, detectOS, () => "macOS");
}

export const osIcon = { macOS: AppleLogo, Windows: WindowsLogo, Linux: LinuxLogo } as const;
