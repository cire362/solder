"use client";

import { useId, useState } from "react";
import { CaretDown, Check, CircleNotch } from "@phosphor-icons/react";
import { getRole } from "@/content/careers";

const TOPICS = [
  ["sales", "Sales and team plans"],
  ["support", "Product support"],
  ["partnerships", "Partnerships"],
  ["press", "Press"],
  ["careers", "Careers"],
] as const;

type Topic = (typeof TOPICS)[number][0];
type Field = "name" | "email" | "topic" | "message";
type Values = Record<Field | "company", string>;

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

function validate(v: Values): Partial<Record<Field, string>> {
  const e: Partial<Record<Field, string>> = {};
  if (!v.name.trim()) e.name = "Enter your name.";
  if (!EMAIL.test(v.email.trim())) e.email = "Enter a valid email address, like you@company.com.";
  if (!v.topic) e.topic = "Choose what this is about.";
  if (v.message.trim().length < 20) e.message = "Tell us a little more, at least 20 characters.";
  return e;
}

const input =
  "w-full rounded-lg border bg-elev px-3.5 text-[15px] text-fg outline-none placeholder:text-subtle focus:ring-2 focus:ring-accent/25";

// Demo form: not connected to a backend yet. Replace the timeout with a POST to an API route.
export function ContactForm({ topicParam, roleParam }: { topicParam?: string; roleParam?: string }) {
  const id = useId();
  const role = getRole(roleParam ?? null);
  const initialTopic = TOPICS.some(([t]) => t === topicParam) ? (topicParam as Topic) : "";

  const [values, setValues] = useState<Values>({
    name: "",
    email: "",
    company: "",
    topic: initialTopic,
    message: role ? `I would like to apply for the ${role.title} role.\n\n` : "",
  });
  const [submitted, setSubmitted] = useState(false);
  const [status, setStatus] = useState<"idle" | "sending" | "sent">("idle");
  const errors = submitted ? validate(values) : {};

  function set(field: keyof Values) {
    return (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>) =>
      setValues((v) => ({ ...v, [field]: e.target.value }));
  }

  async function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    setSubmitted(true);
    const errs = validate(values);
    if (Object.keys(errs).length) {
      document.getElementById(`${id}-${Object.keys(errs)[0]}`)?.focus();
      return;
    }
    setStatus("sending");
    await new Promise((r) => setTimeout(r, 1100));
    setStatus("sent");
  }

  if (status === "sent") {
    return (
      <div role="status" className="rounded-2xl border border-line-strong bg-elev p-8 shadow-window md:p-10">
        <span className="grid size-10 place-items-center rounded-full bg-accent text-accent-fg">
          <Check weight="bold" className="size-5" />
        </span>
        <h2 className="mt-6 text-2xl font-semibold tracking-tight text-fg">Thanks, {values.name.trim().split(" ")[0]}.</h2>
        <p className="mt-2 max-w-[44ch] text-[15.5px] leading-relaxed text-muted">
          Your message is with the right team. We reply to {values.email.trim()} within one business day.
        </p>
        <button
          type="button"
          onClick={() => {
            setValues({ name: "", email: "", company: "", topic: "", message: "" });
            setSubmitted(false);
            setStatus("idle");
          }}
          className="mt-8 h-10 rounded-lg border border-line-strong px-4 text-[14px] text-fg hover:bg-sunken"
        >
          Send another message
        </button>
      </div>
    );
  }

  const field = (name: Field) => ({
    id: `${id}-${name}`,
    "aria-invalid": !!errors[name],
    "aria-describedby": errors[name] ? `${id}-${name}-error` : undefined,
  });
  const border = (name: Field) => (errors[name] ? "border-accent" : "border-line-strong focus:border-accent");
  const errorFor = (name: Field) =>
    errors[name] ? (
      <p id={`${id}-${name}-error`} className="text-[13px] text-accent">
        {errors[name]}
      </p>
    ) : null;

  return (
    <form
      onSubmit={onSubmit}
      noValidate
      className="grid gap-6 rounded-2xl border border-line-strong bg-elev p-6 shadow-window md:grid-cols-2 md:p-8"
    >
      <div className="grid gap-2">
        <label htmlFor={`${id}-name`} className="text-sm font-medium text-fg">Name</label>
        <input {...field("name")} autoComplete="name" value={values.name} onChange={set("name")} className={`${input} h-11 ${border("name")}`} />
        {errorFor("name")}
      </div>

      <div className="grid gap-2">
        <label htmlFor={`${id}-email`} className="text-sm font-medium text-fg">Work email</label>
        <input {...field("email")} type="email" autoComplete="email" value={values.email} onChange={set("email")} placeholder="you@company.com" className={`${input} h-11 ${border("email")}`} />
        {errorFor("email")}
      </div>

      <div className="grid gap-2">
        <label htmlFor={`${id}-company`} className="text-sm font-medium text-fg">
          Company <span className="font-normal text-subtle">(optional)</span>
        </label>
        <input id={`${id}-company`} autoComplete="organization" value={values.company} onChange={set("company")} className={`${input} h-11 border-line-strong focus:border-accent`} />
      </div>

      <div className="grid gap-2">
        <label htmlFor={`${id}-topic`} className="text-sm font-medium text-fg">Topic</label>
        <div className="relative">
          <select {...field("topic")} value={values.topic} onChange={set("topic")} className={`${input} h-11 appearance-none pr-10 ${border("topic")} ${values.topic ? "" : "text-subtle"}`}>
            <option value="" disabled>Choose a topic</option>
            {TOPICS.map(([v, l]) => (
              <option key={v} value={v}>{l}</option>
            ))}
          </select>
          <CaretDown className="pointer-events-none absolute right-3.5 top-1/2 size-4 -translate-y-1/2 text-subtle" />
        </div>
        {errorFor("topic")}
      </div>

      <div className="grid gap-2 md:col-span-2">
        <label htmlFor={`${id}-message`} className="text-sm font-medium text-fg">Message</label>
        <textarea {...field("message")} rows={6} value={values.message} onChange={set("message")} className={`${input} resize-y py-3 leading-relaxed ${border("message")}`} />
        {errors.message ? (
          errorFor("message")
        ) : (
          <p className="text-[13px] text-muted">Include your stack and team size if you are asking about plans.</p>
        )}
      </div>

      <div className="flex flex-col-reverse items-start gap-4 md:col-span-2 md:flex-row md:items-center md:justify-between">
        <p className="text-[13px] text-muted">We only use your details to reply to this message.</p>
        <button
          type="submit"
          disabled={status === "sending"}
          className="inline-flex h-12 items-center gap-2 rounded-lg bg-accent px-6 text-[15px] font-medium text-accent-fg transition hover:brightness-110 active:scale-[0.98] disabled:opacity-80"
        >
          {status === "sending" && <CircleNotch className="size-4 animate-spin motion-reduce:animate-none" />}
          {status === "sending" ? "Sending" : "Send message"}
        </button>
      </div>
    </form>
  );
}
