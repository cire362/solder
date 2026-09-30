import { Info } from "@phosphor-icons/react/ssr";
import { Code } from "./highlight";

// Minimal content model for docs, blog posts and legal pages.
export type Block =
  | { p: string }
  | { h: string; id: string }
  | { list: string[] }
  | { code: string[]; file?: string }
  | { note: string };

/** Renders `backtick` spans as inline code and **double stars** as strong. */
export function Inline({ text }: { text: string }) {
  const parts = text.split(/(`[^`]+`|\*\*[^*]+\*\*)/g);
  return (
    <>
      {parts.map((part, i) => {
        if (part.startsWith("`")) return <code key={i}>{part.slice(1, -1)}</code>;
        if (part.startsWith("**")) return <strong key={i}>{part.slice(2, -2)}</strong>;
        return part;
      })}
    </>
  );
}

export function headings(blocks: Block[]) {
  return blocks.filter((b): b is { h: string; id: string } => "h" in b);
}

export function Blocks({ blocks }: { blocks: Block[] }) {
  return (
    <div className="prose-solder">
      {blocks.map((b, i) => {
        if ("p" in b) return <p key={i}><Inline text={b.p} /></p>;
        if ("h" in b) return <h2 key={i} id={b.id}>{b.h}</h2>;
        if ("list" in b)
          return (
            <ul key={i}>
              {b.list.map((li) => (
                <li key={li}><Inline text={li} /></li>
              ))}
            </ul>
          );
        if ("note" in b)
          return (
            <aside key={i} className="flex gap-3 rounded-2xl border border-line bg-sunken p-4 text-[15px]">
              <Info className="mt-1 size-4 shrink-0 text-accent" />
              <p><Inline text={b.note} /></p>
            </aside>
          );
        return (
          <div key={i} className="overflow-hidden rounded-2xl border border-line bg-elev">
            {b.file && (
              <p className="border-b border-line px-4 py-2.5 font-mono text-[12px] text-subtle">{b.file}</p>
            )}
            <pre className="overflow-x-auto px-4 py-4 font-mono text-[13px] leading-[1.75]">
              {b.code.map((l, j) => (
                <div key={j}><Code line={l} /></div>
              ))}
            </pre>
          </div>
        );
      })}
    </div>
  );
}
