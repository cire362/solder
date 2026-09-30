import type { Metadata } from "next";
import { LegalPage } from "@/components/legal-page";
import { TERMS } from "@/content/legal";

export const metadata: Metadata = { title: "Terms of service", description: TERMS.intro };

export default function Page() {
  return <LegalPage doc={TERMS} path="/terms" />;
}
