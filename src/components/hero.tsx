import Link from "next/link";
import { ArrowRight } from "@phosphor-icons/react/ssr";
import { DownloadButton } from "./download-button";
import { EditorPreview } from "./editor-preview";
import { HeroTilt } from "./hero-tilt";
import { HeroFlora } from "./flora-scenes";
import { Reveal } from "./reveal";

export function Hero() {
  return (
    <section id="top" className="relative">
      <div
        aria-hidden
        className="dot-grid pointer-events-none absolute inset-0 [mask-image:radial-gradient(ellipse_60%_55%_at_70%_45%,black,transparent)]"
      />
      <div className="relative mx-auto grid max-w-[1400px] items-center gap-12 px-4 pb-20 pt-12 md:px-8 lg:min-h-[calc(100dvh-4rem)] lg:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)] lg:gap-14 lg:pb-16 lg:pt-10">
        <HeroFlora className="pointer-events-none absolute bottom-0 left-4 hidden w-[220px] md:left-8 lg:block xl:w-[270px]" />
        <div>
          <Reveal y={16}>
            <h1 className="text-[44px] font-semibold leading-[0.98] tracking-tighter text-fg sm:text-6xl lg:whitespace-nowrap lg:text-[50px] xl:text-[60px] 2xl:text-[68px]">
              The whole stack.
              <br />
              <span className="text-muted">One editor.</span>
            </h1>
          </Reveal>
          <Reveal y={16} delay={0.08}>
            <p className="mt-6 max-w-[42ch] text-lg leading-relaxed text-muted">
              Frontend, API, database and deploys in one native workspace, with an AI pair that understands all of it.
            </p>
          </Reveal>
          <Reveal y={16} delay={0.16}>
            <div className="mt-9 flex flex-wrap items-center gap-3">
              <DownloadButton />
              <Link
                href="/docs"
                className="group inline-flex h-12 items-center gap-2 whitespace-nowrap rounded-lg border border-line-strong px-5 text-[15px] font-medium text-fg transition hover:bg-elev active:scale-[0.98]"
              >
                Read the docs
                <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
              </Link>
            </div>
          </Reveal>
        </div>

        <HeroTilt>
          <EditorPreview />
        </HeroTilt>
      </div>
    </section>
  );
}
