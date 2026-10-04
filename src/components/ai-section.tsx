"use client";

import { useEffect, useRef, useState } from "react";
import { AnimatePresence, motion, useInView, useReducedMotion } from "motion/react";
import {
  ChatCircleText,
  Check,
  CircleNotch,
  GitDiff,
  Pause,
  Play,
  Robot,
  ShieldCheck,
  Sparkle,
  TreeStructure,
} from "@phosphor-icons/react";
import { Code } from "@/lib/highlight";
import { Reveal } from "./reveal";

const ease = [0.16, 1, 0.3, 1] as const;

const MODES = [
  {
    id: "ask",
    Icon: ChatCircleText,
    title: "Ask the codebase",
    body: "Questions are answered with the component, route, schema and logs in view.",
  },
  {
    id: "edit",
    Icon: TreeStructure,
    title: "Edit across the stack",
    body: "One instruction becomes a coordinated change in the migration, the API and the UI.",
  },
  {
    id: "agent",
    Icon: Robot,
    title: "Hand off a task",
    body: "The agent plans, edits, runs your tests and stops for your review.",
  },
  {
    id: "review",
    Icon: GitDiff,
    title: "Review before you push",
    body: "Catches N+1 queries, missing auth checks and type drift between client and server.",
  },
] as const;

type ModeId = (typeof MODES)[number]["id"];

// How long each demo stays up during the auto tour. The agent demo needs ~5s for its steps.
const DWELL: Record<ModeId, number> = { ask: 7, edit: 8, agent: 9, review: 7 };

const line = {
  hidden: { opacity: 0, y: 8 },
  show: (i: number) => ({ opacity: 1, y: 0, transition: { delay: 0.08 * i, duration: 0.45, ease } }),
};

function Prompt({ children }: { children: React.ReactNode }) {
  return (
    <motion.div
      variants={line}
      custom={0}
      className="ml-auto w-fit max-w-[85%] rounded-2xl rounded-br-md bg-sunken px-4 py-2.5 text-[14px] text-fg"
    >
      {children}
    </motion.div>
  );
}

function Mono({ children }: { children: React.ReactNode }) {
  return (
    <code className="rounded-md bg-sunken px-1.5 py-0.5 font-mono text-[12.5px] text-fg">{children}</code>
  );
}

function AskDemo() {
  const refs = [
    "db/migrations/0007_orders.sql:5",
    "apps/api/src/routes/orders.ts:8",
    "apps/web/app/orders/page.tsx:5",
  ];
  return (
    <>
      <Prompt>Why do new customers see an empty orders page?</Prompt>
      <motion.div variants={line} custom={1} className="flex gap-3">
        <Sparkle weight="fill" className="mt-1 size-4 shrink-0 text-accent" />
        <div className="space-y-3 text-[14px] leading-relaxed text-muted">
          <p>
            New orders are created with status <Mono>{`'pending'`}</Mono> by the migration default. The API
            only returns <Mono>{`'paid'`}</Mono> orders, and the page never asks for anything else.
          </p>
          <p>Show pending orders with a badge, or add an empty state that explains the wait.</p>
        </div>
      </motion.div>
      <motion.ul variants={line} custom={2} className="flex flex-wrap gap-2 pl-7">
        {refs.map((r) => (
          <li
            key={r}
            className="rounded-lg border border-line px-2.5 py-1 font-mono text-[11.5px] text-muted"
          >
            {r}
          </li>
        ))}
      </motion.ul>
    </>
  );
}

function EditDemo() {
  const [applied, setApplied] = useState(false);
  const files = [
    ["db/migrations/0008_total_cents.sql", "+6", ""],
    ["apps/api/src/schema.ts", "+2", "-2"],
    ["apps/api/src/routes/orders.ts", "+1", "-1"],
    ["apps/web/app/orders/order-row.tsx", "+3", "-1"],
  ];
  return (
    <>
      <Prompt>Store order totals as cents everywhere and format them in the UI.</Prompt>
      <motion.ul variants={line} custom={1} className="divide-y divide-line rounded-2xl border border-line">
        {files.map(([f, add, del]) => (
          <li key={f} className="flex items-center justify-between gap-3 px-4 py-2.5 font-mono text-[12px]">
            <span className="truncate text-fg">{f}</span>
            <span className="shrink-0">
              <span className="text-accent">{add}</span> <span className="text-subtle">{del}</span>
            </span>
          </li>
        ))}
      </motion.ul>
      <motion.div variants={line} custom={2} className="overflow-hidden rounded-lg border border-line font-mono text-[12px] leading-[1.9]">
        <p className="bg-sunken px-3 text-subtle">
          - {"<td>{order.total}</td>"}
        </p>
        <p className="bg-accent-soft px-3">
          <span className="text-accent">+ </span>
          <Code line="<td>{formatCents(order.totalCents)}</td>" />
        </p>
      </motion.div>
      <motion.div variants={line} custom={3} className="flex items-center gap-3">
        <button
          type="button"
          onClick={() => setApplied(true)}
          disabled={applied}
          className="inline-flex h-9 items-center gap-2 rounded-lg bg-fg px-3.5 text-[13px] font-medium text-bg transition active:scale-[0.98] disabled:opacity-100"
        >
          {applied ? <Check weight="bold" className="size-3.5" /> : null}
          {applied ? "Applied to 4 files" : "Apply 4 files"}
        </button>
        <span className="text-[13px] text-subtle">Type check passes on all 4 files</span>
      </motion.div>
    </>
  );
}

