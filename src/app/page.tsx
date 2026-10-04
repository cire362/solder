import { Hero } from "@/components/hero";
import { StackMarquee } from "@/components/stack-marquee";
import { BuiltIn } from "@/components/built-in";
import { AISection } from "@/components/ai-section";
import { Performance } from "@/components/performance";
import { Plugins } from "@/components/plugins";
import { DownloadSection } from "@/components/download-section";

export default function Home() {
  return (
    <>
      <Hero />
      <StackMarquee />
      <BuiltIn />
      <AISection />
      <Performance />
      <Plugins />
      <DownloadSection />
    </>
  );
}
