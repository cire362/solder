"use client";

import { useState } from "react";
import Link from "next/link";
import { AnimatePresence, motion } from "motion/react";
import { Check } from "@phosphor-icons/react";

// mock: placeholder pricing. Replace with real plans before launch.
const PLANS = [
  {
    id: "hobby",
    name: "Hobby",
    tagline: "For personal projects and learning.",
    monthly: 0,
    yearly: 0,
    unit: "forever",
    cta: { label: "Download", href: "/download" },
    features: [
      "The full editor, debugger and Git tools",
      "Database, HTTP and container panels",
      "50 hosted AI requests per day",
      "Unlimited AI with your own key or local models",
      "Every community plugin",
    ],
  },
  {
    id: "pro",
    name: "Pro",
    tagline: "For developers who ship every day.",
    monthly: 12,
    yearly: 10,
    unit: "per month",
    cta: { label: "Start free trial", href: "/download" },
    features: [
      "Everything in Hobby",
      "Unlimited hosted AI, including agent tasks",
      "AI review on every push",
      "10 active preview environments",
      "Settings sync across machines",
      "Email support",
    ],
  },
  {
    id: "team",
    name: "Team",
    tagline: "For teams that share a codebase.",
    monthly: 24,
    yearly: 20,
    unit: "per seat, per month",
    cta: { label: "Contact sales", href: "/contact?topic=sales" },
    features: [
      "Everything in Pro",
      "Shared project context and prompts",
      "Unlimited preview environments",
      "SAML SSO and SCIM provisioning",
      "Usage controls and audit log",
      "Priority support with a named engineer",
    ],
  },
] as const;

export function PricingPlans() {
  const [yearly, setYearly] = useState(true);

  return (
    <section className="mx-auto max-w-[1400px] px-4 md:px-8">
      <div
        role="radiogroup"
        aria-label="Billing period"
        className="relative inline-flex rounded-lg border border-line-strong bg-elev p-1"
      >
        {[
          [false, "Monthly"],
          [true, "Yearly, save 2 months"],
        ].map(([value, label]) => {
          const active = yearly === value;
          return (
            <button
              key={String(value)}
              type="button"
              role="radio"
              aria-checked={active}
              onClick={() => setYearly(value as boolean)}
              className={`relative h-9 rounded-md px-4 text-sm transition-colors ${active ? "text-bg" : "text-muted hover:text-fg"}`}
            >
              {active && (
                <motion.span
                  layoutId="billing-pill"
                  transition={{ type: "spring", stiffness: 420, damping: 34 }}
                  className="absolute inset-0 rounded-md bg-fg"
                />
              )}
              <span className="relative">{label as string}</span>
            </button>
          );
        })}
      </div>

      <div className="mt-10 grid items-stretch gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.15fr)_minmax(0,1fr)]">
        {PLANS.map((plan) => {
          const featured = plan.id === "pro";
          const price = yearly ? plan.yearly : plan.monthly;
          return (
            <article
              key={plan.id}
              className={`relative flex flex-col rounded-2xl border p-6 md:p-8 ${
                featured
                  ? "border-accent bg-elev shadow-window lg:-my-4 lg:py-12"
                  : "border-line bg-bg"
              }`}
            >
              <div className="flex items-baseline justify-between gap-3">
                <h2 className="text-xl font-semibold tracking-tight text-fg">{plan.name}</h2>
                {featured && <span className="text-[13px] font-medium text-accent">Recommended</span>}
              </div>
              <p className="mt-1.5 text-[15px] text-muted">{plan.tagline}</p>

              <div className="mt-8 flex h-14 items-end gap-2">
                <span className="text-lg text-muted">$</span>
                <AnimatePresence mode="popLayout" initial={false}>
                  <motion.span
                    key={price}
                    initial={{ opacity: 0, y: 12 }}
                    animate={{ opacity: 1, y: 0 }}
                    exit={{ opacity: 0, y: -12 }}
                    transition={{ duration: 0.25 }}
                    className="font-mono text-5xl font-medium leading-none tracking-tight text-fg"
                  >
                    {price}
                  </motion.span>
                </AnimatePresence>
                <span className="pb-1 text-sm text-subtle">{plan.unit}</span>
              </div>
              <p className="mt-2 h-5 text-[13px] text-subtle">
                {plan.monthly > 0 && yearly ? `Billed $${plan.yearly * 12} yearly` : plan.monthly > 0 ? "Billed monthly" : ""}
              </p>

              <Link
                href={plan.cta.href}
                className={`mt-8 inline-flex h-11 items-center justify-center rounded-lg text-[15px] font-medium transition active:scale-[0.98] ${
                  featured
                    ? "bg-accent text-accent-fg hover:brightness-110"
                    : "border border-line-strong text-fg hover:bg-elev"
                }`}
              >
                {plan.cta.label}
              </Link>

              <ul className="mt-8 space-y-3 border-t border-line pt-8">
                {plan.features.map((f) => (
                  <li key={f} className="flex gap-3 text-[15px] leading-snug text-muted">
                    <Check weight="bold" className="mt-0.5 size-4 shrink-0 text-accent" />
                    {f}
                  </li>
                ))}
              </ul>
            </article>
          );
        })}
      </div>
    </section>
  );
}
