"use client";

import { useEffect, useState } from "react";

// "On this page" list. Highlights the section currently in view via IntersectionObserver.
export function Toc({ items, title = "On this page" }: { items: { id: string; h: string }[]; title?: string }) {
  const [active, setActive] = useState(items[0]?.id);

  useEffect(() => {
    const els = items.map((i) => document.getElementById(i.id)).filter((el): el is HTMLElement => !!el);
    const io = new IntersectionObserver(
      (entries) => {
        const visible = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible[0]) setActive(visible[0].target.id);
      },
      { rootMargin: "-80px 0px -65% 0px" },
    );
    els.forEach((el) => io.observe(el));
    return () => io.disconnect();
  }, [items]);

  if (!items.length) return null;

  return (
    <nav aria-label={title}>
      <p className="text-[13px] font-medium text-fg">{title}</p>
      <ul className="mt-3 space-y-0.5 border-l border-line">
        {items.map((i) => (
          <li key={i.id}>
            <a
              href={`#${i.id}`}
              aria-current={active === i.id ? "location" : undefined}
              className={`-ml-px block border-l py-1 pl-3 text-[13.5px] transition-colors ${
                active === i.id ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg"
              }`}
            >
              {i.h}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}
