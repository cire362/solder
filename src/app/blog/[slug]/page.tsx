import type { Metadata } from "next";
import Image from "next/image";
import Link from "next/link";
import { notFound } from "next/navigation";
import { ArrowLeft } from "@phosphor-icons/react/ssr";
import { Author } from "@/components/author";
import { FloraCluster } from "@/components/flora-cluster";
import { Blocks } from "@/lib/blocks";
import { POSTS, coverUrl, getPost } from "@/content/blog";

export function generateStaticParams() {
  return POSTS.map((p) => ({ slug: p.slug }));
}

export async function generateMetadata({ params }: PageProps<"/blog/[slug]">): Promise<Metadata> {
  const { slug } = await params;
  const post = getPost(slug);
  if (!post) return {};
  return { title: post.title, description: post.excerpt };
}

export default async function PostPage({ params }: PageProps<"/blog/[slug]">) {
  const { slug } = await params;
  const post = getPost(slug);
  if (!post) notFound();
  const more = POSTS.filter((p) => p.slug !== post.slug).slice(0, 2);

  return (
    <article>
      <header className="mx-auto max-w-3xl px-4 pb-12 pt-14 md:px-8 md:pt-20">
        <Link href="/blog" className="group inline-flex items-center gap-2 text-[14px] text-muted hover:text-fg">
          <ArrowLeft className="size-4 transition-transform group-hover:-translate-x-0.5" />
          All posts
        </Link>
        <p className="mt-10 text-[14px] text-subtle">
          <span className="text-accent">{post.tag}</span>
          <span className="mx-2">/</span>
          <time dateTime={post.iso}>{post.date}</time>
          <span className="mx-2">/</span>
          {post.minutes} min read
        </p>
        <h1 className="mt-4 text-4xl font-semibold leading-[1.05] tracking-tighter text-fg md:text-[56px]">{post.title}</h1>
        <p className="mt-5 text-lg leading-relaxed text-muted">{post.excerpt}</p>
        <div className="mt-8">
          <Author author={post.author} size="lg" />
        </div>
      </header>

      <div className="mx-auto max-w-[1200px] px-4 md:px-8">
        <div className="relative aspect-[21/9] overflow-hidden rounded-2xl border border-line bg-sunken">
          <Image src={coverUrl(post.cover, 2000, 860)} alt="" fill priority sizes="(min-width: 1200px) 1200px, 100vw" className="object-cover opacity-90" />
        </div>
      </div>

      <div className="mx-auto max-w-3xl px-4 py-16 md:px-8 md:py-20">
        <Blocks blocks={post.body} />
        {/* End-of-article ornament. */}
        <FloraCluster preset="fleuron" trigger="view" className="mx-auto mt-16 block w-[180px]" />
      </div>

      <aside className="border-t border-line">
        <div className="mx-auto max-w-[1400px] px-4 py-20 md:px-8">
          <h2 className="text-2xl font-semibold tracking-tight text-fg">Keep reading</h2>
          <ul className="mt-8 grid gap-4 md:grid-cols-2">
            {more.map((p) => (
              <li key={p.slug}>
                <Link
                  href={`/blog/${p.slug}`}
                  className="group flex h-full gap-5 rounded-2xl border border-line p-4 transition-colors hover:border-line-strong"
                >
                  <div className="relative aspect-square w-28 shrink-0 overflow-hidden rounded-lg bg-sunken">
                    <Image src={coverUrl(p.cover, 400, 400)} alt="" fill sizes="112px" className="object-cover opacity-90" />
                  </div>
                  <div className="py-1">
                    <p className="text-[13px] text-accent">{p.tag}</p>
                    <h3 className="mt-1 text-lg font-medium leading-snug text-fg group-hover:text-accent">{p.title}</h3>
                    <p className="mt-1 text-[14px] text-subtle">{p.minutes} min read</p>
                  </div>
                </Link>
              </li>
            ))}
          </ul>
        </div>
      </aside>
    </article>
  );
}
