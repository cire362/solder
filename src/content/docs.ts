import type { Block } from "@/lib/blocks";

export type Doc = {
  slug: string;
  title: string;
  description: string;
  keywords: string;
  body: Block[];
};

export type DocGroup = { id: string; title: string; summary: string; docs: Doc[] };

export const DOC_GROUPS: DocGroup[] = [
  {
    id: "start",
    title: "Get started",
    summary: "Install Solder, open a project and move your settings over.",
    docs: [
      {
        slug: "getting-started",
        title: "Getting started",
        description: "Install Solder, open a project and run your whole stack in one window.",
        keywords: "install setup first run quickstart brew winget services",
        body: [
          { h: "Install", id: "install" },
          { p: "Download the app from the download page, or install it with your package manager. Solder updates itself in the background after that." },
          { code: ["brew install --cask solder", "solder ~/code/storefront"], file: "Terminal" },
          { h: "Open a project", id: "open-a-project" },
          { p: "Open any folder. Solder reads `package.json`, `go.mod`, `Cargo.toml`, `pyproject.toml` and `docker-compose.yml` to detect the services in your project, then indexes the code in the background." },
          { note: "Language features start working file by file while the index builds. You do not have to wait for indexing to finish before editing." },
          { h: "Run your services", id: "run-your-services" },
          { p: "Open the **Services** panel with `Cmd+Shift+S`. Every detected service gets a start button, a log tail and a port. Start them all at once with **Run stack**." },
          { list: ["Web apps from `dev` scripts in `package.json`", "APIs from `dev`, `start` or `serve` scripts and Go or Rust binaries", "Databases and queues from your compose file"] },
          { h: "Next steps", id: "next-steps" },
          { p: "Connect your database in the database client, then set a breakpoint in a component and follow the request into your API with the fullstack debugger." },
        ],
      },
      {
        slug: "importing-settings",
        title: "Importing settings",
        description: "Bring keymaps, themes and settings from VS Code, Cursor and JetBrains IDEs.",
        keywords: "vscode cursor jetbrains keymap theme migrate import vim",
        body: [
          { h: "On first launch", id: "first-launch" },
          { p: "Solder looks for existing editor configs and offers to import them. You can pick exactly what comes over: keymaps, themes, fonts, formatter settings and snippets." },
          { h: "Import later", id: "import-later" },
          { p: "Run **Import settings** from the command palette at any time. Imports are additive and never overwrite changes you made in Solder." },
          { h: "What maps across", id: "what-maps" },
          { list: ["Keymaps from VS Code, Cursor, IntelliJ, WebStorm and GoLand", "Themes in VS Code JSON format", "Vim and Neovim configs through the Neovim plugin", "Extensions with a Solder equivalent are suggested, not installed"] },
        ],
      },
    ],
  },
  {
    id: "build",
    title: "Build",
    summary: "The database client, HTTP client and debugger that ship in the box.",
    docs: [
      {
        slug: "database-client",
        title: "Database client",
        description: "Connect Postgres, MySQL, SQLite, Redis and MongoDB and query them with schema-aware completion.",
        keywords: "postgres mysql sqlite redis mongo sql query connection schema",
        body: [
          { h: "Connect", id: "connect" },
          { p: "Solder reads connection strings from `.env` files and your compose file. Detected databases appear in the **Database** panel ready to open. Add others with **New connection**." },
          { h: "Query", id: "query" },
          { p: "Open a `.sql` file or a scratch query with `Cmd+Enter` to run the statement under the cursor. Completion knows your tables, columns and indexes." },
          { code: ["SELECT customer, SUM(total_cents) / 100.0 AS revenue", "FROM orders", "WHERE status = 'paid'", "GROUP BY customer;"], file: "scratch.sql" },
          { h: "Edit rows safely", id: "edit-rows" },
          { p: "Edits in the results grid are staged, shown as a SQL diff and applied in one transaction when you confirm. Production connections are read-only unless you unlock them for the session." },
        ],
      },
      {
        slug: "fullstack-debugging",
        title: "Debugging across the stack",
        description: "One debug session that follows a request from the browser into your API and database.",
        keywords: "debugger breakpoint browser node go rust call stack trace",
        body: [
          { h: "Start a session", id: "start" },
          { p: "Press `F5` with your stack running. Solder attaches to the browser, your API processes and the database driver in a single session." },
          { h: "Follow a request", id: "follow" },
          { p: "Set a breakpoint in a React component and step over a `fetch` call. Solder matches the outgoing request to the handler that receives it and continues stepping on the server." },
          { note: "Cross-process stepping works for Node, Bun, Deno, Go and Rust servers. Python support is in preview." },
          { h: "Inspect queries", id: "queries" },
          { p: "Queries run while paused are listed in the call stack with their parameters and timing, so you can see exactly what reached the database." },
        ],
      },
    ],
  },
  {
    id: "ai",
    title: "Solder AI",
    summary: "How the AI reads your project, and how to run it on your own models.",
    docs: [
      {
        slug: "project-context",
        title: "How project context works",
        description: "What Solder AI sees, what it sends and how to control both.",
        keywords: "ai context privacy index embeddings ignore files",
        body: [
          { h: "What the AI sees", id: "sees" },
          { p: "Solder builds a local map of your project: components, routes, schemas, migrations and recent logs. For each request it selects only the pieces relevant to the question." },
          { h: "Controlling context", id: "controlling" },
          { p: "Add paths to `.solderignore` to keep them out of AI context entirely. Secrets in `.env` files are always excluded." },
          { code: ["# .solderignore", "infra/terraform/state/", "*.pem", "fixtures/large/"], file: ".solderignore" },
          { h: "Retention", id: "retention" },
          { p: "Hosted requests are processed and discarded. Nothing is stored after the response and nothing is used for training." },
        ],
      },
      {
        slug: "local-models",
        title: "Running local models",
        description: "Use Ollama, LM Studio or any OpenAI-compatible server so code never leaves your machine.",
        keywords: "ollama lm studio local offline model byok api key",
        body: [
          { h: "Add a provider", id: "provider" },
          { p: "Open **Settings, AI, Providers** and choose Ollama, LM Studio or a custom endpoint. Solder lists the models the server exposes." },
          { h: "Pick models per task", id: "per-task" },
          { p: "Chat, inline edits and agent tasks can each use a different model. A small local model for completions and a larger one for agent tasks is a good default." },
          { code: ["{", '  "ai.completions": "ollama:qwen2.5-coder:7b",', '  "ai.agent": "ollama:qwen2.5-coder:32b"', "}"], file: "settings.json" },
        ],
      },
      {
        slug: "context-servers",
        title: "Context servers",
        description: "Give the agent tools of its own with Model Context Protocol servers: a database to query, an issue tracker to read.",
        keywords: "mcp model context protocol context server tools agent postgres github",
        body: [
          { h: "Add a server", id: "add" },
          { p: "A context server is a Model Context Protocol (MCP) server: a program Solder starts and that gives the agent tools of its own. Name it in `settings.json` with the command that starts it, in the form Zed uses." },
          { code: ["{", '  "context_servers": {', '    "postgres": {', '      "command": "npx",', '      "args": ["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/app"]', "    }", "  }", "}"], file: "settings.json" },
          { h: "When it runs", id: "runs" },
          { p: "Nothing starts with the editor. Servers start when an agent task begins and stay for the next one. The **context servers** view of the AI tab lists them with their tools, and says why one did not start." },
          { h: "What asks first", id: "asks-first" },
          { p: "A server's tool runs where the server does, not in the agent's sandbox. The first call of each tool in a task waits for you and shows the tool, the server and the arguments. After you allow it, that task may call the tool again." },
        ],
      },
    ],
  },
  {
    id: "extend",
    title: "Extend",
    summary: "Write plugins in TypeScript, Rust or Go against a sandboxed API, and install extensions made for Zed and VS Code.",
    docs: [
      {
        slug: "plugin-api",
        title: "Plugin API",
        description: "Build a plugin, request permissions and publish it to the registry.",
        keywords: "plugin extension api wasm sandbox permissions publish typescript rust go",
        body: [
          { h: "Create a plugin", id: "create" },
          { p: "Scaffold a plugin with the CLI. It opens in a second Solder window with hot reload." },
          { code: ["solder plugin new request-timer --lang ts", "cd request-timer && solder plugin dev"], file: "Terminal" },
          { h: "Permissions", id: "permissions" },
          { p: "Plugins run in a WebAssembly sandbox and can only use the capabilities they declare. Users see the list before installing." },
          { code: ["export default defineExtension({", '  name: "request-timer",', '  permissions: ["http:read", "statusBar"],', "  activate(ctx) {", "    ctx.http.onResponse((res) => ctx.statusBar.show(`${res.durationMs} ms`));", "  },", "});"], file: "extension.ts" },
          { h: "Publish", id: "publish" },
          { p: "Run `solder plugin publish`. The registry builds your plugin from source, signs it and makes it available within a few minutes." },
        ],
      },
      {
        slug: "extensions",
        title: "Extensions from Zed and VS Code",
        description: "Install extensions made for Zed and VS Code from their public catalogs, and see what each one brings.",
        keywords: "extensions zed vscode open vsx catalog language server grammar themes snippets install update",
        body: [
          { h: "Install an extension", id: "install" },
          { p: "Open the **Extensions** tab of the sidebar. It lists what is installed, then the answers of Zed's catalog and Open VSX in one list. Type to search both, and press **Install** on a row." },
          { note: "Solder has no extension service of its own. The two catalogs are asked when the tab is opened and when you search, never at startup." },
          { h: "What an extension brings", id: "what-works" },
          { p: "A Zed extension brings its languages with highlighting and with the way they are typed (indentation, brackets, pairs that close themselves, comments), its snippets, its themes, its icon themes, its language servers and its debug adapters. A VS Code extension brings its themes and snippets. Its code is written for VS Code and does not run in Solder, so for a language the tab points to the Zed extension instead." },
          { p: "Select a row to see both lists for that extension: what Solder uses, and what does not run here." },
          { h: "What asks first", id: "asks-first" },
          { p: "An extension's own code runs in a sandbox: it can compute, and it can write to one folder of its own. A language server is different. It is a program the extension downloads and Solder starts with your rights." },
          { p: "So an extension that brings a server, or declares commands it runs, is downloaded and then waits. The tab shows what installing it allows, and nothing is put in place until you press **Install** there. A later version asks again only if it wants something new." },
          { p: "What you allowed can be taken back for one extension: its details have a **Refuse** button for the commands it runs, for npm, for downloads, and for each host it downloaded from. Below them the tab lists what its code did since Solder started, and what it asked for and did not get." },
          { h: "Updates, and turning one off", id: "manage" },
          { list: [
            "Newer versions are looked for when the tab is opened. **Update** on a row installs one, and the button next to the search installs all of them.",
            "**Keep this version** stops updates for one extension.",
            "**Turn off** keeps an extension installed without using its languages, snippets or servers.",
            "**Remove** deletes it, with the servers it downloaded.",
          ] },
        ],
      },
    ],
  },
];

export const ALL_DOCS = DOC_GROUPS.flatMap((g) => g.docs.map((d) => ({ ...d, group: g.title })));

export function getDoc(slug: string) {
  const i = ALL_DOCS.findIndex((d) => d.slug === slug);
  if (i === -1) return null;
  return { doc: ALL_DOCS[i], prev: ALL_DOCS[i - 1] ?? null, next: ALL_DOCS[i + 1] ?? null };
}
