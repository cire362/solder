import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { ArrowLeft, ArrowRight, PencilSimple } from "@phosphor-icons/react/ssr";
import { ALL_DOCS, getDoc } from "@/content/docs";
import { Blocks, headings } from "@/lib/blocks";
import { Toc } from "@/components/toc";

export function generateStaticParams() {
  return ALL_DOCS.map((d) => ({ slug: d.slug }));
}

export async function generateMetadata({ params }: PageProps<"/docs/[slug]">): Promise<Metadata> {
  const { slug } = await params;
  const found = getDoc(slug);
  if (!found) return {};
  return { title: `${found.doc.title} (Docs)`, description: found.doc.description };
}

export default async function DocPage({ params }: PageProps<"/docs/[slug]">) {
  const { slug } = await params;
  const found = getDoc(slug);
  if (!found) notFound();
  const { doc, prev, next } = found;
  const toc = headings(doc.body);

  return (
    <div className="grid gap-12 xl:grid-cols-[minmax(0,1fr)_200px]">
      <article className="min-w-0 max-w-[72ch]">
        <p className="text-[14px] text-subtle">
          <Link href="/docs" className="hover:text-fg">Docs</Link>
          <span className="mx-2">/</span>
          {doc.group}
        </p>
        <h1 className="mt-4 text-4xl font-semibold leading-[1.08] tracking-tighter text-fg md:text-5xl">{doc.title}</h1>
        <p className="mt-4 text-lg leading-relaxed text-muted">{doc.description}</p>

        <div className="mt-12">
          <Blocks blocks={doc.body} />
        </div>

        <a
          href="#"
          className="mt-14 inline-flex items-center gap-2 text-[14px] text-muted hover:text-fg"
        >
          <PencilSimple className="size-4" />
          Edit this page on GitHub
        </a>

        <nav aria-label="Pagination" className="mt-8 grid gap-3 border-t border-line pt-8 sm:grid-cols-2">
          {prev ? (
            <Link href={`/docs/${prev.slug}`} className="group rounded-2xl border border-line p-5 transition-colors hover:border-line-strong">
              <span className="flex items-center gap-1.5 text-[13px] text-subtle">
                <ArrowLeft className="size-3.5 transition-transform group-hover:-translate-x-0.5" />
                Previous
              </span>
              <span className="mt-1 block font-medium text-fg">{prev.title}</span>
            </Link>
          ) : (
            <span />
          )}
          {next && (
            <Link href={`/docs/${next.slug}`} className="group rounded-2xl border border-line p-5 text-right transition-colors hover:border-line-strong">
              <span className="flex items-center justify-end gap-1.5 text-[13px] text-subtle">
                Next
                <ArrowRight className="size-3.5 transition-transform group-hover:translate-x-0.5" />
              </span>
              <span className="mt-1 block font-medium text-fg">{next.title}</span>
            </Link>
          )}
        </nav>
      </article>

      <aside className="hidden xl:block">
        <div className="sticky top-28">
          <Toc items={toc} />
        </div>
      </aside>
    </div>
  );
}
