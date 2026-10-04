"use client";

import { usePathname } from "next/navigation";
import { Terminal } from "@phosphor-icons/react";

// Shows the requested path as a dev-server log line, the way a 404 looks from inside Solder.
export function NotFoundPath() {
  const pathname = usePathname();
  return (
    <div className="overflow-hidden rounded-2xl border border-line-strong bg-elev font-mono text-[13px] shadow-window">
      <p className="flex items-center gap-2 border-b border-line px-4 py-2.5 text-[12px] text-subtle">
        <Terminal className="size-3.5" />
        Terminal
      </p>
      <div className="space-y-1.5 px-4 py-5 leading-relaxed">
        <p className="flex gap-3">
          <span className="w-8 shrink-0 text-accent">web</span>
          <span className="break-all text-fg">GET {pathname} 404</span>
        </p>
        <p className="flex gap-3">
          <span className="w-8 shrink-0 text-subtle">web</span>
          <span className="text-muted">no route matched this path</span>
        </p>
        <p className="flex gap-3">
          <span className="w-8 shrink-0 text-subtle">$</span>
          <span className="caret inline-block h-[1.1em] w-[7px] translate-y-[3px] bg-accent" />
        </p>
      </div>
    </div>
  );
}
