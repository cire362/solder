import type { Metadata } from "next";
import Link from "next/link";
import { ArrowRight, Hammer, PuzzlePiece, RocketLaunch, Sparkle } from "@phosphor-icons/react/ssr";
import { DocsSearch } from "@/components/docs-search";
import { CopyCommand } from "@/components/copy-command";
import { Reveal } from "@/components/reveal";
import { ALL_DOCS, DOC_GROUPS } from "@/content/docs";

export const metadata: Metadata = {
  title: "Documentation",
  description: "Guides and reference for the Solder IDE, Solder AI and the plugin API.",
};

const ICONS = { start: RocketLaunch, build: Hammer, ai: Sparkle, extend: PuzzlePiece } as const;

// Cell treatments give the grid rhythm: one tinted, one patterned, two plain.
const TREATMENT: Record<string, string> = {
  start: "bg-[linear-gradient(135deg,var(--accent-soft),transparent_60%)] bg-elev",
  build: "bg-elev",
  ai: "bg-sunken",
  extend: "dot-grid bg-bg",
};

export default function DocsHome() {
  const entries = ALL_DOCS.map(({ slug, title, description, keywords, group }) => ({
    slug,
    title,
    description,
    keywords,
    group,
  }));

  return (
    <div className="max-w-4xl">
      <Reveal y={16}>
        <h1 className="text-[40px] font-semibold leading-[1.02] tracking-tighter text-fg md:text-5xl">Documentation</h1>
        <p className="mt-4 max-w-[56ch] text-lg leading-relaxed text-muted">
          Everything you need to set up Solder, run your stack and extend the editor.
        </p>
      </Reveal>

      <Reveal y={16} delay={0.06} className="mt-10">
        <DocsSearch entries={entries} />
      </Reveal>

      <Reveal y={16} delay={0.1} className="mt-14">
        <h2 className="text-xl font-semibold tracking-tight text-fg">Up and running in a minute</h2>
        <p className="mt-2 text-[15px] text-muted">Install with Homebrew, then open any project folder.</p>
        <div className="mt-4 grid gap-3 sm:grid-cols-2">
          <CopyCommand cmd="brew install --cask solder" />
          <CopyCommand cmd="solder ~/code/my-app" />
        </div>
      </Reveal>

      <div className="mt-16 grid gap-4 md:grid-cols-2">
        {DOC_GROUPS.map((g, i) => {
          const Icon = ICONS[g.id as keyof typeof ICONS];
          return (
            <Reveal key={g.id} delay={i * 0.05}>
              <section className={`flex h-full flex-col rounded-2xl border border-line p-6 md:p-7 ${TREATMENT[g.id]}`}>
                <Icon className="size-5 text-accent" />
                <h2 className="mt-4 text-lg font-medium tracking-tight text-fg">{g.title}</h2>
                <p className="mt-1.5 text-[15px] leading-relaxed text-muted">{g.summary}</p>
                <ul className="mt-6 space-y-1">
                  {g.docs.map((d) => (
                    <li key={d.slug}>
                      <Link
                        href={`/docs/${d.slug}`}
                        className="group -mx-2 flex items-center justify-between gap-3 rounded-lg px-2 py-2 text-[15px] text-fg hover:bg-bg/60"
                      >
                        {d.title}
                        <ArrowRight className="size-4 text-subtle transition-transform group-hover:translate-x-0.5 group-hover:text-accent" />
                      </Link>
                    </li>
                  ))}
                </ul>
              </section>
            </Reveal>
          );
        })}
      </div>
    </div>
  );
}
