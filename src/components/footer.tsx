import Link from "next/link";
import { DiscordLogo, GithubLogo, XLogo } from "@phosphor-icons/react/ssr";
import { Logo } from "./logo";

const COLUMNS = [
  {
    title: "Product",
    links: [
      ["Features", "/#built-in"],
      ["Solder AI", "/#ai"],
      ["Pricing", "/pricing"],
      ["Download", "/download"],
      ["Changelog", "/changelog"],
    ],
  },
  {
    title: "Developers",
    links: [
      ["Documentation", "/docs"],
      ["Getting started", "/docs/getting-started"],
      ["Plugin API", "/docs/plugin-api"],
      ["Security", "/security"],
    ],
  },
  {
    title: "Company",
    links: [
      ["About", "/about"],
      ["Blog", "/blog"],
      ["Careers", "/careers"],
      ["Contact", "/contact"],
    ],
  },
];

const SOCIAL = [
  { Icon: GithubLogo, label: "GitHub" },
  { Icon: XLogo, label: "X" },
  { Icon: DiscordLogo, label: "Discord" },
];

export function Footer() {
  return (
    <footer className="border-t border-line">
      <div className="mx-auto grid max-w-[1400px] gap-12 px-4 py-16 md:grid-cols-[minmax(0,2fr)_repeat(3,minmax(0,1fr))] md:px-8">
        <div>
          <Link href="/" aria-label="Solder home" className="inline-block">
            <Logo />
          </Link>
          <p className="mt-4 max-w-[32ch] text-sm leading-relaxed text-muted">
            The native IDE for people who ship the frontend, the backend and everything in between.
          </p>
          <div className="mt-6 flex gap-2">
            {SOCIAL.map(({ Icon, label }) => (
              <a
                key={label}
                href="#"
                aria-label={label}
                className="grid size-9 place-items-center rounded-lg border border-line text-muted transition-colors hover:border-line-strong hover:text-fg"
              >
                <Icon className="size-4" />
              </a>
            ))}
          </div>
        </div>
        {COLUMNS.map((c) => (
          <nav key={c.title} aria-label={c.title}>
            <h3 className="text-sm font-medium text-fg">{c.title}</h3>
            <ul className="mt-4 space-y-2.5">
              {c.links.map(([label, href]) => (
                <li key={href}>
                  <Link href={href} className="text-sm text-muted transition-colors hover:text-fg">
                    {label}
                  </Link>
                </li>
              ))}
            </ul>
          </nav>
        ))}
      </div>
      <div className="mx-auto flex max-w-[1400px] flex-col gap-2 border-t border-line px-4 py-6 text-[13px] text-subtle sm:flex-row sm:justify-between md:px-8">
        <span>© 2026 Solder Labs</span>
        <span className="flex gap-5">
          <Link href="/privacy" className="hover:text-fg">Privacy</Link>
          <Link href="/terms" className="hover:text-fg">Terms</Link>
          <Link href="/security" className="hover:text-fg">Security</Link>
        </span>
      </div>
    </footer>
  );
}
