import type { ReactNode } from 'react';

// Shared glyph geometry — the same lucide check/x paths and stroke the CompatTable
// uses, so the Runner comparison table reads identically to the rest of the site.
const SVG_PROPS = {
  width: 16,
  height: 16,
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 3,
  strokeLinecap: 'round',
  strokeLinejoin: 'round',
  className: 'inline size-4',
} as const;

// Green check / red x, colored from the page's status tokens (defined per-theme in
// global.css, so both are AA-contrast in light and dark). Centered by the ColGlow
// cell styling below.
export function Yes() {
  return (
    <span style={{ color: 'var(--status-ok)' }}>
      <svg {...SVG_PROPS} role="img" aria-label="Yes">
        <path d="M20 6 9 17l-5-5" />
      </svg>
    </span>
  );
}

export function No() {
  return (
    <span style={{ color: 'var(--status-bad)' }}>
      <svg {...SVG_PROPS} role="img" aria-label="No">
        <path d="M18 6 6 18" />
        <path d="m6 6 12 12" />
      </svg>
    </span>
  );
}

// Wrapper for a raw <table> in MDX: supplies the full table chrome (border, header
// tint, cell padding — a bare JSX <table> bypasses fumadocs' markdown-table styling)
// and glows the last column (nubx) a light green so the "one command covers every
// tier" point reads at a glance.
export function ColGlow({ children }: { children: ReactNode }) {
  return (
    <div
      className="my-6 overflow-x-auto rounded-lg border border-fd-border
        [&_table]:my-0 [&_table]:w-full [&_table]:border-collapse [&_table]:text-sm
        [&_thead_tr]:border-b [&_thead_tr]:border-fd-border [&_thead_tr]:bg-fd-muted/40
        [&_th]:whitespace-nowrap [&_th]:px-2 [&_th]:py-2.5 [&_th]:text-center [&_th]:text-xs [&_th]:font-medium [&_th]:text-fd-muted-foreground
        [&_th_code]:!bg-transparent [&_th_code]:!p-0 [&_th_code]:!text-inherit
        [&_td]:px-2 [&_td]:py-2.5 [&_td]:text-center [&_td]:align-middle
        [&_tbody_tr]:border-b [&_tbody_tr]:border-fd-border/60 [&_tbody_tr:last-child]:border-0
        [&_th:first-child]:px-4 [&_th:first-child]:text-left [&_td:first-child]:whitespace-nowrap [&_td:first-child]:px-4 [&_td:first-child]:text-left
        [&_th:last-child]:bg-emerald-500/[0.15] [&_td:last-child]:bg-emerald-500/[0.08]
        [&_th:last-child]:font-semibold [&_th:last-child]:!text-[var(--status-ok)] [&_th:last-child_code]:!text-[var(--status-ok)]"
    >
      {children}
    </div>
  );
}
