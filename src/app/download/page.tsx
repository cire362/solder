import type { Metadata } from "next";
import Link from "next/link";
import { ArrowsClockwise, Keyboard, ShieldCheck } from "@phosphor-icons/react/ssr";
import { PageIntro } from "@/components/page-intro";
import { DownloadPicker } from "@/components/download-picker";
import { Reveal } from "@/components/reveal";

export const metadata: Metadata = {
  title: "Download",
  description: "Download Solder for macOS, Windows and Linux.",
};

const NOTES = [
  {
    Icon: Keyboard,
    title: "Bring your setup",
    body: "On first launch Solder offers to import settings, keymaps and themes from VS Code, Cursor or JetBrains IDEs.",
    link: ["Importing settings", "/docs/importing-settings"],
  },
  {
    Icon: ArrowsClockwise,
    title: "Quiet updates",
    body: "Updates download in the background and apply on the next restart. You can pin a version per project.",
  },
  {
    Icon: ShieldCheck,
    title: "Signed and notarized",
    body: "Every build is code-signed. Checksums and signatures are published with each release.",
    link: ["Security", "/security"],
  },
];

export default function DownloadPage() {
  return (
    <>
      <PageIntro
        flora="tulips"
        title="Download Solder"
        body="One native app for macOS, Windows and Linux. Free for personal projects, no account required."
      />
      <DownloadPicker />

      <section className="border-t border-line bg-sunken">
        <div className="mx-auto grid max-w-[1400px] gap-10 px-4 py-20 md:grid-cols-3 md:gap-0 md:px-8">
          {NOTES.map((n, i) => (
            <Reveal
              key={n.title}
              delay={i * 0.06}
              className={`md:px-8 ${i === 0 ? "md:pl-0" : "md:border-l md:border-line"}`}
            >
              <n.Icon className="mb-4 size-6 text-accent" />
              <h2 className="text-lg font-medium text-fg">{n.title}</h2>
              <p className="mt-2 max-w-[40ch] text-[15px] leading-relaxed text-muted">{n.body}</p>
              {n.link && (
                <Link href={n.link[1]} className="mt-3 inline-block text-[15px] text-fg underline decoration-accent underline-offset-4">
                  {n.link[0]}
                </Link>
              )}
            </Reveal>
          ))}
        </div>
      </section>
    </>
  );
}
