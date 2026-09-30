"use client";

import { motion } from "motion/react";
import { Cpu, Lightning, Stack } from "@phosphor-icons/react";
import { Reveal } from "./reveal";

const ease = [0.16, 1, 0.3, 1] as const;

// mock: placeholder benchmark figures. Replace with published benchmark data before launch.
const METRICS = [
  {
    label: "Cold start",
    detail: "Launch to an editable file",
    solder: 0.4,
    other: 2.9,
    unit: "s",
  },
  {
    label: "Memory",
    detail: "500k-line monorepo, 40 files open",
    solder: 310,
    other: 1640,
    unit: "MB",
  },
  {
    label: "Input latency",
    detail: "Keystroke to pixel on screen",
    solder: 6,
    other: 24,
    unit: "ms",
  },
];

const FACTS = [
  {
    Icon: Cpu,
    title: "Rust core",
    body: "No Electron and no webview. Editing, search and indexing run as native code across every core.",
  },
  {
    Icon: Lightning,
    title: "GPU rendering",
    body: "The whole UI is drawn on the GPU at your display's refresh rate, up to 120 Hz.",
  },
  {
    Icon: Stack,
    title: "Incremental indexing",
    body: "Go-to-definition works file by file while the rest of the project indexes in the background.",
  },
];

function Bar({ value, max, accent, delay }: { value: number; max: number; accent?: boolean; delay: number }) {
  return (
    <motion.span
      className={`block h-2 rounded-full ${accent ? "bg-accent" : "bg-line-strong"}`}
      style={{ width: `${Math.max(4, (value / max) * 100)}%`, transformOrigin: "left" }}
      initial={{ scaleX: 0 }}
      whileInView={{ scaleX: 1 }}
      viewport={{ once: true, amount: 0.6 }}
      transition={{ duration: 1.1, delay, ease }}
    />
  );
}

export function Performance() {
  return (
    <section id="performance" className="border-y border-line bg-sunken">
      <div className="mx-auto max-w-[1400px] px-4 py-24 md:px-8 md:py-32">
        <Reveal className="max-w-3xl">
          <h2 className="text-4xl font-semibold leading-[1.05] tracking-tighter text-fg md:text-6xl">
            Native speed, even on a monorepo.
          </h2>
          <p className="mt-5 max-w-[58ch] text-lg leading-relaxed text-muted">
            Solder opens before your dev server boots and stays responsive while ten services stream logs.
          </p>
        </Reveal>

        <div className="mt-16 space-y-10 md:space-y-12">
          {METRICS.map((m, i) => (
            <Reveal key={m.label} delay={i * 0.05}>
              <div className="grid gap-5 md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] md:gap-12">
                <div>
                  <h3 className="text-lg font-medium text-fg">{m.label}</h3>
                  <p className="mt-1 text-[15px] text-muted">{m.detail}</p>
                </div>
                <div className="space-y-3">
                  <div className="grid grid-cols-[88px_minmax(0,1fr)_72px] items-center gap-4 sm:grid-cols-[140px_minmax(0,1fr)_80px]">
                    <span className="text-sm font-medium text-fg">Solder</span>
                    <Bar value={m.solder} max={m.other} accent delay={0.1} />
                    <span className="text-right font-mono text-sm text-fg">
                      {m.solder} {m.unit}
                    </span>
                  </div>
                  <div className="grid grid-cols-[88px_minmax(0,1fr)_72px] items-center gap-4 sm:grid-cols-[140px_minmax(0,1fr)_80px]">
                    <span className="text-sm text-subtle">
                      <span className="sm:hidden">Electron</span>
                      <span className="hidden sm:inline">Electron-based IDE</span>
                    </span>
                    <Bar value={m.other} max={m.other} delay={0.2} />
                    <span className="text-right font-mono text-sm text-subtle">
                      {m.other.toLocaleString("en-US")} {m.unit}
                    </span>
                  </div>
                </div>
              </div>
            </Reveal>
          ))}
        </div>

        <p className="mt-10 text-[13px] text-subtle">
          Preliminary internal benchmarks on an Apple M3 Pro with 18 GB of memory. Methodology is published in the docs.
        </p>

        <div className="mt-20 grid gap-10 border-t border-line pt-12 md:grid-cols-3 md:gap-0">
          {FACTS.map((f, i) => (
            <Reveal
              key={f.title}
              delay={i * 0.08}
              className={`md:px-8 ${i === 0 ? "md:pl-0" : "md:border-l md:border-line"}`}
            >
              <f.Icon className="mb-4 size-6 text-accent" />
              <h3 className="text-lg font-medium text-fg">{f.title}</h3>
              <p className="mt-2 max-w-[38ch] text-[15px] leading-relaxed text-muted">{f.body}</p>
            </Reveal>
          ))}
        </div>
      </div>
    </section>
  );
}
