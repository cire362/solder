"use client";

import { useId, useState } from "react";
import { Plus } from "@phosphor-icons/react";
import { Collapse } from "./collapse";

export function Faq({ items }: { items: { q: string; a: React.ReactNode }[] }) {
  const [open, setOpen] = useState<number | null>(0);
  const baseId = useId();

  return (
    <div className="divide-y divide-line border-y border-line">
      {items.map((item, i) => {
        const isOpen = open === i;
        const panelId = `${baseId}-${i}`;
        return (
          <div key={item.q}>
            <h3>
              <button
                type="button"
                aria-expanded={isOpen}
                aria-controls={panelId}
                onClick={() => setOpen(isOpen ? null : i)}
                className="flex w-full items-center justify-between gap-6 py-5 text-left text-[17px] font-medium text-fg"
              >
                {item.q}
                <Plus
                  className={`size-5 shrink-0 transition-[transform,color] duration-[400ms] ease-[cubic-bezier(0.16,1,0.3,1)] ${
                    isOpen ? "rotate-45 text-accent" : "text-subtle"
                  }`}
                />
              </button>
            </h3>
            <Collapse open={isOpen} id={panelId}>
              <div className="max-w-[62ch] pb-6 text-[15.5px] leading-relaxed text-muted">{item.a}</div>
            </Collapse>
          </div>
        );
      })}
    </div>
  );
}
