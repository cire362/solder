import type { Metadata } from "next";
import Image from "next/image";
import Link from "next/link";
import { ArrowRight } from "@phosphor-icons/react/ssr";
import { PageIntro } from "@/components/page-intro";
import { BlogList } from "@/components/blog-list";
import { Author } from "@/components/author";
import { SubscribeForm } from "@/components/subscribe-form";
import { Reveal } from "@/components/reveal";
import { POSTS, coverUrl } from "@/content/blog";

export const metadata: Metadata = {
  title: "Blog",
  description: "How we build Solder: engineering deep dives, AI research and product updates.",
};

export default function BlogPage() {
  const [featured, ...rest] = POSTS;

  return (
    <>
      <PageIntro
        flora="wild"
        title="Blog"
        body="Engineering deep dives, AI research and the reasoning behind each release."
      />

      <section className="mx-auto max-w-[1400px] px-4 md:px-8">
        <Reveal>
          <Link
            href={`/blog/${featured.slug}`}
            className="group grid overflow-hidden rounded-2xl border border-line-strong bg-elev shadow-window lg:grid-cols-[minmax(0,7fr)_minmax(0,5fr)]"
          >
            <div className="relative aspect-[16/10] lg:aspect-auto lg:min-h-[440px]">
              <Image
                src={coverUrl(featured.cover, 1600, 1000)}
                alt=""
                fill
                priority
                sizes="(min-width: 1024px) 58vw, 100vw"
                className="object-cover opacity-90 transition-transform duration-700 ease-[cubic-bezier(0.16,1,0.3,1)] group-hover:scale-[1.02]"
              />
            </div>
            <div className="flex flex-col justify-between gap-10 p-6 md:p-10">
              <div>
                <p className="text-[13px] text-subtle">
                  <span className="text-accent">{featured.tag}</span>
                  <span className="mx-2">/</span>
                  <time dateTime={featured.iso}>{featured.date}</time>
                </p>
                <h2 className="mt-3 text-3xl font-semibold leading-[1.1] tracking-tighter text-fg md:text-4xl">
                  {featured.title}
                </h2>
                <p className="mt-4 text-[16.5px] leading-relaxed text-muted">{featured.excerpt}</p>
              </div>
              <div className="flex items-center justify-between gap-4">
                <Author author={featured.author} />
                <span className="inline-flex items-center gap-1.5 text-[14px] text-fg">
                  Read
                  <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
                </span>
              </div>
            </div>
          </Link>
        </Reveal>
      </section>

      <section className="mx-auto max-w-[1400px] px-4 py-20 md:px-8 md:py-28">
        <BlogList posts={rest} />
      </section>

      <section className="border-t border-line bg-sunken">
        <div className="mx-auto max-w-[1400px] px-4 py-20 md:px-8">
          <h2 className="max-w-[24ch] text-2xl font-semibold tracking-tight text-fg md:text-3xl">
            New posts and release notes, once a month.
          </h2>
          <div className="mt-8">
            <SubscribeForm label="Email address" />
          </div>
        </div>
      </section>
    </>
  );
}
