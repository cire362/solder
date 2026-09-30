export function LogoMark({ className = "size-6" }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="none" aria-hidden className={className}>
      <rect x="1.5" y="6" width="8" height="12" rx="2.5" fill="currentColor" />
      <rect x="14.5" y="6" width="8" height="12" rx="2.5" fill="currentColor" />
      <circle cx="12" cy="12" r="3.25" fill="var(--accent)" />
    </svg>
  );
}

export function Logo() {
  return (
    <span className="flex items-center gap-2 text-fg">
      <LogoMark />
      <span className="text-[17px] font-semibold tracking-tight">Solder</span>
    </span>
  );
}
