"use client";

import { useState } from "react";
import { Check, Copy } from "@phosphor-icons/react";

export function CopyCommand({ cmd, className = "" }: { cmd: string; className?: string }) {
  const [state, setState] = useState<"idle" | "copied" | "error">("idle");

  async function copy() {
    try {
      await navigator.clipboard.writeText(cmd);
      setState("copied");
      window.setTimeout(() => setState("idle"), 1800);
    } catch {
      setState("error");
    }
  }

  return (
    <div className={className}>
      <div className="flex items-center gap-2 rounded-lg border border-line-strong bg-elev py-1.5 pl-4 pr-1.5">
        <span className="select-none font-mono text-sm text-accent">$</span>
        <code className="min-w-0 flex-1 truncate font-mono text-[13px] text-fg">{cmd}</code>
        <button
          type="button"
          onClick={copy}
          aria-label={`Copy command: ${cmd}`}
          className="grid size-8 shrink-0 place-items-center rounded-lg text-muted transition hover:bg-sunken hover:text-fg active:scale-95"
        >
          {state === "copied" ? <Check weight="bold" className="size-4 text-accent" /> : <Copy className="size-4" />}
        </button>
      </div>
      {state === "error" && (
        <p role="status" className="mt-2 text-[13px] text-muted">
          Copy is blocked in this browser. Select the command and copy it manually.
        </p>
      )}
    </div>
  );
}