const STEPS = [
  "Read schema, route and page",
  "Add cursor param to GET /orders",
  "Update the typed API client",
  "Add “Load more” to the orders page",
  "Run tests",
];

function AgentDemo() {
  const reduce = useReducedMotion();
  const [done, setDone] = useState(reduce ? STEPS.length : 0);

  useEffect(() => {
    if (reduce) return;
    const id = window.setInterval(() => {
      setDone((d) => {
        if (d >= STEPS.length) {
          window.clearInterval(id);
          return d;
        }
        return d + 1;
      });
    }, 900);
    return () => window.clearInterval(id);
  }, [reduce]);

  return (
    <>
      <Prompt>Add cursor pagination to the orders page.</Prompt>
      <motion.ol variants={line} custom={1} className="space-y-2.5 pl-1">
        {STEPS.map((s, i) => {
          const state = i < done ? "done" : i === done ? "running" : "queued";
          return (
            <li key={s} className="flex items-center gap-3 text-[14px]">
              <span
                className={`grid size-5 place-items-center rounded-full border ${
                  state === "done" ? "border-accent bg-accent text-accent-fg" : "border-line-strong text-subtle"
                }`}
              >
                {state === "done" && <Check weight="bold" className="size-3" />}
                {state === "running" && <CircleNotch className="size-3 animate-spin motion-reduce:animate-none" />}
              </span>
              <span className={state === "queued" ? "text-subtle" : "text-fg"}>{s}</span>
              {i === STEPS.length - 1 && state === "done" && (
                <span className="font-mono text-[12px] text-subtle">14 passed</span>
              )}
            </li>
          );
        })}
      </motion.ol>
      <motion.p variants={line} custom={2} className="pl-1 text-[13px] text-subtle">
        {done >= STEPS.length
          ? "Ready for review. 4 files changed, nothing committed yet."
          : "Working in a sandboxed branch. You can stop it at any step."}
      </motion.p>
    </>
  );
}

function ReviewDemo() {
  const [fixed, setFixed] = useState(false);
  return (
    <>
      <motion.div variants={line} custom={0} className="overflow-hidden rounded-2xl border border-line font-mono text-[12px] leading-[1.9]">
        <p className="border-b border-line bg-sunken px-4 py-1.5 text-[11.5px] text-subtle">apps/api/src/jobs/receipts.ts</p>
        <div className="px-4 py-2">
          {fixed ? (
            <>
              <p><Code line={`const rows = await db.query(`} /></p>
              <p><Code line={`  "SELECT * FROM orders WHERE id = ANY($1)", [ids],`} /></p>
              <p><Code line={`);`} /></p>
            </>
          ) : (
            <>
              <p><Code line={`for (const id of ids) {`} /></p>
              <p className="-mx-4 bg-accent-soft px-4"><Code line={`  const order = await db.query("SELECT * FROM orders WHERE id = $1", [id]);`} /></p>
              <p><Code line={`  receipts.push(render(order));`} /></p>
              <p><Code line={`}`} /></p>
            </>
          )}
        </div>
      </motion.div>
      <motion.div variants={line} custom={1} className="flex gap-3 rounded-2xl border border-line bg-sunken p-4">
        <Sparkle weight="fill" className="mt-0.5 size-4 shrink-0 text-accent" />
        <div className="text-[14px] leading-relaxed text-muted">
          {fixed ? (
            <p className="text-fg">Fixed. One query instead of one per order.</p>
          ) : (
            <>
              <p>
                This runs one query per order. A batch of 500 receipts makes 500 round trips. Fetch them
                together with <Mono>ANY($1)</Mono>.
              </p>
              <button
                type="button"
                onClick={() => setFixed(true)}
                className="mt-3 inline-flex h-8 items-center rounded-lg bg-fg px-3 text-[13px] font-medium text-bg active:scale-[0.98]"
              >
                Apply fix
              </button>
            </>
          )}
        </div>
      </motion.div>
    </>
  );
}

const DEMOS: Record<ModeId, () => React.JSX.Element> = {
  ask: AskDemo,
  edit: EditDemo,
  agent: AgentDemo,
  review: ReviewDemo,
};

