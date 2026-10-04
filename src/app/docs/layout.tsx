import { DocsSidebar } from "@/components/docs-sidebar";
import { DOC_GROUPS } from "@/content/docs";

export default function DocsLayout({ children }: LayoutProps<"/docs">) {
  const groups = DOC_GROUPS.map((g) => ({
    id: g.id,
    title: g.title,
    docs: g.docs.map((d) => ({ slug: d.slug, title: d.title })),
  }));

  return (
    <div className="mx-auto grid max-w-[1400px] px-4 md:px-8 lg:grid-cols-[232px_minmax(0,1fr)] lg:gap-12">
      <DocsSidebar groups={groups} />
      <div className="min-w-0 pb-24 pt-10 md:pt-14">{children}</div>
    </div>
  );
}
