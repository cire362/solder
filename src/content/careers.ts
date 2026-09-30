// mock: placeholder openings for the careers template.
export type Role = { slug: string; title: string; team: Team; location: string; summary: string };
export type Team = "Engineering" | "AI" | "Design" | "Developer relations" | "Operations";

export const TEAMS: ("All" | Team)[] = ["All", "Engineering", "AI", "Design", "Developer relations", "Operations"];

export const ROLES: Role[] = [
  {
    slug: "senior-rust-engineer-editor-core",
    title: "Senior Rust engineer, editor core",
    team: "Engineering",
    location: "Remote, Europe",
    summary: "Text layout, rendering and the incremental index that keeps large monorepos fast.",
  },
  {
    slug: "debugger-engineer",
    title: "Debugger engineer, Node and Go runtimes",
    team: "Engineering",
    location: "Remote, Europe or Americas",
    summary: "Cross-process stepping and the runtime adapters behind the fullstack debugger.",
  },
  {
    slug: "applied-ai-engineer-context",
    title: "Applied AI engineer, context and retrieval",
    team: "AI",
    location: "Remote, Europe",
    summary: "Decide which code, schema and logs a model sees, and measure whether the answers get better.",
  },
  {
    slug: "ml-engineer-local-models",
    title: "ML engineer, local models",
    team: "AI",
    location: "Remote",
    summary: "Make small local models useful for completions and edits on a laptop GPU.",
  },
  {
    slug: "product-designer-editor",
    title: "Product designer, editor UI",
    team: "Design",
    location: "Remote, Europe",
    summary: "Design dense, keyboard-first interfaces for panels that developers stare at all day.",
  },
  {
    slug: "developer-advocate",
    title: "Developer advocate",
    team: "Developer relations",
    location: "Remote, Americas",
    summary: "Write guides, record demos and bring what developers tell you back to the team.",
  },
];

export function getRole(slug: string | null) {
  return ROLES.find((r) => r.slug === slug) ?? null;
}
