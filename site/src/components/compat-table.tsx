import type { ReactNode } from 'react';

/** Plain-text the amber tooltip needs from a (possibly markdown) note. */
function noteText(note: ReactNode): string | undefined {
  if (note == null || note === '') return undefined;
  if (typeof note === 'string') return note;
  return undefined;
}

// Per-package-manager compatibility table. Vertical (one feature per row) so it
// never overflows horizontally, with a color-coded status glyph:
//   yes      → green check  (supported)
//   no       → red X        (a genuine nub gap — the incumbent PM has it, nub doesn't mirror it)
//   partial  → amber check  (partially supported — use sparingly)
//   n/a      → gray dash     (not this PM's feature — a faithful mirror correctly omits it; NOT a failure)
// The check/X/dash are distinct in BOTH shape and color so support reads at a glance.
// Styled to match the docs' dark theme and the existing fumadocs table look.

export type CompatStatus = 'yes' | 'no' | 'partial' | 'n/a';

export interface CompatRow {
  /** The config field / capability — markdown rendered, so backticked code works. */
  feature: ReactNode;
  /** nub's support for the feature. */
  status: CompatStatus;
  /** The incumbent PM's support, when a two-column (incumbent vs nub) table is
   *  rendered. Defaults to 'yes' — the rows are the incumbent's own features. */
  incumbentStatus?: CompatStatus;
  /** The qualifier that was packed into the old cell (e.g. "read+write"). Optional. */
  note?: ReactNode;
}

const SVG_PROPS = {
  'aria-hidden': true,
  width: 14,
  height: 14,
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 3,
  strokeLinecap: 'round',
  strokeLinejoin: 'round',
  className: 'size-3.5 shrink-0',
} as const;

const STATUS_META: Record<
  CompatStatus,
  { color: string; label: string; icon: ReactNode }
> = {
  'n/a': {
    color: 'text-fd-muted-foreground',
    label: 'Not applicable',
    icon: (
      <svg {...SVG_PROPS}>
        <path d="M5 12h14" />
      </svg>
    ),
  },
  no: {
    color: 'text-red-500 dark:text-red-400',
    label: 'Not supported',
    icon: (
      <svg {...SVG_PROPS}>
        <path d="M18 6 6 18" />
        <path d="m6 6 12 12" />
      </svg>
    ),
  },
  partial: {
    color: 'text-amber-600 dark:text-amber-400',
    label: 'Partially supported',
    icon: (
      <svg {...SVG_PROPS}>
        <path d="M20 6 9 17l-5-5" />
      </svg>
    ),
  },
  yes: {
    color: 'text-emerald-600 dark:text-emerald-400',
    label: 'Supported',
    icon: (
      <svg {...SVG_PROPS}>
        <path d="M20 6 9 17l-5-5" />
      </svg>
    ),
  },
};

function StatusGlyph({
  status,
  tooltip,
}: {
  status: CompatStatus;
  /** Plain-text breakdown surfaced on hover/focus of the glyph. */
  tooltip?: string;
}) {
  const { color, label, icon } = STATUS_META[status];
  // Surface the note on hover/focus whenever the row has one (not just amber):
  // a CSS-only reveal (`group` + `group-hover`/`group-focus-within`) plus
  // sr-only text, so it's reachable by pointer, keyboard, and screen readers.
  // The wrapper is focusable so keyboard users get it too. No native `title` —
  // it would double up with the custom popover on hover.
  const hasTip = !!tooltip;
  const glyph = (
    <span
      className={`inline-flex items-center gap-1.5 font-medium ${color} ${
        hasTip
          ? 'cursor-help underline decoration-dotted decoration-from-font underline-offset-4'
          : ''
      }`}
    >
      {icon}
      <span className="sr-only">
        {label}
        {hasTip ? `. ${tooltip}` : ''}
      </span>
    </span>
  );
  if (!hasTip) return glyph;
  return (
    <span
      className="group relative inline-flex"
      tabIndex={0}
    >
      {glyph}
      <span
        role="tooltip"
        className="pointer-events-none invisible absolute left-1/2 top-full z-20 mt-1.5 w-64 -translate-x-1/2 whitespace-normal rounded-md border border-fd-border bg-fd-popover px-3 py-2 text-left text-xs font-normal leading-relaxed text-fd-popover-foreground opacity-0 shadow-md transition-opacity duration-100 group-hover:visible group-hover:opacity-100 group-focus-within:visible group-focus-within:opacity-100"
      >
        {tooltip}
      </span>
    </span>
  );
}

export function CompatTable({
  rows,
  incumbent,
}: {
  rows: CompatRow[];
  /** When set, render a two-column "<incumbent> vs nub" parity view — each row's
   *  feature belongs to the incumbent PM (so its column defaults to ✓) shown
   *  beside nub's support. Omit it for the default single-(nub-)status table. */
  incumbent?: string;
}) {
  const pmTh =
    'w-px whitespace-nowrap px-4 py-2.5 text-center font-mono font-medium text-fd-muted-foreground';
  const pmTd = 'w-px whitespace-nowrap px-4 py-2.5 text-center align-top';
  return (
    <div className="my-6 overflow-hidden rounded-lg border border-fd-border [&_table]:my-0">
      <table className="w-full border-collapse text-left text-sm">
        <thead>
          <tr className="border-b border-fd-border bg-fd-muted/40">
            <th className="px-4 py-2.5 font-medium text-fd-muted-foreground">Feature</th>
            {incumbent ? (
              <>
                <th className={pmTh}>{incumbent}</th>
                <th className={pmTh}>nub</th>
              </>
            ) : (
              <th className="w-px whitespace-nowrap px-4 py-2.5 font-medium text-fd-muted-foreground">
                Status
              </th>
            )}
            <th className="px-4 py-2.5 font-medium text-fd-muted-foreground">Notes</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i} className="border-b border-fd-border/60 last:border-0">
              <td className="px-4 py-2.5 align-top text-fd-foreground">
                {row.feature}
              </td>
              {incumbent ? (
                <>
                  <td className={pmTd}>
                    <span className="inline-flex justify-center">
                      <StatusGlyph status={row.incumbentStatus ?? 'yes'} />
                    </span>
                  </td>
                  <td className={pmTd}>
                    <span className="inline-flex justify-center">
                      <StatusGlyph status={row.status} tooltip={noteText(row.note)} />
                    </span>
                  </td>
                </>
              ) : (
                <td className="w-px whitespace-nowrap px-4 py-2.5 align-top">
                  <StatusGlyph status={row.status} tooltip={noteText(row.note)} />
                </td>
              )}
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
