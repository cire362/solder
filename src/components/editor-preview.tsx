"use client";

import { useEffect, useRef, useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import {
  CaretDown,
  Database,
  FileSql,
  FileTs,
  FileTsx,
  FolderSimple,
  GitBranch,
  Sparkle,
  Terminal,
} from "@phosphor-icons/react";
import { Code } from "@/lib/highlight";

type FileId = "page" | "route" | "sql";

const FILES: Record<
  FileId,
  { name: string; path: string; Icon: typeof FileTs; lines: string[] }
> = {
  page: {
    name: "page.tsx",
    path: "apps/web/app/orders",
    Icon: FileTsx,
    lines: [
      `import { getOrders } from "@/api/client";`,
      `import { OrderRow } from "./order-row";`,
      ``,
      `export default async function OrdersPage() {`,
      `  const orders = await getOrders({ status: "paid" });`,
      ``,
      `  return (`,
      `    <section className="orders">`,
      `      <h1>Paid orders</h1>`,
      `      {orders.map((order) => (`,
      `        <OrderRow key={order.id} order={order} />`,
      `      ))}`,
      `    </section>`,
      `  );`,
      `}`,
    ],
  },
  route: {
    name: "orders.ts",
    path: "apps/api/src/routes",
    Icon: FileTs,
    lines: [
      `import { Hono } from "hono";`,
      `import { db } from "../db";`,
      `import { OrderStatus } from "../schema";`,
      ``,
      `export const orders = new Hono();`,
      ``,
      `orders.get("/", async (c) => {`,
      `  const status = OrderStatus.parse(c.req.query("status"));`,
      `  const rows = await db.query(`,
      `    "SELECT id, customer, total_cents FROM orders WHERE status = $1",`,
      `    [status],`,
      `  );`,
      `  return c.json(rows);`,
      `});`,
    ],
  },
  sql: {
    name: "0007_orders.sql",
    path: "db/migrations",
    Icon: FileSql,
    lines: [
      `CREATE TABLE orders (`,
      `  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),`,
      `  customer    text NOT NULL,`,
      `  total_cents integer NOT NULL CHECK (total_cents >= 0),`,
      `  status      text NOT NULL DEFAULT 'pending',`,
      `  created_at  timestamptz NOT NULL DEFAULT now()`,
      `);`,
      ``,
      `CREATE INDEX orders_status_idx ON orders (status);`,
    ],
  },
};

// AI suggestion inserted after line index 4 of page.tsx
const SUGGESTION = [
  ``,
  `  if (orders.length === 0) {`,
  `    return <EmptyState title="No paid orders yet" />;`,
  `  }`,
];
const SUGGESTION_AFTER = 4;
const SUGGESTION_TEXT = SUGGESTION.join("\n");

const LOGS: { src: "web" | "api" | "db"; text: string }[] = [
  { src: "api", text: "listening on http://localhost:8787" },
  { src: "web", text: "ready on http://localhost:3000" },
  { src: "db", text: "postgres 17 accepting connections on :5432" },
  { src: "web", text: "GET /orders 200" },
  { src: "api", text: "GET /orders?status=paid 200" },
  { src: "db", text: "SELECT orders WHERE status = 'paid' (4 rows)" },
];

const ROWS = [
  ["a41f…", "Maren Olsen", "12900", "paid"],
  ["c07b…", "Tobias Reuter", "4450", "paid"],
  ["e9d2…", "Ayaan Kapoor", "31800", "paid"],
  ["f13a…", "Lucía Ferrer", "7625", "paid"],
];

function TreeRow({
  depth,
  label,
  Icon,
  active,
  onClick,
}: {
  depth: number;
  label: string;
  Icon: typeof FileTs;
  active?: boolean;
  onClick?: () => void;
}) {
  const content = (
    <>
      <Icon className={`size-3.5 shrink-0 ${active ? "text-accent" : "text-subtle"}`} />
      <span className="truncate">{label}</span>
    </>
  );
  const cls = `flex h-6 w-full items-center gap-1.5 rounded-md pr-2 text-left text-[12px] ${
    active ? "bg-accent-soft text-fg" : "text-muted"
  }`;
  const style = { paddingLeft: 8 + depth * 12 };
  if (!onClick)
    return (
      <div className={cls} style={style}>
        {content}
      </div>
    );
  return (
    <button type="button" onClick={onClick} className={`${cls} hover:text-fg`} style={style}>
      {content}
    </button>
  );
}

export function EditorPreview() {
  const reduce = useReducedMotion();
  const [file, setFile] = useState<FileId>("page");
  const [panel, setPanel] = useState<"terminal" | "db">("terminal");
  const [typedRaw, setTyped] = useState(0);
  const [accepted, setAccepted] = useState(false);
  const [logCountRaw, setLogCount] = useState(0);
  const editorRef = useRef<HTMLDivElement>(null);

  // Stream the dev-server logs in, showing web + api + db booting together.
  useEffect(() => {
    if (reduce) return;
    let i = 0;
    const id = window.setInterval(() => {
      i += 1;
      setLogCount(i);
      if (i >= LOGS.length) window.clearInterval(id);
    }, 520);
    return () => window.clearInterval(id);
  }, [reduce]);

  // Type the AI suggestion as ghost text once the page file is visible.
  useEffect(() => {
    if (file !== "page" || accepted || reduce) return;
    let raf = 0;
    const start = window.setTimeout(() => {
      const t0 = performance.now();
      const step = (now: number) => {
        const n = Math.min(SUGGESTION_TEXT.length, Math.floor((now - t0) / 22));
        setTyped(n);
        if (n < SUGGESTION_TEXT.length) raf = requestAnimationFrame(step);
      };
      raf = requestAnimationFrame(step);
    }, 1400);
    return () => {
      window.clearTimeout(start);
      cancelAnimationFrame(raf);
    };
  }, [file, accepted, reduce]);

  // Under reduced motion everything renders in its final state, no typing or streaming.
  const typed = reduce ? SUGGESTION_TEXT.length : typedRaw;
  const logCount = reduce ? LOGS.length : logCountRaw;
  const suggestionReady = typed >= SUGGESTION_TEXT.length;

  function accept() {
    setTyped(SUGGESTION_TEXT.length);
    setAccepted(true);
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Tab" && file === "page" && !accepted && typed > 0) {
      e.preventDefault();
      accept();
    }
  }

  const f = FILES[file];
  const ghostLines = SUGGESTION_TEXT.slice(0, typed).split("\n");
  const showGhost = file === "page" && typed > 0 && !accepted;

  let lines: { text: string; ghost?: boolean; added?: boolean }[] = f.lines.map((text) => ({ text }));
  if (file === "page" && (accepted || showGhost)) {
    const insert = (accepted ? SUGGESTION : ghostLines).map((text) => ({
      text,
      ghost: !accepted,
      added: accepted,
    }));
    lines = [
      ...lines.slice(0, SUGGESTION_AFTER + 1),
      ...insert,
      ...lines.slice(SUGGESTION_AFTER + 1),
    ];
  }

  return (
    <div
      ref={editorRef}
      tabIndex={0}
      onKeyDown={onKeyDown}
      aria-label="Interactive preview of the Solder editor. Press Tab to accept the AI suggestion."
      className="overflow-hidden rounded-2xl border border-line-strong bg-elev text-left shadow-window outline-none focus-visible:ring-2 focus-visible:ring-accent"
    >
      {/* Title bar */}
      <div className="flex h-10 items-center justify-between border-b border-line px-4">
        <div className="flex gap-1.5" aria-hidden>
          <span className="size-2.5 rounded-full bg-line-strong" />
          <span className="size-2.5 rounded-full bg-line-strong" />
          <span className="size-2.5 rounded-full bg-line-strong" />
        </div>
        <span className="font-mono text-[11px] text-subtle">storefront</span>
        <span className="flex items-center gap-1 font-mono text-[11px] text-subtle">
          <GitBranch className="size-3" />
          main
        </span>
      </div>

      <div className="grid md:grid-cols-[196px_minmax(0,1fr)]">
        {/* Explorer */}
        <aside className="hidden border-r border-line bg-sunken/60 px-2 py-3 md:block">
          <p className="mb-2 px-2 text-[11px] font-medium text-subtle">Explorer</p>
          <TreeRow depth={0} label="apps" Icon={CaretDown} />
          <TreeRow depth={1} label="web" Icon={FolderSimple} />
          <TreeRow
            depth={2}
            label="orders/page.tsx"
            Icon={FileTsx}
            active={file === "page"}
            onClick={() => setFile("page")}
          />
          <TreeRow depth={1} label="api" Icon={FolderSimple} />
          <TreeRow
            depth={2}
            label="routes/orders.ts"
            Icon={FileTs}
            active={file === "route"}
            onClick={() => setFile("route")}
          />
          <TreeRow depth={0} label="db" Icon={CaretDown} />
          <TreeRow
            depth={1}
            label="0007_orders.sql"
            Icon={FileSql}
            active={file === "sql"}
            onClick={() => setFile("sql")}
          />
          <TreeRow depth={0} label="docker-compose.yml" Icon={FolderSimple} />
        </aside>

        <div className="min-w-0">
          {/* Tabs */}
          <div className="flex h-9 items-stretch overflow-x-auto border-b border-line" role="tablist">
            {(Object.keys(FILES) as FileId[]).map((id) => {
              const { name, Icon } = FILES[id];
              const active = id === file;
              return (
                <button
                  key={id}
                  role="tab"
                  aria-selected={active}
                  type="button"
                  onClick={() => setFile(id)}
                  className={`relative flex shrink-0 items-center gap-1.5 border-r border-line px-3 font-mono text-[11.5px] transition-colors ${
                    active ? "bg-elev text-fg" : "bg-sunken/60 text-subtle hover:text-muted"
                  }`}
                >
                  <Icon className={`size-3.5 ${active ? "text-accent" : ""}`} />
                  {name}
                  {active && (
                    <motion.span
                      layoutId="editor-tab"
                      className="absolute inset-x-0 top-0 h-[2px] bg-accent"
                    />
                  )}
                </button>
              );
            })}
          </div>

          {/* Code */}
          <div className="relative h-[268px] overflow-auto py-3 font-mono text-[11.5px] leading-[1.75] sm:text-[12.5px]">
            <p className="mb-1 px-4 text-[11px] text-subtle">{f.path}/{f.name}</p>
            <AnimatePresence mode="wait" initial={false}>
              <motion.div
                key={file}
                initial={{ opacity: 0, y: 6 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -6 }}
                transition={{ duration: 0.18 }}
              >
                {lines.map((l, i) => (
                  <div
                    key={i}
                    className={`flex whitespace-pre pr-4 ${l.added ? "bg-accent-soft" : ""}`}
                  >
                    <span className="w-10 shrink-0 select-none pr-3 text-right text-subtle/70">
                      {i + 1}
                    </span>
                    {l.ghost ? (
                      <span className="text-subtle italic opacity-80">
                        {l.text || " "}
                        {i === SUGGESTION_AFTER + ghostLines.length && !suggestionReady && (
                          <span className="caret ml-px inline-block h-[1.1em] w-[2px] translate-y-[2px] bg-accent" />
                        )}
                      </span>
                    ) : (
                      <span>
                        <Code line={l.text} />
                      </span>
                    )}
                  </div>
                ))}
              </motion.div>
            </AnimatePresence>

            <AnimatePresence>
              {showGhost && suggestionReady && (
                <motion.button
                  type="button"
                  onClick={accept}
                  initial={{ opacity: 0, y: 4 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0 }}
                  className="absolute right-4 top-[150px] flex items-center gap-1.5 rounded-lg border border-line-strong bg-elev px-2.5 py-1 font-sans text-[11px] text-muted shadow-window hover:text-fg"
                >
                  <Sparkle weight="fill" className="size-3 text-accent" />
                  Accept
                  <kbd className="rounded-md border border-line-strong px-1 font-mono text-[10px]">Tab</kbd>
                </motion.button>
              )}
            </AnimatePresence>
          </div>

          {/* Bottom panel */}
          <div className="border-t border-line">
            <div className="flex h-8 items-center gap-4 px-4 text-[11px]">
              {(
                [
                  ["terminal", "Terminal", Terminal],
                  ["db", "Database", Database],
                ] as const
              ).map(([id, label, Icon]) => (
                <button
                  key={id}
                  type="button"
                  onClick={() => setPanel(id)}
                  className={`flex items-center gap-1.5 ${panel === id ? "text-fg" : "text-subtle hover:text-muted"}`}
                >
                  <Icon className="size-3.5" />
                  {label}
                </button>
              ))}
            </div>
            <div className="h-[118px] overflow-hidden px-4 pb-3 font-mono text-[11px] leading-[1.7]">
              {panel === "terminal" ? (
                <ul>
                  {LOGS.slice(0, logCount).map((l, i) => (
                    <motion.li
                      key={i}
                      initial={{ opacity: 0, x: -4 }}
                      animate={{ opacity: 1, x: 0 }}
                      className="flex gap-3 whitespace-nowrap"
                    >
                      <span className={`w-6 ${l.src === "web" ? "text-accent" : "text-subtle"}`}>{l.src}</span>
                      <span className="truncate text-muted">{l.text}</span>
                    </motion.li>
                  ))}
                </ul>
              ) : (
                <table className="w-full text-left">
                  <thead className="text-subtle">
                    <tr>
                      <th className="font-normal">id</th>
                      <th className="font-normal">customer</th>
                      <th className="font-normal">total_cents</th>
                      <th className="hidden font-normal sm:table-cell">status</th>
                    </tr>
                  </thead>
                  <tbody className="text-muted">
                    {ROWS.map((r) => (
                      <tr key={r[0]}>
                        <td>{r[0]}</td>
                        <td className="text-fg">{r[1]}</td>
                        <td className="tk-num">{r[2]}</td>
                        <td className="hidden sm:table-cell">{r[3]}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* Status bar */}
      <div className="flex h-7 items-center justify-between border-t border-line bg-sunken/60 px-4 font-mono text-[10.5px] text-subtle">
        <span>{accepted ? "AI edit applied to page.tsx" : "3 services running"}</span>
        <span className="flex items-center gap-1.5">
          <Sparkle weight="fill" className="size-3 text-accent" />
          Solder AI
        </span>
      </div>
    </div>
  );
}
