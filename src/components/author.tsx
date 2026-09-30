import type { Author as AuthorType } from "@/content/blog";

// Initials monogram instead of a stock avatar.
export function Author({ author, size = "sm" }: { author: AuthorType; size?: "sm" | "lg" }) {
  const initials = author.name
    .split(" ")
    .map((n) => n[0])
    .join("");
  return (
    <span className="flex items-center gap-3">
      <span
        aria-hidden
        className={`grid shrink-0 place-items-center rounded-full border border-line-strong bg-accent-soft font-mono font-medium text-accent ${
          size === "lg" ? "size-11 text-[14px]" : "size-8 text-[11px]"
        }`}
      >
        {initials}
      </span>
      <span className="leading-tight">
        <span className={`block font-medium text-fg ${size === "lg" ? "text-[15px]" : "text-[14px]"}`}>{author.name}</span>
        <span className="block text-[13px] text-subtle">{author.role}</span>
      </span>
    </span>
  );
}
