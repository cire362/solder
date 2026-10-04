import type { Metadata } from "next";
import { LegalPage } from "@/components/legal-page";
import { SECURITY } from "@/content/legal";

export const metadata: Metadata = { title: "Security", description: SECURITY.intro };

export default function Page() {
  return <LegalPage doc={SECURITY} path="/security" />;
}