export function AISection() {
  const [mode, setMode] = useState<ModeId>("ask");
  const [cycle, setCycle] = useState(0); // restarts the progress bar on manual selection
  const [playing, setPlaying] = useState(true);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const stage = useRef<HTMLDivElement>(null);
  const inView = useInView(stage, { amount: 0.4 });
  const reduce = useReducedMotion();
  const Demo = DEMOS[mode];

  // Auto tour: never under reduced motion, paused while the reader hovers, types or scrolls away.
  const autoplay = playing && !reduce;
  const running = autoplay && inView && !hovered && !focused;

  function select(id: ModeId) {
    setMode(id);
    setCycle((c) => c + 1);
  }

  function next() {
    const i = MODES.findIndex((m) => m.id === mode);
    setMode(MODES[(i + 1) % MODES.length].id);
    setCycle((c) => c + 1);
  }

  return (
    <section id="ai" className="mx-auto max-w-[1400px] px-4 py-24 md:px-8 md:py-32">
      <Reveal className="max-w-3xl">
        <p className="mb-5 font-mono text-[12px] uppercase tracking-[0.2em] text-accent">Solder AI</p>
        <h2 className="text-4xl font-semibold leading-[1.05] tracking-tighter text-fg md:text-5xl">
          An AI pair that has read your whole stack.
        </h2>
        <p className="mt-5 max-w-[60ch] text-lg leading-relaxed text-muted">
          It sees the component, the route, the schema and the logs together, so its answers and edits line up across every layer.
        </p>
      </Reveal>

      <div
        ref={stage}
        onPointerEnter={() => setHovered(true)}
        onPointerLeave={() => setHovered(false)}
        onFocus={(e) => {
          // Keyboard focus inside the demo pauses the tour; clicking tabs does not.
          if (!(e.target as HTMLElement).closest("[role=tab]")) setFocused(true);
        }}
        onBlur={() => setFocused(false)}
        className="mt-14 grid gap-6 lg:grid-cols-[minmax(0,5fr)_minmax(0,7fr)] lg:gap-10"
      >
        <div className="flex flex-col justify-between gap-10">
          <div role="tablist" aria-label="AI capabilities" className="flex flex-col">
            {MODES.map((m) => {
              const active = m.id === mode;
              return (
                <button
                  key={m.id}
                  role="tab"
                  id={`ai-tab-${m.id}`}
                  aria-selected={active}
                  aria-controls="ai-panel"
                  type="button"
                  onClick={() => select(m.id)}
                  className="group relative flex gap-4 rounded-lg py-4 pl-5 pr-3 text-left"
                >
                  <span aria-hidden className="absolute inset-y-3 left-0 w-px bg-line-strong" />
                  {active &&
                    (autoplay ? (
                      // Progress of the auto tour. When it fills, the next mode opens.
                      <span
                        key={cycle}
                        aria-hidden
                        onAnimationEnd={next}
                        className="ai-progress absolute inset-y-3 left-0 w-[2px] origin-top bg-accent"
                        style={{
                          animationDuration: `${DWELL[m.id]}s`,
                          animationPlayState: running ? "running" : "paused",
                        }}
                      />
                    ) : (
                      <motion.span
                        layoutId="ai-indicator"
                        transition={{ type: "spring", stiffness: 380, damping: 32 }}
                        className="absolute inset-y-3 left-0 w-[2px] bg-accent"
                      />
                    ))}
                  <m.Icon
                    className={`mt-0.5 size-5 shrink-0 transition-colors ${active ? "text-accent" : "text-subtle group-hover:text-muted"}`}
                  />
                  <span>
                    <span className={`block text-[16px] font-medium transition-colors ${active ? "text-fg" : "text-muted group-hover:text-fg"}`}>
                      {m.title}
                    </span>
                    <span
                      className={`mt-1 block text-[14px] leading-relaxed text-subtle transition-opacity ${
                        active ? "opacity-100" : "opacity-70"
                      }`}
                    >
                      {m.body}
                    </span>
                  </span>
                </button>
              );
            })}
          </div>

          {!reduce && (
            <button
              type="button"
              aria-pressed={!playing}
              onClick={() => setPlaying((p) => !p)}
              className="-mt-6 inline-flex w-fit items-center gap-2 rounded-lg px-2 py-1.5 text-[13px] text-subtle transition-colors hover:text-fg"
            >
              {playing ? <Pause weight="fill" className="size-3.5" /> : <Play weight="fill" className="size-3.5" />}
              {playing ? "Pause tour" : "Play tour"}
            </button>
          )}

          <div className="flex gap-3 border-t border-line pt-6 text-[14px] leading-relaxed text-muted">
            <ShieldCheck className="mt-0.5 size-5 shrink-0 text-fg" />
            <p>
              Use hosted models, bring your own API key or run local models. Your code is never stored or used for training.
            </p>
          </div>
        </div>

        <div
          id="ai-panel"
          role="tabpanel"
          aria-labelledby={`ai-tab-${mode}`}
          className="relative min-h-[420px] overflow-hidden rounded-2xl border border-line-strong bg-elev p-5 shadow-window md:p-8"
        >
          <AnimatePresence mode="wait">
            <motion.div
              key={mode}
              initial="hidden"
              animate="show"
              exit={{ opacity: 0, y: -8, transition: { duration: 0.15 } }}
              className="flex flex-col gap-5"
            >
              <Demo />
            </motion.div>
          </AnimatePresence>
        </div>
      </div>
    </section>
  );
}
