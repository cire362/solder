"use client";

import { useState } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { AnimatePresence, motion } from "motion/react";
import { CircleHalf, List, X } from "@phosphor-icons/react";
import { Logo } from "./logo";
import { DownloadButton } from "./download-button";

const links = [
  { href: "/#built-in", label: "Features" },
  { href: "/#ai", label: "AI" },
  { href: "/#plugins", label: "Plugins" },
  { href: "/pricing", label: "Pricing" },
  { href: "/docs", label: "Docs" },
  { href: "/changelog", label: "Changelog" },
];

function toggleTheme() {
  const root = document.documentElement;
  const current =
    root.dataset.theme ??
    (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  const next = current === "dark" ? "light" : "dark";
  root.dataset.theme = next;
  try {
    localStorage.setItem("solder-theme", next);
  } catch {}
}

export function Nav() {
  const [open, setOpen] = useState(false);
  const pathname = usePathname();
  const isActive = (href: string) => !href.startsWith("/#") && pathname.startsWith(href);

  return (
    <header className="sticky top-0 z-40 border-b border-line bg-bg/80 backdrop-blur-xl">
      <nav className="mx-auto flex h-16 max-w-[1400px] items-center justify-between px-4 md:px-8">
        <Link href="/" aria-label="Solder home" className="shrink-0">
          <Logo />
        </Link>

        <ul className="hidden items-center gap-1 lg:flex">
          {links.map((l) => (
            <li key={l.href}>
              <Link
                href={l.href}
                aria-current={isActive(l.href) ? "page" : undefined}
                className={`rounded-lg px-3 py-2 text-sm transition-colors hover:text-fg ${
                  isActive(l.href) ? "text-fg" : "text-muted"
                }`}
              >
                {l.label}
              </Link>
            </li>
          ))}
        </ul>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={toggleTheme}
            aria-label="Toggle color theme"
            className="grid size-9 place-items-center rounded-lg text-muted transition-colors hover:bg-sunken hover:text-fg active:scale-95"
          >
            <CircleHalf className="size-[18px]" />
          </button>
          <div className="hidden sm:block">
            <DownloadButton size="sm" />
          </div>
          <button
            type="button"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
            aria-controls="mobile-menu"
            aria-label={open ? "Close menu" : "Open menu"}
            className="grid size-9 place-items-center rounded-lg text-fg hover:bg-sunken lg:hidden"
          >
            {open ? <X className="size-5" /> : <List className="size-5" />}
          </button>
        </div>
      </nav>

      <AnimatePresence>
        {open && (
          <motion.div
            id="mobile-menu"
            initial={{ clipPath: "inset(0 0 100% 0)", opacity: 0.6 }}
            animate={{ clipPath: "inset(0 0 0% 0)", opacity: 1 }}
            exit={{ clipPath: "inset(0 0 100% 0)", opacity: 0.6, transition: { duration: 0.28, ease: [0.4, 0, 1, 1] } }}
            transition={{ duration: 0.45, ease: [0.16, 1, 0.3, 1] }}
            className="absolute inset-x-0 top-16 z-50 border-b border-line bg-bg px-4 pb-6 pt-2 lg:hidden"
          >
            <ul className="flex flex-col">
              {links.map((l, i) => (
                <motion.li
                  key={l.href}
                  initial={{ opacity: 0, y: -6 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ delay: 0.04 + i * 0.03, duration: 0.3, ease: [0.16, 1, 0.3, 1] }}
                >
                  <Link href={l.href} onClick={() => setOpen(false)} className="block py-3 text-lg text-fg">
                    {l.label}
                  </Link>
                </motion.li>
              ))}
            </ul>
            <div className="mt-4 sm:hidden" onClick={() => setOpen(false)}>
              <DownloadButton />
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </header>
  );
}
