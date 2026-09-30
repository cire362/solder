"use client";

import Link from "next/link";
import { osIcon, useOS } from "@/lib/os";

export function DownloadButton({ size = "lg" }: { size?: "sm" | "lg" }) {
  const os = useOS();
  const Icon = osIcon[os];
  const sizing = size === "lg" ? "h-12 px-5 text-[15px]" : "h-9 px-3.5 text-sm";
  return (
    <Link
      href="/download"
      className={`${sizing} inline-flex items-center gap-2 whitespace-nowrap rounded-lg bg-accent font-medium text-accent-fg transition-[transform,filter] duration-200 hover:brightness-110 active:translate-y-px active:scale-[0.98]`}
    >
      <Icon weight="fill" className={size === "lg" ? "size-[18px]" : "size-4"} />
      {size === "lg" ? `Download for ${os}` : "Download"}
    </Link>
  );
}
