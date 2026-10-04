"use client";

import { useState } from "react";
import Link from "next/link";
import { Check, Copy } from "@phosphor-icons/react";
import { DownloadButton } from "./download-button";
import { osIcon, useOS } from "@/lib/os";
import { SolderJoin } from "./solder-join";
import { CtaFlora } from "./flora-scenes";
import { INSTALL } from "@/content/install";
import { Reveal } from "./reveal";


export function DownloadSection() {
  const os = useOS();
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState(false);
  const cmd = INSTALL[os];
  const others = (Object.keys(INSTALL) as (keyof typeof INSTALL)[]).filter((o) => o !== os);

  async function copy() {
    try {
      await navigator.clipboard.writeText(cmd);
      setError(false);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1800);
    } catch {
      setError(true);
    }
  }

  return (
    <section id="download" className="relative overflow-hidden border-t border-line">
      <div
        aria-hidden
        className="pointer-events-none absolute inset-x-0 top-0 h-[480px] bg-[radial-gradient(ellipse_50%_60%_at_50%_0%,var(--accent-soft),transparent)]"
      />
      <CtaFlora />
      <div className="relative mx-auto max-w-[1400px] px-4 py-28 text-center md:px-8 md:py-40">
        <SolderJoin />
        <Reveal>
          <h2 className="mx-auto mt-8 max-w-[16ch] text-5xl font-semibold leading-[1] tracking-tighter text-fg md:text-7xl">
            Ship the whole stack from one window.
          </h2>
          <p className="mx-auto mt-6 max-w-[48ch] text-lg leading-relaxed text-muted">
            Free for personal projects. Your settings from VS Code or JetBrains come with you.
          </p>
        </Reveal>

        <Reveal delay={0.1}>
          <div className="mt-10 flex flex-col items-center gap-5">
            <DownloadButton />

            <div className="flex w-full max-w-md items-center gap-2 rounded-lg border border-line-strong bg-elev py-1.5 pl-4 pr-1.5 text-left">
              <span className="select-none font-mono text-sm text-accent">$</span>
              <code className="min-w-0 flex-1 truncate font-mono text-[13px] text-fg">{cmd}</code>
              <button
                type="button"
                onClick={copy}
                aria-label="Copy install command"
                className="grid size-8 shrink-0 place-items-center rounded-lg text-muted transition hover:bg-sunken hover:text-fg active:scale-95"
              >
                {copied ? <Check weight="bold" className="size-4 text-accent" /> : <Copy className="size-4" />}
              </button>
            </div>
            <p role="status" className="h-5 text-[13px] text-muted">
              {error ? "Copy is blocked in this browser. Select the command and copy it manually." : copied ? "Copied to clipboard" : ""}
            </p>

            <p className="flex flex-wrap items-center justify-center gap-x-5 gap-y-2 text-sm text-muted">
              Also available for
              {others.map((o) => {
                const Icon = osIcon[o];
                return (
                  <Link key={o} href="/download" className="inline-flex items-center gap-1.5 text-fg underline-offset-4 hover:underline">
                    <Icon weight="fill" className="size-4" />
                    {o}
                  </Link>
                );
              })}
            </p>
          </div>
        </Reveal>
      </div>
    </section>
  );
}
