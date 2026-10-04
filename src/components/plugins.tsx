"use client";

import { useId, useMemo, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { Check, Keyboard, MagnifyingGlass, Palette, Plus, PuzzlePiece } from "@phosphor-icons/react";
import {
  siApachekafka,
  siAstro,
  siDrizzle,
  siElixir,
  siGraphql,
  siJetbrains,
  siNeovim,
  siOpenapiinitiative,
  siPrisma,
  siStorybook,
  siStripe,
  siTerraform,
  type SimpleIcon,
} from "simple-icons";
import { Code } from "@/lib/highlight";
import { Reveal } from "./reveal";
import { PluginGarden } from "./flora-scenes";

type Category = "Languages" | "Frameworks" | "Tools" | "Themes" | "Keymaps";

type Plugin = {
  name: string;
  author: string;
  desc: string;
  cat: Category;
  icon: SimpleIcon | typeof Palette;
};

const PLUGINS: Plugin[] = [
  { name: "Prisma Schema", author: "solder", desc: "Formatting, go-to-model and live migration diffs.", cat: "Tools", icon: siPrisma },
  { name: "Drizzle Studio", author: "drizzle-team", desc: "Browse and edit Drizzle tables in the database panel.", cat: "Tools", icon: siDrizzle },
  { name: "GraphQL Explorer", author: "solder", desc: "Schema-aware queries with results beside the resolver.", cat: "Languages", icon: siGraphql },
  { name: "OpenAPI Sync", author: "yusuf.dev", desc: "Keeps your spec and route handlers in lockstep.", cat: "Tools", icon: siOpenapiinitiative },
  { name: "Astro", author: "solder", desc: "Islands, content collections and .astro language support.", cat: "Frameworks", icon: siAstro },
  { name: "Storybook Preview", author: "priyanka-r", desc: "Render stories in a split pane as you edit components.", cat: "Frameworks", icon: siStorybook },
  { name: "Elixir LS", author: "beam-tools", desc: "Completions, Dialyzer hints and Mix tasks.", cat: "Languages", icon: siElixir },
  { name: "Terraform", author: "solder", desc: "Plan output inline and resource graph navigation.", cat: "Tools", icon: siTerraform },
  { name: "Kafka Inspector", author: "streamwise", desc: "Tail topics and replay messages to local consumers.", cat: "Tools", icon: siApachekafka },
  { name: "Stripe Webhooks", author: "ines.m", desc: "Forward test events to your local API with one click.", cat: "Tools", icon: siStripe },
  { name: "Ember Dark", author: "solder", desc: "The default warm dark theme, tuned for long sessions.", cat: "Themes", icon: Palette },
  { name: "Neovim Mode", author: "solder", desc: "Real Neovim under the hood, with your init.lua.", cat: "Keymaps", icon: siNeovim },
  { name: "JetBrains Keymap", author: "solder", desc: "Every shortcut you already have in muscle memory.", cat: "Keymaps", icon: siJetbrains },
  { name: "VS Code Keymap", author: "solder", desc: "Familiar bindings, imported with your settings.", cat: "Keymaps", icon: Keyboard },
];

const CATS: ("All" | Category)[] = ["All", "Languages", "Frameworks", "Tools", "Themes", "Keymaps"];

const API_SNIPPET = [
  `import { defineExtension } from "@solder/api";`,
  ``,
  `export default defineExtension({`,
  `  name: "request-timer",`,
  `  permissions: ["http:read"],`,
  `  activate(ctx) {`,
  `    ctx.http.onResponse((res) => {`,
  `      ctx.statusBar.show(\`\${res.route} \${res.durationMs} ms\`);`,
  `    });`,
  `  },`,
  `});`,
];

const POINTS = [
  "Sandboxed in WebAssembly. A plugin cannot block typing or read files you did not grant.",
  "Write plugins in TypeScript, Rust or Go, with hot reload while you develop them.",
  "Your VS Code settings, keymaps and themes import on first launch.",
];

function PluginIcon({ icon }: { icon: Plugin["icon"] }) {
  if ("path" in icon) {
    return (
      <svg viewBox="0 0 24 24" className="size-[18px] fill-current" aria-hidden>
        <path d={icon.path} />
      </svg>
    );
  }
  const Icon = icon;
  return <Icon className="size-[18px]" />;
}

export function Plugins() {
  const inputId = useId();
  const [query, setQuery] = useState("");
  const [cat, setCat] = useState<(typeof CATS)[number]>("All");
  const [installed, setInstalled] = useState<Set<string>>(() => new Set(["Ember Dark"]));

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    return PLUGINS.filter(
      (p) =>
        (cat === "All" || p.cat === cat) &&
        (!q || p.name.toLowerCase().includes(q) || p.desc.toLowerCase().includes(q)),
    );
  }, [query, cat]);

  function toggle(name: string) {
    setInstalled((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  }

  return (
    <section id="plugins" className="mx-auto max-w-[1400px] px-4 py-24 md:px-8 md:py-32">
      <div className="grid gap-14 lg:grid-cols-[minmax(0,6fr)_minmax(0,6fr)] lg:gap-16">
        <div>
          <Reveal>
            <h2 className="text-4xl font-semibold leading-[1.05] tracking-tighter text-fg md:text-5xl">
              Plugins that can’t slow you down.
            </h2>
            <p className="mt-5 max-w-[52ch] text-lg leading-relaxed text-muted">
              Add a language, a framework or a whole workflow. Every plugin runs isolated from the editor core.
            </p>
          </Reveal>

          <Reveal delay={0.08}>
            <ul className="mt-10 space-y-4">
              {POINTS.map((p) => (
                <li key={p} className="flex gap-3 text-[15px] leading-relaxed text-muted">
                  <Check weight="bold" className="mt-1 size-4 shrink-0 text-accent" />
                  {p}
                </li>
              ))}
            </ul>
          </Reveal>

          <Reveal delay={0.14}>
            <div className="mt-10 overflow-hidden rounded-2xl border border-line bg-elev">
              <p className="flex items-center gap-2 border-b border-line px-4 py-2.5 font-mono text-[11.5px] text-subtle">
                <PuzzlePiece className="size-3.5 text-accent" />
                extension.ts
              </p>
              <pre className="overflow-x-auto px-4 py-4 font-mono text-[12.5px] leading-[1.75]">
                {API_SNIPPET.map((l, i) => (
                  <div key={i}>
                    <Code line={l} />
                  </div>
                ))}
              </pre>
            </div>
          </Reveal>
        </div>

        <Reveal delay={0.1} className="lg:pt-2">
          <div className="rounded-2xl border border-line-strong bg-elev p-4 shadow-window md:p-6">
            <div className="-mx-1 -mt-2 mb-2">
              <PluginGarden count={installed.size} />
            </div>
            <div className="mb-2 flex items-baseline justify-between gap-4">
              <label htmlFor={inputId} className="block text-sm font-medium text-fg">
                Search plugins
              </label>
              <span className="text-[13px] text-subtle" aria-live="polite">
                {installed.size} installed
              </span>
            </div>
            <div className="relative">
              <MagnifyingGlass className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-subtle" />
              <input
                id={inputId}
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="prisma, graphql, vim…"
                className="h-11 w-full rounded-lg border border-line-strong bg-bg pl-9 pr-3 text-[15px] text-fg outline-none placeholder:text-subtle focus:border-accent focus:ring-2 focus:ring-accent/25"
              />
            </div>

            <div className="mt-4 flex gap-2 overflow-x-auto pb-1" role="group" aria-label="Filter by category">
              {CATS.map((c) => (
                <button
                  key={c}
                  type="button"
                  aria-pressed={cat === c}
                  onClick={() => setCat(c)}
                  className={`h-8 shrink-0 rounded-lg px-3 text-[13px] transition-colors ${
                    cat === c ? "bg-fg text-bg" : "bg-sunken text-muted hover:text-fg"
                  }`}
                >
                  {c}
                </button>
              ))}
            </div>

            <div className="mt-4 h-[468px] overflow-y-auto pr-1">
              {results.length === 0 ? (
                <div className="grid h-full place-items-center px-6 text-center">
                  <div>
                    <PuzzlePiece className="mx-auto size-8 text-subtle" />
                    <p className="mt-4 font-medium text-fg">Nothing matches “{query}”</p>
                    <p className="mx-auto mt-1.5 max-w-[34ch] text-sm leading-relaxed text-muted">
                      Try a framework or database name, or build it yourself with the plugin API.
                    </p>
                    <button
                      type="button"
                      onClick={() => {
                        setQuery("");
                        setCat("All");
                      }}
                      className="mt-5 h-9 rounded-lg border border-line-strong px-3.5 text-[13px] text-fg hover:bg-sunken"
                    >
                      Clear search
                    </button>
                  </div>
                </div>
              ) : (
                <ul className="space-y-1">
                  <AnimatePresence initial={false}>
                    {results.map((p) => {
                      const on = installed.has(p.name);
                      return (
                        <motion.li
                          key={p.name}
                          layout
                          initial={{ opacity: 0 }}
                          animate={{ opacity: 1 }}
                          exit={{ opacity: 0 }}
                          transition={{ duration: 0.2 }}
                          className="flex items-center gap-3 rounded-lg px-2 py-2.5 hover:bg-sunken"
                        >
                          <span className="grid size-10 shrink-0 place-items-center rounded-lg border border-line bg-bg text-fg">
                            <PluginIcon icon={p.icon} />
                          </span>
                          <span className="min-w-0 flex-1">
                            <span className="flex items-baseline gap-2">
                              <span className="truncate text-[14.5px] font-medium text-fg">{p.name}</span>
                              <span className="truncate font-mono text-[11.5px] text-subtle">{p.author}</span>
                            </span>
                            <span className="block truncate text-[13.5px] text-muted">{p.desc}</span>
                          </span>
                          <button
                            type="button"
                            onClick={() => toggle(p.name)}
                            aria-label={`${on ? "Uninstall" : "Install"} ${p.name}`}
                            className={`inline-flex h-8 shrink-0 items-center gap-1.5 rounded-lg px-3 text-[12.5px] font-medium transition active:scale-[0.97] ${
                              on
                                ? "text-muted"
                                : "border border-line-strong text-fg hover:border-accent hover:text-accent"
                            }`}
                          >
                            {on ? <Check weight="bold" className="size-3.5 text-accent" /> : <Plus weight="bold" className="size-3.5" />}
                            {on ? "Installed" : "Install"}
                          </button>
                        </motion.li>
                      );
                    })}
                  </AnimatePresence>
                </ul>
              )}
            </div>
          </div>
        </Reveal>
      </div>
    </section>
  );
}
