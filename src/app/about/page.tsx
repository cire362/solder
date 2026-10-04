import type { Metadata } from "next";
import Image from "next/image";
import Link from "next/link";
import { ArrowRight, Keyboard, Lightning, LockSimple, Stack } from "@phosphor-icons/react/ssr";
import { PageIntro } from "@/components/page-intro";
import { Reveal } from "@/components/reveal";

export const metadata: Metadata = {
  title: "About",
  description: "Solder Labs builds a native IDE for developers who ship the whole product.",
};

const PRINCIPLES = [
  {
    Icon: Lightning,
    title: "Fast is a feature",
    body: "If a change makes typing, opening or searching slower, it does not ship. We measure latency on every build.",
  },
  {
    Icon: LockSimple,
    title: "Your code stays yours",
    body: "Local by default. Hosted AI only sees what a request needs, keeps nothing and trains on nothing.",
  },
  {
    Icon: Stack,
    title: "Built in, not bolted on",
    body: "The tools a fullstack day needs share one project model, so the debugger, database and AI agree with each other.",
  },
  {
    Icon: Keyboard,
    title: "Respect the keyboard",
    body: "Every action has a shortcut and every panel can be driven without a mouse. Your muscle memory is a feature too.",
  },
];

// mock: placeholder company milestones
const MILESTONES = [
  { when: "Spring 2024", what: "Three engineers start Solder after one too many bugs that crossed four tools." },
  { when: "Summer 2025", what: "Public preview. Rust core, database client and the first version of Solder AI." },
  { when: "March 2026", what: "Solder 1.0 on macOS, Windows and Linux." },
  { when: "May 2026", what: "Plugin API 1.0 and the plugin registry." },
  { when: "September 2026", what: "Agent tasks in sandboxed branches." },
];

export default function AboutPage() {
  return (
    <>
      <PageIntro
        center
        flora="meadow"
        floraAlt="wild"
        title="We build the editor we wanted for shipping whole products."
        body="Solder Labs is a small, remote team of engineers and designers. We use Solder to build Solder, every day."
      />

      <Reveal className="mx-auto max-w-[1400px] px-4 md:px-8">
        <div className="relative aspect-[16/9] overflow-hidden rounded-2xl border border-line bg-sunken md:aspect-[21/9]">
          {/* TODO: replace with a real team or workspace photo, 2400x1030 */}
          <Image
            src="https://picsum.photos/seed/solder-about-workspace/2400/1030?grayscale"
            alt="A desk with a laptop and notebooks in a quiet workspace"
            fill
            priority
            sizes="(min-width: 1400px) 1400px, 100vw"
            className="object-cover opacity-90"
          />
        </div>
      </Reveal>

      <section className="mx-auto grid max-w-[1400px] gap-10 px-4 py-24 md:px-8 md:py-32 lg:grid-cols-[minmax(0,4fr)_minmax(0,7fr)] lg:gap-16">
        <Reveal>
          <h2 className="text-3xl font-semibold tracking-tighter text-fg md:text-4xl lg:sticky lg:top-28">Why Solder exists</h2>
        </Reveal>
        <Reveal delay={0.06} className="prose-solder max-w-[64ch] text-[17px]">
          <p>
            A normal bug in a web product touches a component, an API handler, a query and a migration. Fixing it meant
            an editor, a database GUI, an HTTP client, two debuggers and a terminal with six tabs. Each tool was good.
            None of them knew about the others.
          </p>
          <p>
            We started Solder to put those tools in one native app that understands how a project fits together. The
            debugger knows which handler serves a request. The database client knows your migrations. The AI sees all
            of it, so its answers line up across the stack.
          </p>
          <p>
            We are funded by the people who use Solder. The editor is free for personal work, and teams pay for hosted
            AI, preview environments and admin controls. That keeps our incentives simple: make the editor better.
          </p>
        </Reveal>
      </section>

      <section className="border-y border-line bg-sunken">
        <div className="mx-auto max-w-[1400px] px-4 py-24 md:px-8 md:py-28">
          <Reveal>
            <h2 className="text-3xl font-semibold tracking-tighter text-fg md:text-4xl">What we hold ourselves to</h2>
          </Reveal>
          <div className="mt-14 grid gap-px overflow-hidden rounded-2xl border border-line bg-line md:grid-cols-2">
            {PRINCIPLES.map((p, i) => (
              <Reveal key={p.title} delay={i * 0.05} className="h-full">
                <div className="h-full bg-bg p-8 md:p-10">
                  <p.Icon className="size-6 text-accent" />
                  <h3 className="mt-6 text-xl font-medium tracking-tight text-fg">{p.title}</h3>
                  <p className="mt-2 max-w-[44ch] text-[15.5px] leading-relaxed text-muted">{p.body}</p>
                </div>
              </Reveal>
            ))}
          </div>
        </div>
      </section>

      <section className="mx-auto max-w-[1400px] px-4 py-24 md:px-8 md:py-32">
        <Reveal>
          <h2 className="text-3xl font-semibold tracking-tighter text-fg md:text-4xl">So far</h2>
        </Reveal>
        <ol className="relative mt-14 grid gap-10 md:grid-cols-5 md:gap-6">
          <span aria-hidden className="absolute left-[7px] top-2 h-[calc(100%-1rem)] w-px bg-line-strong md:left-0 md:top-[7px] md:h-px md:w-full" />
          {MILESTONES.map((m, i) => (
            <li key={m.when} className="relative pl-8 md:pl-0 md:pt-10">
              <span
                aria-hidden
                className={`absolute left-0 top-1 size-[15px] rounded-full border-2 md:top-0 ${
                  i === MILESTONES.length - 1 ? "border-accent bg-accent" : "border-line-strong bg-bg"
                }`}
              />
              <Reveal delay={i * 0.06} y={12}>
                <p className="text-[14px] font-medium text-fg">{m.when}</p>
                <p className="mt-2 text-[15px] leading-relaxed text-muted">{m.what}</p>
              </Reveal>
            </li>
          ))}
        </ol>
      </section>

      <section className="border-t border-line">
        <div className="mx-auto flex max-w-[1400px] flex-col items-start justify-between gap-8 px-4 py-20 md:flex-row md:items-center md:px-8">
          <h2 className="max-w-[20ch] text-3xl font-semibold tracking-tighter text-fg md:text-4xl">
            Want to work on the tools you use every day?
          </h2>
          <Link
            href="/careers"
            className="group inline-flex h-12 shrink-0 items-center gap-2 rounded-lg bg-accent px-5 text-[15px] font-medium text-accent-fg transition hover:brightness-110 active:scale-[0.98]"
          >
            See open roles
            <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
          </Link>
        </div>
      </section>
    </>
  );
}
