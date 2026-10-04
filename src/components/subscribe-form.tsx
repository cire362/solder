"use client";

import { useId, useState } from "react";
import { Check, CircleNotch } from "@phosphor-icons/react";

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

// Demo form: not connected to a backend yet. Replace the timeout with a POST to your mailing-list API.
export function SubscribeForm({ label = "Get release notes by email" }: { label?: string }) {
  const id = useId();
  const [email, setEmail] = useState("");
  const [status, setStatus] = useState<"idle" | "error" | "loading" | "done">("idle");

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!EMAIL.test(email.trim())) {
      setStatus("error");
      return;
    }
    setStatus("loading");
    await new Promise((r) => setTimeout(r, 900));
    setStatus("done");
  }

  if (status === "done") {
    return (
      <div role="status" className="flex items-start gap-3 rounded-2xl border border-line bg-elev p-5">
        <span className="grid size-7 shrink-0 place-items-center rounded-full bg-accent text-accent-fg">
          <Check weight="bold" className="size-4" />
        </span>
        <div>
          <p className="font-medium text-fg">You are subscribed</p>
          <p className="mt-0.5 text-[14px] text-muted">
            Release notes will go to {email.trim()}. There is an unsubscribe link in every email.
          </p>
        </div>
      </div>
    );
  }

  return (
    <form onSubmit={submit} noValidate className="max-w-md">
      <label htmlFor={id} className="block text-sm font-medium text-fg">
        {label}
      </label>
      <div className="mt-2 flex gap-2">
        <input
          id={id}
          type="email"
          autoComplete="email"
          value={email}
          onChange={(e) => {
            setEmail(e.target.value);
            if (status === "error") setStatus("idle");
          }}
          aria-invalid={status === "error"}
          aria-describedby={`${id}-help`}
          placeholder="you@company.com"
          className={`h-11 min-w-0 flex-1 rounded-lg border bg-elev px-3.5 text-[15px] text-fg outline-none placeholder:text-subtle focus:ring-2 ${
            status === "error" ? "border-accent focus:ring-accent/25" : "border-line-strong focus:border-accent focus:ring-accent/25"
          }`}
        />
        <button
          type="submit"
          disabled={status === "loading"}
          className="inline-flex h-11 shrink-0 items-center gap-2 rounded-lg bg-fg px-4 text-[15px] font-medium text-bg transition active:scale-[0.98] disabled:opacity-70"
        >
          {status === "loading" && <CircleNotch className="size-4 animate-spin motion-reduce:animate-none" />}
          {status === "loading" ? "Subscribing" : "Subscribe"}
        </button>
      </div>
      <p id={`${id}-help`} className={`mt-2 text-[13px] ${status === "error" ? "text-accent" : "text-muted"}`}>
        {status === "error" ? "Enter a valid email address, like you@company.com." : "About one email a month. No marketing."}
      </p>
    </form>
  );
}
