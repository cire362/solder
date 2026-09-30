import { Reveal } from "./reveal";
import { FloraCluster, type FloraPreset } from "./flora-cluster";

// Shared header for secondary pages. Headline + body stacked, left-aligned by default.
// An optional bouquet grows from the header's bottom edge: on the right for left-aligned
// headers, on both sides for centered ones (`floraAlt` is the left-hand bouquet).
export function PageIntro({
  title,
  body,
  children,
  center = false,
  flora,
  floraAlt,
}: {
  title: React.ReactNode;
  body?: React.ReactNode;
  children?: React.ReactNode;
  center?: boolean;
  flora?: FloraPreset;
  floraAlt?: FloraPreset;
}) {
  const bouquet = "absolute bottom-0 hidden w-[230px] lg:block xl:w-[270px]";
  return (
    <header className="relative mx-auto max-w-[1400px] px-4 pb-12 pt-16 md:px-8 md:pb-16 md:pt-24">
      {flora && <FloraCluster preset={flora} className={`${bouquet} right-4 md:right-8`} />}
      {center && floraAlt && <FloraCluster preset={floraAlt} flip className={`${bouquet} left-4 md:left-8`} delay={0.7} />}
      <Reveal y={16} className={`relative ${center ? "mx-auto max-w-3xl text-center" : "max-w-3xl"}`}>
        <h1 className="text-[40px] font-semibold leading-[1.02] tracking-tighter text-fg md:text-6xl">{title}</h1>
        {body && (
          <p className={`mt-5 max-w-[58ch] text-lg leading-relaxed text-muted ${center ? "mx-auto" : ""}`}>{body}</p>
        )}
      </Reveal>
      {children && (
        <Reveal y={16} delay={0.08} className={`relative ${center ? "mt-8 flex justify-center" : "mt-8"}`}>
          {children}
        </Reveal>
      )}
    </header>
  );
}
