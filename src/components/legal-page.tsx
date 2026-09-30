import Link from "next/link";
import { Blocks, headings } from "@/lib/blocks";
import type { LegalDoc } from "@/content/legal";
import { Toc } from "./toc";
import { Reveal } from "./reveal";
import { FloraCluster } from "./flora-cluster";

const LEGAL_LINKS = [
  ["Privacy", "/privacy"],
  ["Terms", "/terms"],
  ["Security", "/security"],
];

export function LegalPage({ doc, path }: { doc: LegalDoc; path: string }) {
  return (
    <div className="mx-auto max-w-[1400px] px-4 pb-24 pt-16 md:px-8 md:pb-32 md:pt-24">
      <div className="grid gap-12 lg:grid-cols-[220px_minmax(0,1fr)] lg:gap-16">
        <aside className="lg:sticky lg:top-28 lg:self-start">
          <nav aria-label="Legal" className="flex gap-2 lg:flex-col lg:gap-0.5">
            {LEGAL_LINKS.map(([label, href]) => (
              <Link
                key={href}
                href={href}
                aria-current={href === path ? "page" : undefined}
                className={`rounded-lg px-3 py-2 text-[14px] transition-colors ${
                  href === path ? "bg-sunken font-medium text-fg" : "text-muted hover:text-fg"
                }`}
              >
                {label}
              </Link>
            ))}
          </nav>
          <div className="mt-10 hidden lg:block">
            <Toc items={headings(doc.body)} title="Sections" />
            <FloraCluster preset="lavender" delay={0.8} className="-ml-6 mt-12 w-[200px]" />
          </div>
        </aside>

        <Reveal y={16} className="max-w-[70ch]">
          <h1 className="text-4xl font-semibold leading-[1.05] tracking-tighter text-fg md:text-5xl">{doc.title}</h1>
          <p className="mt-3 text-[14px] text-subtle">
            Last updated <time dateTime={doc.iso}>{doc.updated}</time>
          </p>
          <p className="mt-6 text-lg leading-relaxed text-muted">{doc.intro}</p>
          <div className="mt-12 border-t border-line pt-12">
            <Blocks blocks={doc.body} />
          </div>
        </Reveal>
      </div>
    </div>
  );
}
