import Link from "next/link";
import { ArrowRight } from "@phosphor-icons/react/ssr";
import { NotFoundPath } from "@/components/not-found-path";
import { FloraCluster } from "@/components/flora-cluster";

export default function NotFound() {
  return (
    <section className="mx-auto grid max-w-[1400px] items-center gap-14 px-4 py-24 md:px-8 md:py-32 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <div>
        <p className="font-mono text-sm text-accent">404</p>
        <h1 className="mt-4 text-5xl font-semibold leading-[1] tracking-tighter text-fg md:text-6xl">
          This page does not exist.
        </h1>
        <p className="mt-5 max-w-[46ch] text-lg leading-relaxed text-muted">
          The link may be out of date, or the page moved. The docs and the home page are good places to start again.
        </p>
        <div className="mt-9 flex flex-wrap gap-3">
          <Link
            href="/"
            className="inline-flex h-12 items-center rounded-lg bg-accent px-5 text-[15px] font-medium text-accent-fg transition hover:brightness-110 active:scale-[0.98]"
          >
            Back to home
          </Link>
          <Link
            href="/docs"
            className="group inline-flex h-12 items-center gap-2 rounded-lg border border-line-strong px-5 text-[15px] font-medium text-fg transition hover:bg-elev active:scale-[0.98]"
          >
            Read the docs
            <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
          </Link>
        </div>
        <FloraCluster preset="tulips" delay={0.6} className="-ml-6 mt-10 hidden w-[240px] md:block" />
      </div>

      <NotFoundPath />
    </section>
  );
}
