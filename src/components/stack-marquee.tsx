import {
  siBun,
  siDeno,
  siDocker,
  siGo,
  siGraphql,
  siKubernetes,
  siNextdotjs,
  siNodedotjs,
  siPostgresql,
  siPrisma,
  siPython,
  siReact,
  siRedis,
  siRust,
  siSvelte,
  siTailwindcss,
  siTypescript,
  siVuedotjs,
  type SimpleIcon,
} from "simple-icons";

const ICONS: SimpleIcon[] = [
  siTypescript, siReact, siNextdotjs, siVuedotjs, siSvelte, siTailwindcss, siNodedotjs,
  siBun, siDeno, siGo, siRust, siPython, siGraphql, siPrisma, siPostgresql, siRedis,
  siDocker, siKubernetes,
];

function Row({ hidden }: { hidden?: boolean }) {
  return (
    <ul className="flex shrink-0 items-center gap-14 pr-14" aria-hidden={hidden}>
      {ICONS.map((icon) => (
        <li key={icon.slug}>
          <svg
            role="img"
            aria-label={hidden ? undefined : icon.title}
            viewBox="0 0 24 24"
            className="size-7 fill-current text-subtle transition-colors duration-300 hover:text-fg"
          >
            <path d={icon.path} />
          </svg>
        </li>
      ))}
    </ul>
  );
}

export function StackMarquee() {
  return (
    <section aria-labelledby="stack-heading" className="border-y border-line py-10">
      <h2 id="stack-heading" className="mb-8 px-4 text-center text-sm text-muted">
        First-class support for the stack you already run
      </h2>
      <div className="marquee relative flex overflow-hidden [mask-image:linear-gradient(to_right,transparent,black_12%,black_88%,transparent)]">
        <div className="marquee-track flex w-max">
          <Row />
          <Row hidden />
        </div>
      </div>
    </section>
  );
}
