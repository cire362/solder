"use client";

import { useEffect, useId, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { ArrowRight, MagnifyingGlass } from "@phosphor-icons/react";

type Entry = { slug: string; title: string; description: string; keywords: string; group: string };

export function DocsSearch({ entries }: { entries: Entry[] }) {
  const id = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const [q, setQ] = useState("");

  // Cmd/Ctrl+K focuses search, matching the shortcut inside the app.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        inputRef.current?.focus();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const results = useMemo(() => {
    const terms = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return [];
    return entries.filter((e) => {
      const hay = `${e.title} ${e.description} ${e.keywords}`.toLowerCase();
      return terms.every((t) => hay.includes(t));
    });
  }, [q, entries]);

  return (
    <div>
      <label htmlFor={id} className="mb-2 block text-sm font-medium text-fg">
        Search the docs
      </label>
      <div className="relative">
        <MagnifyingGlass className="pointer-events-none absolute left-4 top-1/2 size-5 -translate-y-1/2 text-subtle" />
        <input
          ref={inputRef}
          id={id}
          type="search"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="postgres, breakpoints, local models…"
          className="h-14 w-full rounded-lg border border-line-strong bg-elev pl-12 pr-16 text-[16px] text-fg shadow-window outline-none placeholder:text-subtle focus:border-accent focus:ring-2 focus:ring-accent/25"
        />
        <kbd className="pointer-events-none absolute right-4 top-1/2 hidden -translate-y-1/2 rounded-md border border-line-strong px-1.5 py-0.5 font-mono text-[11px] text-subtle sm:block">
          ⌘K
        </kbd>
      </div>

      {q.trim() && (
        <div className="mt-3 rounded-2xl border border-line bg-elev p-2" aria-live="polite">
          {results.length ? (
            <ul>
              {results.map((r) => (
                <li key={r.slug}>
                  <Link
                    href={`/docs/${r.slug}`}
                    className="group flex items-center gap-4 rounded-lg px-3 py-3 hover:bg-sunken"
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block text-[15px] font-medium text-fg">{r.title}</span>
                      <span className="block truncate text-[14px] text-muted">{r.description}</span>
                    </span>
                    <span className="hidden shrink-0 text-[13px] text-subtle sm:block">{r.group}</span>
                    <ArrowRight className="size-4 shrink-0 text-subtle transition-transform group-hover:translate-x-0.5 group-hover:text-accent" />
                  </Link>
                </li>
              ))}
            </ul>
          ) : (
            <div className="px-3 py-6 text-center">
              <p className="font-medium text-fg">No pages match “{q.trim()}”</p>
              <p className="mt-1 text-[14px] text-muted">
                Try a broader term, or ask in{" "}
                <a href="#" className="text-fg underline decoration-accent underline-offset-4">
                  the community Discord
                </a>
                .
              </p>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
