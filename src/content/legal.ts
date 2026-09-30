import type { Block } from "@/lib/blocks";

// mock: placeholder legal copy for layout purposes only. Must be replaced by text reviewed by counsel.
export type LegalDoc = { title: string; updated: string; iso: string; intro: string; body: Block[] };

export const PRIVACY: LegalDoc = {
  title: "Privacy policy",
  updated: "September 1, 2026",
  iso: "2026-09-01",
  intro: "What Solder Labs collects, why, and the controls you have. The short version: your code stays on your machine unless you send it to hosted AI, and we never train on it.",
  body: [
    { h: "Code and project data", id: "code" },
    { p: "The editor runs locally. Source code, databases and logs are not uploaded to Solder Labs. Hosted AI requests send only the files selected for that request, are processed in memory and are discarded after the response." },
    { h: "Account data", id: "account" },
    { p: "If you create an account we store your name, email address, plan and billing details. Payments are handled by our payment processor, and we never see full card numbers." },
    { h: "Usage telemetry", id: "telemetry" },
    { p: "Solder sends anonymous crash reports and performance metrics, like startup time and input latency. It never includes file names, file contents or keystrokes. Turn it off in **Settings, Privacy**." },
    { h: "Your rights", id: "rights" },
    { list: ["Export or delete your account data from the account page", "Opt out of telemetry at any time", "Contact privacy@solder.dev with any request, and we reply within 30 days"] },
  ],
};

export const TERMS: LegalDoc = {
  title: "Terms of service",
  updated: "September 1, 2026",
  iso: "2026-09-01",
  intro: "The agreement between you and Solder Labs for using the Solder editor, Solder AI and related services.",
  body: [
    { h: "Using Solder", id: "using" },
    { p: "The Hobby plan is free for personal, educational and open-source use. Commercial use by teams of two or more requires a Team plan." },
    { h: "Your content", id: "content" },
    { p: "You own your code and everything the editor or AI produces for you. You give us only the rights needed to run the services you choose to use." },
    { h: "Plugins", id: "plugins" },
    { p: "Third-party plugins are provided by their authors under their own licenses. We review registry plugins for malware, but we are not responsible for how they behave." },
    { h: "Billing and cancellation", id: "billing" },
    { p: "Paid plans renew automatically. You can cancel at any time and keep access until the end of the billing period." },
  ],
};

export const SECURITY: LegalDoc = {
  title: "Security",
  updated: "August 12, 2026",
  iso: "2026-08-12",
  intro: "How we protect the editor, the plugin registry and hosted services, and how to report a vulnerability.",
  body: [
    { h: "Reporting a vulnerability", id: "report" },
    { p: "Email **security@solder.dev** with details and steps to reproduce. We acknowledge reports within two business days and keep you updated until the fix ships." },
    { note: "Please give us 90 days to fix an issue before public disclosure. We credit researchers in release notes unless you ask us not to." },
    { h: "Signed builds", id: "builds" },
    { p: "Every release is code-signed and notarized. Checksums and signatures are published with each release, and the updater verifies them before installing." },
    { h: "Plugin sandbox", id: "sandbox" },
    { p: "Plugins run in WebAssembly with only the permissions they declare. Registry builds are compiled from source and signed by us." },
    { h: "Hosted services", id: "hosted" },
    { list: ["Encryption in transit with TLS 1.3 and at rest with AES-256", "SOC 2 Type II audit in progress", "Single sign-on and audit logs on the Team plan"] },
  ],
};
