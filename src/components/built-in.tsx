import {
  Bug,
  Cube,
  Database,
  GitBranch,
  GitDiff,
  Globe,
  PaperPlaneTilt,
} from "@phosphor-icons/react/ssr";
import { Reveal } from "./reveal";
import { Code } from "@/lib/highlight";

function CellHead({
  Icon,
  title,
  body,
}: {
  Icon: typeof Bug;
  title: string;
  body: string;
}) {
  return (
    <div className="max-w-[46ch]">
      <Icon className="mb-4 size-5 text-accent" />
      <h3 className="text-lg font-medium tracking-tight text-fg">{title}</h3>
      <p className="mt-1.5 text-[15px] leading-relaxed text-muted">{body}</p>
    </div>
  );
}

const cell =
  "group relative flex h-full flex-col justify-between gap-8 overflow-hidden rounded-2xl border border-line p-6 transition-colors duration-300 hover:border-line-strong md:p-7";

function DatabaseCell() {
  const rows = [
    ["Maren Olsen", "3", "412.50"],
    ["Ayaan Kapoor", "2", "318.00"],
    ["Lucía Ferrer", "5", "276.25"],
  ];
  return (
    <div className={`${cell} bg-[linear-gradient(135deg,var(--accent-soft),transparent_55%)] bg-elev`}>
      <CellHead
        Icon={Database}
        title="A real database client"
        body="Browse tables, run queries and edit rows in Postgres, MySQL, SQLite, Redis and MongoDB. Autocomplete knows your schema."
      />
      <div className="overflow-hidden rounded-2xl border border-line bg-bg/70 font-mono text-[12px]">
        <div className="border-b border-line px-4 py-3 leading-relaxed">
          <Code line="SELECT customer, COUNT(*), SUM(total_cents) / 100.0" />
          <br />
          <Code line="FROM orders WHERE status = 'paid' GROUP BY customer" />
        </div>
        <table className="w-full text-left">
          <thead className="text-subtle">
            <tr>
              <th className="px-4 py-2 font-normal">customer</th>
              <th className="px-4 py-2 font-normal">count</th>
              <th className="px-4 py-2 text-right font-normal">revenue</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r[0]} className="text-muted">
                <td className="px-4 py-1.5 text-fg">{r[0]}</td>
                <td className="px-4 py-1.5">{r[1]}</td>
                <td className="tk-num px-4 py-1.5 text-right">{r[2]}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function HttpCell() {
  return (
    <div className={`${cell} bg-elev`}>
      <CellHead
        Icon={PaperPlaneTilt}
        title="HTTP client that knows your routes"
        body="Requests are generated from your API code, so you test the endpoints you actually wrote."
      />
      <div className="rounded-2xl border border-line bg-bg/70 p-4 font-mono text-[12px]">
        <p>
          <span className="text-accent">GET</span> <span className="text-fg">/orders?status=paid</span>
        </p>
        <p className="mt-2 text-subtle">200 OK in 12 ms</p>
        <pre className="mt-2 leading-relaxed">
          <Code line={`[{ "id": "a41f", "total_cents": 12900 }]`} />
        </pre>
      </div>
    </div>
  );
}

function DebuggerCell() {
  const frames = [
    ["OrdersPage", "page.tsx:5", "browser"],
    ["getOrders", "client.ts:18", "fetch"],
    ["orders.get", "orders.ts:9", "node"],
    ["db.query", "orders", "postgres"],
  ];
  return (
    <div className={`${cell} bg-sunken`}>
      <CellHead
        Icon={Bug}
        title="One debugger for both sides"
        body="Set a breakpoint in a React component, then step into the API handler and the SQL it runs. One session, one call stack."
      />
      <div className="space-y-5">
      <div className="rounded-lg border border-line bg-elev font-mono text-[12px]">
        <p className="border-b border-line px-3 py-2 text-[11px] text-subtle">Paused at orders.ts:9</p>
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 px-3 py-2.5">
          <dt className="text-subtle">status</dt>
          <dd className="tk-str truncate">&quot;paid&quot;</dd>
          <dt className="text-subtle">c.req.url</dt>
          <dd className="truncate text-fg">/orders?status=paid</dd>
          <dt className="text-subtle">session</dt>
          <dd className="truncate text-fg">user_8c2e (Maren Olsen)</dd>
        </dl>
      </div>
      <ol className="relative space-y-3 font-mono text-[12px]">
        <span aria-hidden className="absolute bottom-3 left-[7px] top-3 w-px bg-line-strong" />
        {frames.map(([fn, loc, rt], i) => (
          <li key={fn} className="relative flex items-center gap-3">
            <span
              className={`relative z-10 size-[15px] rounded-full border-2 ${
                i === 2 ? "border-accent bg-accent" : "border-line-strong bg-sunken"
              }`}
            />
            <span className="flex min-w-0 flex-1 items-baseline justify-between gap-2 rounded-lg border border-line bg-elev px-3 py-2">
              <span className="truncate">
                <span className="text-fg">{fn}</span> <span className="text-subtle">{loc}</span>
              </span>
              <span className="shrink-0 text-[11px] text-subtle">{rt}</span>
            </span>
          </li>
        ))}
      </ol>
      </div>
    </div>
  );
}

function ContainersCell() {
  const services = [
    ["web", "3000", true],
    ["api", "8787", true],
    ["postgres", "5432", true],
    ["redis", "6379", false],
  ] as const;
  return (
    <div className={`${cell} bg-elev`}>
      <CellHead
        Icon={Cube}
        title="Containers in the sidebar"
        body="Start, stop and tail every service in your compose file without opening another app."
      />
      <ul className="space-y-1.5 font-mono text-[12px]">
        {services.map(([name, port, up]) => (
          <li key={name} className="flex items-center justify-between rounded-lg bg-sunken px-3 py-2">
            <span className="flex items-center gap-2 text-fg">
              <span className={`size-1.5 rounded-full ${up ? "bg-accent" : "bg-line-strong"}`} />
              {name}
            </span>
            <span className="text-subtle">{up ? `:${port}` : "stopped"}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function GitCell() {
  return (
    <div className={`${cell} bg-elev`}>
      <CellHead
        Icon={GitDiff}
        title="Git built for review"
        body="Stage single lines, resolve conflicts in a three-way view and open pull requests from the file you are in."
      />
      <div className="overflow-hidden rounded-lg border border-line font-mono text-[12px] leading-[1.8]">
        <p className="bg-sunken px-3 text-subtle">
          <span className="select-none">- </span>return c.json(rows);
        </p>
        <p className="bg-accent-soft px-3 text-fg">
          <span className="select-none text-accent">+ </span>return c.json(rows, 200, cache);
        </p>
      </div>
    </div>
  );
}

function DeployCell() {
  return (
    <div className={`${cell} dot-grid bg-bg lg:flex-row lg:items-end`}>
      <CellHead
        Icon={Globe}
        title="A preview URL for every branch"
        body="Push a branch and get a live environment with its own seeded database. Deploy to Vercel, Fly.io, AWS or your own cluster."
      />
      <div className="flex shrink-0 flex-wrap items-center gap-3 lg:flex-col lg:items-end">
        <span className="inline-flex items-center gap-2 rounded-lg border border-line-strong bg-elev px-3 py-2 font-mono text-[12px] text-fg shadow-window">
          <GitBranch className="size-3.5 text-accent" />
          feat/empty-state
        </span>
        <span className="text-subtle" aria-hidden>
          to
        </span>
        <span className="inline-flex items-center gap-2 rounded-lg border border-line-strong bg-elev px-3 py-2 font-mono text-[12px] text-fg shadow-window">
          <Globe className="size-3.5 text-accent" />
          empty-state.storefront.dev
        </span>
      </div>
    </div>
  );
}

export function BuiltIn() {
  return (
    <section id="built-in" className="mx-auto max-w-[1400px] px-4 py-24 md:px-8 md:py-32">
      <Reveal className="max-w-3xl">
        <h2 className="text-4xl font-semibold leading-[1.05] tracking-tighter text-fg md:text-5xl">
          Everything a fullstack day needs, already inside.
        </h2>
        <p className="mt-5 max-w-[60ch] text-lg leading-relaxed text-muted">
          No extension hunting. Database, HTTP, containers, debugging and deploys ship in the box and share one project context.
        </p>
      </Reveal>

      <div className="mt-14 grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-6">
        <Reveal className="md:col-span-2 lg:col-span-4">
          <DatabaseCell />
        </Reveal>
        <Reveal className="lg:col-span-2" delay={0.06}>
          <HttpCell />
        </Reveal>
        <Reveal className="lg:col-span-2 lg:row-span-2" delay={0.06}>
          <DebuggerCell />
        </Reveal>
        <Reveal className="lg:col-span-2" delay={0.12}>
          <ContainersCell />
        </Reveal>
        <Reveal className="lg:col-span-2" delay={0.18}>
          <GitCell />
        </Reveal>
        <Reveal className="md:col-span-2 lg:col-span-4" delay={0.12}>
          <DeployCell />
        </Reveal>
      </div>
    </section>
  );
}
