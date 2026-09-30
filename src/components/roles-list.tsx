"use client";

import { useState } from "react";
import Link from "next/link";
import { AnimatePresence, motion } from "motion/react";
import { ArrowRight } from "@phosphor-icons/react";
import { ROLES, TEAMS } from "@/content/careers";

export function RolesList() {
  const [team, setTeam] = useState<(typeof TEAMS)[number]>("All");
  const shown = ROLES.filter((r) => team === "All" || r.team === team);

  return (
    <div>
      <div className="flex gap-2 overflow-x-auto pb-1" role="group" aria-label="Filter roles by team">
        {TEAMS.map((t) => {
          const count = t === "All" ? ROLES.length : ROLES.filter((r) => r.team === t).length;
          return (
            <button
              key={t}
              type="button"
              aria-pressed={team === t}
              onClick={() => setTeam(t)}
              className={`inline-flex h-9 shrink-0 items-center gap-2 rounded-lg px-3.5 text-[14px] transition-colors ${
                team === t ? "bg-fg text-bg" : "bg-sunken text-muted hover:text-fg"
              }`}
            >
              {t}
              <span className={`font-mono text-[12px] ${team === t ? "text-bg/70" : "text-subtle"}`}>{count}</span>
            </button>
          );
        })}
      </div>

      {shown.length === 0 ? (
        <div className="mt-8 rounded-2xl border border-dashed border-line-strong px-6 py-16 text-center">
          <p className="font-medium text-fg">No open roles in {team} right now</p>
          <p className="mx-auto mt-1.5 max-w-[44ch] text-[15px] text-muted">
            We still read every open application. Tell us what you would work on.
          </p>
          <Link
            href="/contact?topic=careers"
            className="mt-6 inline-flex h-10 items-center rounded-lg border border-line-strong px-4 text-[14px] text-fg hover:bg-elev"
          >
            Send an open application
          </Link>
        </div>
      ) : (
        <ul className="mt-8 space-y-3">
          <AnimatePresence initial={false} mode="popLayout">
            {shown.map((r) => (
              <motion.li
                key={r.slug}
                layout
                initial={{ opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.25 }}
              >
                <Link
                  href={`/contact?topic=careers&role=${r.slug}`}
                  className="group grid gap-3 rounded-2xl border border-line p-5 transition-colors hover:border-line-strong hover:bg-elev md:grid-cols-[minmax(0,1fr)_200px_auto] md:items-center md:gap-8 md:p-6"
                >
                  <span>
                    <span className="block text-[17px] font-medium text-fg">{r.title}</span>
                    <span className="mt-1 block text-[15px] text-muted">{r.summary}</span>
                  </span>
                  <span className="text-[14px] text-subtle">
                    <span className="block text-fg">{r.team}</span>
                    {r.location}
                  </span>
                  <span className="inline-flex items-center gap-1.5 text-[14px] font-medium text-fg group-hover:text-accent">
                    Apply
                    <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
                  </span>
                </Link>
              </motion.li>
            ))}
          </AnimatePresence>
        </ul>
      )}
    </div>
  );
}
