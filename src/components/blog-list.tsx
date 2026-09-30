"use client";

import { useState } from "react";
import Image from "next/image";
import Link from "next/link";
import { AnimatePresence, motion } from "motion/react";
import { TAGS, coverUrl, type Post } from "@/content/blog";
import { Author } from "./author";

export function BlogList({ posts }: { posts: Post[] }) {
  const [tag, setTag] = useState<(typeof TAGS)[number]>("All");
  const shown = posts.filter((p) => tag === "All" || p.tag === tag);

  return (
    <div>
      <div className="flex gap-2 overflow-x-auto pb-1" role="group" aria-label="Filter posts by topic">
        {TAGS.map((t) => (
          <button
            key={t}
            type="button"
            aria-pressed={tag === t}
            onClick={() => setTag(t)}
            className={`h-9 shrink-0 rounded-lg px-3.5 text-[14px] transition-colors ${
              tag === t ? "bg-fg text-bg" : "bg-sunken text-muted hover:text-fg"
            }`}
          >
            {t}
          </button>
        ))}
      </div>

      {shown.length === 0 ? (
        <div className="mt-10 rounded-2xl border border-dashed border-line-strong px-6 py-16 text-center">
          <p className="font-medium text-fg">No more {tag} posts right now</p>
          <p className="mt-1.5 text-[15px] text-muted">The latest one is featured above. Subscribe below to get the next by email.</p>
        </div>
      ) : (
        <ul className="mt-10 grid gap-x-8 gap-y-14 md:grid-cols-2">
          <AnimatePresence initial={false} mode="popLayout">
            {shown.map((p) => (
              <motion.li
                key={p.slug}
                layout
                initial={{ opacity: 0, y: 12 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.3 }}
              >
                <Link href={`/blog/${p.slug}`} className="group block">
                  <div className="relative aspect-[16/10] overflow-hidden rounded-2xl border border-line bg-sunken">
                    <Image
                      src={coverUrl(p.cover, 1200, 750)}
                      alt=""
                      fill
                      sizes="(min-width: 768px) 50vw, 100vw"
                      className="object-cover opacity-90 transition-transform duration-700 ease-[cubic-bezier(0.16,1,0.3,1)] group-hover:scale-[1.03]"
                    />
                  </div>
                  <p className="mt-5 text-[13px] text-subtle">
                    <span className="text-accent">{p.tag}</span>
                    <span className="mx-2">/</span>
                    <time dateTime={p.iso}>{p.date}</time>
                  </p>
                  <h3 className="mt-2 text-xl font-semibold tracking-tight text-fg transition-colors group-hover:text-accent md:text-2xl">
                    {p.title}
                  </h3>
                  <p className="mt-2 max-w-[56ch] text-[15.5px] leading-relaxed text-muted">{p.excerpt}</p>
                  <div className="mt-5">
                    <Author author={p.author} />
                  </div>
                </Link>
              </motion.li>
            ))}
          </AnimatePresence>
        </ul>
      )}
    </div>
  );
}
