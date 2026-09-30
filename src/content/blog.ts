import type { Block } from "@/lib/blocks";

export type Author = { name: string; role: string };
export type Post = {
  slug: string;
  title: string;
  excerpt: string;
  date: string;
  iso: string;
  tag: "Engineering" | "AI" | "Product";
  minutes: number;
  author: Author;
  /** picsum seed for placeholder cover photography */
  cover: string;
  body: Block[];
};

const ILSE = { name: "Ilse Vandenberg", role: "Co-founder, CEO" };
const KOFI = { name: "Kofi Mensah", role: "Editor core" };
const NOOR = { name: "Noor Haddad", role: "Solder AI" };
const RAFAEL = { name: "Rafael Duarte", role: "Plugins and platform" };

// mock: placeholder posts for the blog template.
export const POSTS: Post[] = [
  {
    slug: "agent-tasks-you-can-review",
    title: "Agent tasks you can actually review",
    excerpt: "Why the agent in Solder 1.8 works in its own branch, shows its plan up front and never commits for you.",
    date: "September 16, 2026",
    iso: "2026-09-16",
    tag: "Product",
    minutes: 6,
    author: ILSE,
    cover: "solder-blog-desk-review",
    body: [
      { p: "Most coding agents we tried had the same problem. They were fast, and then we spent twenty minutes reading a diff we did not ask for. Solder 1.8 changes where the agent works and when it stops." },
      { h: "A branch, not your working tree", id: "branch" },
      { p: "Every agent task starts in a sandboxed branch with its own copy of your services. You can keep editing while it runs, and nothing lands in your files until you merge it." },
      { h: "The plan comes first", id: "plan" },
      { p: "Before editing anything, the agent writes a short plan: which files, which tests, what it will not touch. You can edit the plan, and the agent follows your version." },
      { h: "It stops for review", id: "review" },
      { p: "When tests pass, the task stops with a diff grouped by layer: migration, API, client. Nothing is committed. Review it like a pull request from a colleague, because that is what it is." },
    ],
  },
  {
    slug: "why-rust",
    title: "Why we wrote the core in Rust",
    excerpt: "Input latency, memory on large monorepos and a plugin sandbox that cannot block typing. The trade-offs behind a native editor.",
    date: "August 28, 2026",
    iso: "2026-08-28",
    tag: "Engineering",
    minutes: 9,
    author: KOFI,
    cover: "solder-blog-circuit-board",
    body: [
      { p: "We started Solder as an Electron prototype. It worked, and it felt like every other editor we had used for a decade. We rewrote the core before the first public preview." },
      { h: "Latency is a feature", id: "latency" },
      { p: "Typing should feel like writing on paper. That means the keystroke-to-pixel path cannot wait on garbage collection, layout or a message queue between processes." },
      { h: "Memory on real projects", id: "memory" },
      { p: "Fullstack repositories are big: two or three apps, shared packages, generated clients, migrations. We keep the syntax trees and index in compact native structures and page them out when a folder goes cold." },
      { h: "What we gave up", id: "gave-up" },
      { p: "Building UI natively is slower for us than HTML would be. We accepted that in exchange for an editor that stays fast when ten services stream logs at once." },
    ],
  },
  {
    slug: "context-for-ai-pairs",
    title: "What an AI pair needs to know about your stack",
    excerpt: "Good answers come from the right context, not more context. How Solder picks the component, route and schema that matter.",
    date: "July 30, 2026",
    iso: "2026-07-30",
    tag: "AI",
    minutes: 7,
    author: NOOR,
    cover: "solder-blog-notebook-sketch",
    body: [
      { p: "Ask a model why a page shows an empty list and it will guess. Give it the component, the API route, the migration and the last few log lines and it will usually find the bug." },
      { h: "A map, not a dump", id: "map" },
      { p: "Solder keeps a local map of how your code connects: which component calls which endpoint, which handler runs which query, which table the query touches." },
      { h: "Selecting context", id: "selecting" },
      { p: "For each request we walk that map from the file you are in and include only what is on the path. Most answers need between four and eight files." },
      { note: "Paths in `.solderignore` and all `.env` files are excluded before selection runs." },
    ],
  },
  {
    slug: "one-call-stack",
    title: "One call stack from React to Postgres",
    excerpt: "How the fullstack debugger matches a fetch in the browser to the handler that receives it, across processes and runtimes.",
    date: "June 24, 2026",
    iso: "2026-06-24",
    tag: "Engineering",
    minutes: 11,
    author: KOFI,
    cover: "solder-blog-cables-rack",
    body: [
      { p: "Debugging a fullstack bug usually means two debuggers, two windows and a lot of console logs. We wanted one session." },
      { h: "Tagging requests", id: "tagging" },
      { p: "While a debug session runs, Solder adds a trace header to outgoing requests from the browser. The server adapter reads it and pauses the matching handler when you step into the call." },
      { h: "Runtimes", id: "runtimes" },
      { p: "Adapters exist for Node, Bun, Deno, Go and Rust. Python is in preview. Each adapter is small, because the hard part lives in the editor, not in the runtime." },
    ],
  },
  {
    slug: "plugin-sandbox",
    title: "Designing a plugin sandbox that cannot block typing",
    excerpt: "WebAssembly isolation, declared permissions and a strict time budget per frame. Lessons from building Plugin API 1.0.",
    date: "May 27, 2026",
    iso: "2026-05-27",
    tag: "Engineering",
    minutes: 8,
    author: RAFAEL,
    cover: "solder-blog-workshop-tools",
    body: [
      { p: "Plugins are why editors feel personal, and also why they get slow. Plugin API 1.0 is built so that a misbehaving plugin can fail without taking the editor with it." },
      { h: "Isolation", id: "isolation" },
      { p: "Each plugin runs in its own WebAssembly instance on a worker thread. It talks to the editor through a typed message API, never shared memory." },
      { h: "Permissions", id: "permissions" },
      { p: "Plugins declare what they need, like `fs:read` for a folder or `http:read`. Users see the list before installing, and the sandbox enforces it." },
      { h: "Budgets", id: "budgets" },
      { p: "Work triggered by typing gets a few milliseconds per frame. Plugins that go over are deferred to idle time, and repeat offenders are flagged in the plugin panel." },
    ],
  },
];

export const TAGS = ["All", "Engineering", "AI", "Product"] as const;

export function getPost(slug: string) {
  return POSTS.find((p) => p.slug === slug) ?? null;
}

export function coverUrl(seed: string, w: number, h: number) {
  return `https://picsum.photos/seed/${seed}/${w}/${h}?grayscale`;
}
