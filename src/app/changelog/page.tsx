import type { Metadata } from "next";
import { Rss } from "@phosphor-icons/react/ssr";
import { PageIntro } from "@/components/page-intro";
import { SubscribeForm } from "@/components/subscribe-form";
import { Reveal } from "@/components/reveal";
import { Inline } from "@/lib/blocks";
import { RELEASES } from "@/content/changelog";

export const metadata: Metadata = {
  title: "Changelog",
  description: "New features, improvements and fixes in every Solder release.",
};

const KINDS = [
  ["new", "New"],
  ["improved", "Improved"],
  ["fixed", "Fixed"],
] as const;

export default function ChangelogPage() {
  return (
    <>
      <PageIntro
        flora="lavender"
        title="Changelog"
        body="A new stable release every four weeks. Preview builds ship daily."
      >
        <div className="flex flex-col gap-6 sm:flex-row sm:items-start sm:gap-10">
          <SubscribeForm />
          <a href="#" className="inline-flex items-center gap-2 text-[15px] text-muted hover:text-fg sm:mt-9">
            <Rss className="size-4 text-accent" />
            RSS feed
          </a>
        </div>
      </PageIntro>

      <div className="mx-auto max-w-[1400px] px-4 pb-24 md:px-8 md:pb-32">
        {RELEASES.map((r) => (
          <Reveal key={r.version}>
            <article
              id={`v${r.version}`}
              className="grid gap-6 border-t border-line py-14 md:grid-cols-[220px_minmax(0,1fr)] md:gap-12 md:py-20"
            >
              <div className="md:sticky md:top-24 md:self-start">
                <p className="font-mono text-2xl font-medium text-fg">{r.version}</p>
                <time dateTime={r.iso} className="mt-1 block text-[14px] text-subtle">
                  {r.date}
                </time>
              </div>

              <div className="max-w-[68ch]">
                <h2 className="text-2xl font-semibold tracking-tight text-fg md:text-3xl">{r.title}</h2>
                <p className="mt-4 text-[17px] leading-relaxed text-muted">{r.summary}</p>

                <div className="mt-8 space-y-7">
                  {KINDS.map(([key, label]) => {
                    const items = r[key];
                    if (!items?.length) return null;
                    return (
                      <section key={key}>
                        <h3 className={`text-sm font-medium ${key === "new" ? "text-accent" : "text-fg"}`}>{label}</h3>
                        <ul className="mt-3 space-y-2 text-[15.5px] leading-relaxed text-muted [&_code]:rounded-md [&_code]:bg-sunken [&_code]:px-1.5 [&_code]:font-mono [&_code]:text-[0.88em] [&_code]:text-fg">
                          {items.map((item) => (
                            <li key={item} className="flex gap-3">
                              <span aria-hidden className="mt-[13px] h-px w-3 shrink-0 bg-line-strong" />
                              <span>
                                <Inline text={item} />
                              </span>
                            </li>
                          ))}
                        </ul>
                      </section>
                    );
                  })}
                </div>
              </div>
            </article>
          </Reveal>
        ))}
      </div>
    </>
  );
}
