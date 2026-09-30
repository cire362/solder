// mock: placeholder release history.
export type Release = {
  version: string;
  date: string;
  iso: string;
  title: string;
  summary: string;
  new?: string[];
  improved?: string[];
  fixed?: string[];
};

export const RELEASES: Release[] = [
  {
    version: "1.8",
    date: "September 16, 2026",
    iso: "2026-09-16",
    title: "Agent tasks in sandboxed branches",
    summary:
      "Hand the agent a task and it works in its own branch, runs your tests and stops for review. Nothing touches your working tree until you merge.",
    new: [
      "Agent tasks with a live plan you can edit while it runs",
      "AI review on push, with suggested fixes you can apply inline",
      "`solder agent` CLI for running tasks from the terminal",
    ],
    improved: [
      "Project context now includes recent service logs",
      "Monorepo indexing uses about a third less memory",
    ],
    fixed: ["Breakpoints in Bun workers were skipped after hot reload", "Tab order in the database grid on Windows"],
  },
  {
    version: "1.7",
    date: "August 19, 2026",
    iso: "2026-08-19",
    title: "Preview environments with seeded databases",
    summary:
      "Every pushed branch can get a live URL and its own copy of your development database, created from your seed scripts.",
    new: ["Preview environments on Vercel, Fly.io, AWS and Kubernetes", "Share a preview link from the status bar"],
    improved: ["HTTP client imports OpenAPI 3.1 specs", "Faster cold start on Linux with Wayland"],
    fixed: ["Compose services with profiles did not appear in the sidebar"],
  },
  {
    version: "1.6",
    date: "July 22, 2026",
    iso: "2026-07-22",
    title: "Local models, first class",
    summary: "Run Solder AI on Ollama, LM Studio or any OpenAI-compatible server, with a different model per task.",
    new: ["Per-task model selection for chat, edits and agents", "Offline mode that blocks all hosted requests"],
    improved: ["Inline completions stream token by token"],
    fixed: ["Rare crash when closing a split while a query was running"],
  },
  {
    version: "1.5",
    date: "June 24, 2026",
    iso: "2026-06-24",
    title: "One debug session across browser and server",
    summary:
      "Step from a React component into the API handler that serves its request, then see the queries it ran, in a single call stack.",
    new: ["Cross-process stepping for Node, Bun, Deno, Go and Rust", "Query timeline in the debugger"],
    improved: ["Source maps resolve for Vite and Turbopack builds without config"],
  },
  {
    version: "1.4",
    date: "May 27, 2026",
    iso: "2026-05-27",
    title: "Plugin API 1.0",
    summary: "The plugin API is stable. Plugins run in a WebAssembly sandbox and declare the permissions they need.",
    new: ["Plugins in TypeScript, Rust and Go", "Plugin registry with signed builds"],
    fixed: ["Theme colors in the minimap ignored high contrast mode"],
  },
];
