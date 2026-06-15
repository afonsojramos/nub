import type { ReactNode } from 'react';

// Per-package-manager compatibility table. Vertical (one feature per row) so it
// never overflows horizontally, with a color-coded status glyph:
//   yes      → green check  (supported)
//   no       → red X        (unsupported)
//   partial  → amber check  (partially supported — use sparingly)
// The check/X are distinct in BOTH shape and color so support reads at a glance.
// Styled to match the docs' dark theme and the existing fumadocs table look.

export type CompatStatus = 'yes' | 'no' | 'partial';

export interface CompatRow {
  /** The config field / capability — markdown rendered, so backticked code works. */
  feature: ReactNode;
  status: CompatStatus;
  /** The qualifier that was packed into the old cell (e.g. "read+write"). Optional. */
  note?: ReactNode;
}

function StatusGlyph({ status }: { status: CompatStatus }) {
  if (status === 'no') {
    return (
      <span className="inline-flex items-center gap-1.5 font-medium text-red-500 dark:text-red-400">
        <svg
          aria-hidden
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="3"
          strokeLinecap="round"
          strokeLinejoin="round"
          className="size-3.5 shrink-0"
        >
          <path d="M18 6 6 18" />
          <path d="m6 6 12 12" />
        </svg>
        <span className="sr-only">Not supported</span>
      </span>
    );
  }
  const amber = status === 'partial';
  return (
    <span
      className={`inline-flex items-center gap-1.5 font-medium ${
        amber
          ? 'text-amber-600 dark:text-amber-400'
          : 'text-emerald-600 dark:text-emerald-400'
      }`}
    >
      <svg
        aria-hidden
        width="14"
        height="14"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="3"
        strokeLinecap="round"
        strokeLinejoin="round"
        className="size-3.5 shrink-0"
      >
        <path d="M20 6 9 17l-5-5" />
      </svg>
      <span className="sr-only">{amber ? 'Partially supported' : 'Supported'}</span>
    </span>
  );
}

export function CompatTable({ rows }: { rows: CompatRow[] }) {
  return (
    <div className="my-6 overflow-hidden rounded-lg border border-fd-border">
      <table className="w-full border-collapse text-left text-sm">
        <thead>
          <tr className="border-b border-fd-border bg-fd-muted/40">
            <th className="px-4 py-2.5 font-medium text-fd-muted-foreground">Feature</th>
            <th className="w-px whitespace-nowrap px-4 py-2.5 font-medium text-fd-muted-foreground">
              Status
            </th>
            <th className="px-4 py-2.5 font-medium text-fd-muted-foreground">Notes</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i} className="border-b border-fd-border/60 last:border-0">
              <td className="px-4 py-2.5 align-top font-mono text-fd-foreground [&_code]:bg-transparent [&_code]:p-0">
                {row.feature}
              </td>
              <td className="w-px whitespace-nowrap px-4 py-2.5 align-top">
                <StatusGlyph status={row.status} />
              </td>
              <td className="px-4 py-2.5 align-top text-fd-muted-foreground">
                {row.note ?? ''}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
