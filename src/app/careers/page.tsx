import type { Metadata } from "next";
import { PageIntro } from "@/components/page-intro";
import { RolesList } from "@/components/roles-list";
import { Reveal } from "@/components/reveal";

export const metadata: Metadata = {
  title: "Careers",
  description: "Join Solder Labs and build a native IDE for fullstack developers.",
};

// mock: placeholder policies and benefits
const HOW = [
  ["Writing first", "Decisions happen in short written proposals, not meetings. Most weeks have two calls or fewer."],
  ["Own it end to end", "You design, build, ship and support what you work on, and you talk to the people who use it."],
  ["Demo on Thursdays", "Everyone shows something running every week, however rough. Solder is built in Solder."],
];

const BENEFITS = [
  ["Published salary bands", "The same band for a role wherever you live, plus equity for everyone."],
  ["Equipment you choose", "A laptop and desk setup budget, refreshed every three years."],
  ["Time off that gets used", "30 days of paid leave plus public holidays, with a 15-day minimum we check."],
  ["Learning and health", "A yearly budget for courses, books and conferences, and health cover in every country we hire."],
];

export default function CareersPage() {
  return (
    <>
      <PageIntro
        flora="cosmos"
        title="Build the editor you use every day."
        body="We are a small, remote team. Everyone ships, everyone talks to users, and everyone uses Solder to build Solder."
      />

      <section className="mx-auto max-w-[1400px] px-4 pb-24 md:px-8 md:pb-32">
        <Reveal>
          <h2 className="text-2xl font-semibold tracking-tight text-fg md:text-3xl">Open roles</h2>
        </Reveal>
        <Reveal delay={0.06} className="mt-8">
          <RolesList />
        </Reveal>
      </section>

      <section className="border-t border-line bg-sunken">
        <div className="mx-auto grid max-w-[1400px] gap-16 px-4 py-24 md:px-8 md:py-28 lg:grid-cols-2">
          <Reveal>
            <h2 className="text-2xl font-semibold tracking-tight text-fg md:text-3xl">How we work</h2>
            <dl className="mt-10 space-y-8">
              {HOW.map(([t, d]) => (
                <div key={t} className="border-l-2 border-accent pl-5">
                  <dt className="text-[17px] font-medium text-fg">{t}</dt>
                  <dd className="mt-1.5 max-w-[52ch] text-[15.5px] leading-relaxed text-muted">{d}</dd>
                </div>
              ))}
            </dl>
          </Reveal>
          <Reveal delay={0.08}>
            <h2 className="text-2xl font-semibold tracking-tight text-fg md:text-3xl">What you get</h2>
            <dl className="mt-10 grid gap-4 sm:grid-cols-2">
              {BENEFITS.map(([t, d]) => (
                <div key={t} className="rounded-2xl border border-line bg-bg p-5">
                  <dt className="text-[16px] font-medium text-fg">{t}</dt>
                  <dd className="mt-1.5 text-[14.5px] leading-relaxed text-muted">{d}</dd>
                </div>
              ))}
            </dl>
          </Reveal>
        </div>
      </section>
    </>
  );
}
