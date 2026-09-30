"use client";

import { useState } from "react";
import Link from "next/link";
import { AnimatePresence, motion } from "motion/react";
import { ArrowRight, DownloadSimple } from "@phosphor-icons/react";
import { osIcon, useOS } from "@/lib/os";
import { CopyCommand } from "./copy-command";
import { INSTALL, INSTALL_PREVIEW, type OS } from "@/content/install";

// mock: placeholder release data and file sizes. Wire to the real release feed before launch.
const RELEASE = {
  stable: { version: "1.8.2", date: "September 16, 2026" },
  preview: { version: "1.9.0-preview.3", date: "September 27, 2026" },
};

const BUILDS: Record<OS, { label: string; file: string; size: string; primary?: boolean }[]> = {
  macOS: [
    { label: "Apple silicon", file: "Solder-arm64.dmg", size: "118 MB", primary: true },
    { label: "Intel", file: "Solder-x64.dmg", size: "124 MB" },
  ],
  Windows: [
    { label: "x64 installer", file: "SolderSetup-x64.exe", size: "109 MB", primary: true },
    { label: "ARM64 installer", file: "SolderSetup-arm64.exe", size: "104 MB" },
    { label: "Portable zip", file: "Solder-win-x64.zip", size: "121 MB" },
  ],
  Linux: [
    { label: "Debian, Ubuntu", file: "solder_amd64.deb", size: "96 MB", primary: true },
    { label: "Fedora, RHEL", file: "solder.x86_64.rpm", size: "97 MB" },
    { label: "AppImage", file: "Solder-x86_64.AppImage", size: "112 MB" },
    { label: "Tarball, ARM64", file: "solder-linux-arm64.tar.gz", size: "93 MB" },
  ],
};

const REQUIREMENTS: Record<OS, string[]> = {
  macOS: ["macOS 13 Ventura or later", "Apple silicon or Intel", "8 GB of memory, 16 GB for large monorepos"],
  Windows: ["Windows 10 22H2 or Windows 11", "x64 or ARM64 with DirectX 12", "8 GB of memory, 16 GB for large monorepos"],
  Linux: ["glibc 2.31 or later", "Vulkan 1.2 capable GPU driver", "X11 or Wayland session"],
};

const ORDER: OS[] = ["macOS", "Windows", "Linux"];

export function DownloadPicker() {
  const detected = useOS();
  const [picked, setPicked] = useState<OS | null>(null);
  const [channel, setChannel] = useState<"stable" | "preview">("stable");
  const os = picked ?? detected;
  const release = RELEASE[channel];

  return (
    <section className="mx-auto max-w-[1400px] px-4 pb-24 md:px-8 md:pb-32">
      <div className="flex flex-col gap-4 border-b border-line sm:flex-row sm:items-end sm:justify-between">
        <div role="tablist" aria-label="Operating system" className="-mb-px flex gap-1">
          {ORDER.map((o) => {
            const Icon = osIcon[o];
            const active = o === os;
            return (
              <button
                key={o}
                role="tab"
                type="button"
                aria-selected={active}
                onClick={() => setPicked(o)}
                className={`relative flex h-12 items-center gap-2 px-4 text-[15px] transition-colors ${
                  active ? "text-fg" : "text-muted hover:text-fg"
                }`}
              >
                <Icon weight={active ? "fill" : "regular"} className="size-[18px]" />
                {o}
                {o === detected && <span className="text-[12px] text-subtle">(yours)</span>}
                {active && <motion.span layoutId="os-tab" className="absolute inset-x-0 bottom-0 h-[2px] bg-accent" />}
              </button>
            );
          })}
        </div>

        <label className="mb-3 flex items-center gap-3 text-sm text-muted">
          <span>Channel</span>
          <select
            value={channel}
            onChange={(e) => setChannel(e.target.value as "stable" | "preview")}
            className="h-9 rounded-lg border border-line-strong bg-elev px-3 text-sm text-fg outline-none focus:border-accent"
          >
            <option value="stable">Stable</option>
            <option value="preview">Preview</option>
          </select>
        </label>
      </div>

      <AnimatePresence mode="wait">
        <motion.div
          key={`${os}-${channel}`}
          role="tabpanel"
          initial={{ opacity: 0, y: 10 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: -6, transition: { duration: 0.12 } }}
          transition={{ duration: 0.35, ease: [0.16, 1, 0.3, 1] }}
          className="grid gap-12 pt-10 lg:grid-cols-[minmax(0,7fr)_minmax(0,5fr)] lg:gap-16"
        >
          <div>
            <p className="text-sm text-muted">
              {channel === "stable" ? "Stable" : "Preview"} {release.version}, released {release.date}
            </p>
            {channel === "preview" && (
              <p className="mt-2 max-w-[56ch] text-[15px] leading-relaxed text-muted">
                Preview builds get new features about three weeks early. They update daily and can break, so keep a
                stable install next to them.
              </p>
            )}

            <ul className="mt-6 space-y-3">
              {BUILDS[os].map((b) => (
                <li key={b.file}>
                  <a
                    href="#"
                    className={`group flex items-center gap-4 rounded-2xl border p-4 transition-colors md:p-5 ${
                      b.primary ? "border-line-strong bg-elev shadow-window" : "border-line hover:border-line-strong"
                    }`}
                  >
                    <span
                      className={`grid size-11 shrink-0 place-items-center rounded-lg ${
                        b.primary ? "bg-accent text-accent-fg" : "bg-sunken text-fg"
                      }`}
                    >
                      <DownloadSimple weight="bold" className="size-5" />
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="block text-[16px] font-medium text-fg">{b.label}</span>
                      <span className="block truncate font-mono text-[12.5px] text-subtle">{b.file}</span>
                    </span>
                    <span className="shrink-0 font-mono text-[13px] text-subtle">{b.size}</span>
                  </a>
                </li>
              ))}
            </ul>

            <Link
              href="/changelog"
              className="group mt-6 inline-flex items-center gap-2 text-[15px] text-fg underline decoration-accent underline-offset-4"
            >
              What changed in {release.version.split("-")[0]}
              <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
            </Link>
          </div>

          <div className="space-y-10">
            <div>
              <h2 className="text-lg font-medium text-fg">Install from the command line</h2>
              <p className="mt-1.5 text-[15px] text-muted">Updates arrive through the same package manager.</p>
              <CopyCommand cmd={(channel === "preview" ? INSTALL_PREVIEW : INSTALL)[os]} className="mt-4" />
            </div>
            <div>
              <h2 className="text-lg font-medium text-fg">System requirements</h2>
              <ul className="mt-3 space-y-2 text-[15px] text-muted">
                {REQUIREMENTS[os].map((r) => (
                  <li key={r} className="flex gap-3">
                    <span aria-hidden className="mt-[11px] h-px w-3 shrink-0 bg-line-strong" />
                    {r}
                  </li>
                ))}
              </ul>
            </div>
          </div>
        </motion.div>
      </AnimatePresence>
    </section>
  );
}
