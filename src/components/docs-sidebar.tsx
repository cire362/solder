"use client";

import { useId, useState } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { CaretDown } from "@phosphor-icons/react";
import { Collapse } from "./collapse";
import { FloraCluster } from "./flora-cluster";

type Group = { id: string; title: string; docs: { slug: string; title: string }[] };

function Links({ groups, onNavigate }: { groups: Group[]; onNavigate?: () => void }) {
  const pathname = usePathname();
  return (
    <nav aria-label="Documentation" className="space-y-8">
      <Link
        href="/docs"
        onClick={onNavigate}
        aria-current={pathname === "/docs" ? "page" : undefined}
        className={`block text-[14px] ${pathname === "/docs" ? "font-medium text-fg" : "text-muted hover:text-fg"}`}
      >
        Overview
      </Link>
      {groups.map((g) => (
        <div key={g.id}>
          <p className="text-[13px] font-medium text-subtle">{g.title}</p>
          <ul className="mt-3 space-y-0.5 border-l border-line">
            {g.docs.map((d) => {
              const href = `/docs/${d.slug}`;
              const active = pathname === href;
              return (
                <li key={d.slug}>
                  <Link
                    href={href}
                    onClick={onNavigate}
                    aria-current={active ? "page" : undefined}
                    className={`-ml-px block border-l py-1.5 pl-4 text-[14px] transition-colors ${
                      active ? "border-accent text-fg" : "border-transparent text-muted hover:border-line-strong hover:text-fg"
                    }`}
                  >
                    {d.title}
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}

export function DocsSidebar({ groups }: { groups: Group[] }) {
  const [open, setOpen] = useState(false);
  const panelId = useId();

  return (
    <>
      <aside className="sticky top-16 hidden h-[calc(100dvh-4rem)] flex-col overflow-y-auto border-r border-line pr-6 pt-10 lg:flex">
        <Links groups={groups} />
        <FloraCluster preset="sprig" delay={0.9} className="-ml-4 mt-auto w-[190px] shrink-0 pt-10" />
      </aside>

      <div className="border-b border-line lg:hidden">
        <button
          type="button"
          aria-expanded={open}
          aria-controls={panelId}
          onClick={() => setOpen((v) => !v)}
          className="flex w-full items-center justify-between py-3.5 text-[15px] font-medium text-fg"
        >
          Browse documentation
          <CaretDown
            className={`size-4 text-subtle transition-transform duration-[400ms] ease-[cubic-bezier(0.16,1,0.3,1)] ${
              open ? "rotate-180" : ""
            }`}
          />
        </button>
        <Collapse open={open} id={panelId}>
          <div className="pb-6 pt-3">
            <Links groups={groups} onNavigate={() => setOpen(false)} />
          </div>
        </Collapse>
      </div>
    </>
  );
}
