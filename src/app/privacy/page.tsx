import type { Metadata } from "next";
import { LegalPage } from "@/components/legal-page";
import { PRIVACY } from "@/content/legal";

export const metadata: Metadata = { title: "Privacy policy", description: PRIVACY.intro };

export default function Page() {
  return <LegalPage doc={PRIVACY} path="/privacy" />;
}
