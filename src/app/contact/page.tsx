import type { Metadata } from "next";
import Link from "next/link";
import { BookOpen, Briefcase, Megaphone, ShieldCheck } from "@phosphor-icons/react/ssr";
import { PageIntro } from "@/components/page-intro";
import { ContactForm } from "@/components/contact-form";
import { Reveal } from "@/components/reveal";

export const metadata: Metadata = {
  title: "Contact",
  description: "Talk to the Solder team about plans, support, partnerships or press.",
};

const CHANNELS = [
  {
    Icon: BookOpen,
    title: "Stuck on something?",
    body: "Most answers are in the docs. The community Discord is fast for everything else.",
    link: { label: "Read the docs", href: "/docs" },
  },
  {
    Icon: Briefcase,
    title: "Team and enterprise plans",
    body: "SSO, self-hosted AI gateways and custom contracts.",
    link: { label: "sales@solder.dev", href: "mailto:sales@solder.dev" },
  },
  {
    Icon: Megaphone,
    title: "Press",
    body: "Interviews, logos and product screenshots.",
    link: { label: "press@solder.dev", href: "mailto:press@solder.dev" },
  },
  {
    Icon: ShieldCheck,
    title: "Report a vulnerability",
    body: "Please do not use this form. Follow our disclosure policy instead.",
    link: { label: "Security policy", href: "/security" },
  },
];

export default async function ContactPage({ searchParams }: PageProps<"/contact">) {
  const { topic, role } = await searchParams;
  const one = (v: string | string[] | undefined) => (Array.isArray(v) ? v[0] : v);
  return (
    <>
      <PageIntro
        flora="sprig"
        title="Talk to the team"
        body="Questions about plans, a partnership idea or something we should fix. A person reads every message."
      />

      <section className="mx-auto grid max-w-[1400px] gap-12 px-4 pb-24 md:px-8 md:pb-32 lg:grid-cols-[minmax(0,4fr)_minmax(0,7fr)] lg:gap-16">
        <Reveal>
          <ul className="space-y-8">
            {CHANNELS.map((c) => (
              <li key={c.title} className="flex gap-4">
                <c.Icon className="mt-0.5 size-5 shrink-0 text-accent" />
                <div>
                  <h2 className="text-[16px] font-medium text-fg">{c.title}</h2>
                  <p className="mt-1 max-w-[40ch] text-[15px] leading-relaxed text-muted">{c.body}</p>
                  <Link href={c.link.href} className="mt-2 inline-block text-[15px] text-fg underline decoration-accent underline-offset-4">
                    {c.link.label}
                  </Link>
                </div>
              </li>
            ))}
          </ul>
        </Reveal>

        <Reveal delay={0.06}>
          <ContactForm key={`${one(topic)}-${one(role)}`} topicParam={one(topic)} roleParam={one(role)} />
        </Reveal>
      </section>
    </>
  );
}
