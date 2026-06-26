'use client';

/* An OPTIONAL "scribbled onto the page" nudge: a handwritten "Leave a star" note
   with a hand-drawn arrow that swoops to the RIGHT and then UP, pointing at the
   BOTTOM edge of the GitHub star pill in the top bar — as if someone annotated
   the site by hand.

   ── STACKING / POSITIONING (why a portal) ────────────────────────────────────
   The arrow's tip reaches UP into the top bar. The home nav is a sticky header
   with `z-40` (fumadocs `layouts/home/slots/header.js`), which paints above the
   hero's stacking context — so an `absolute` nudge mounted inside the hero gets
   CLIPPED behind the bar's background no matter how high its local z-index is
   (its z is resolved inside the hero's lower context, not against the nav).

   Fix: PORTAL the nudge to `document.body` and position it `absolute` with
   `z-[60]` (clear of the nav's z-40 and any other z-50 element). As a direct body
   child it is no longer trapped in the hero's stacking context, so z-[60] truly
   outranks the nav and the whole swoop — arrowhead included — renders ON TOP of
   the bar. `absolute` (not `fixed`) keeps it anchored to the PAGE top, so it
   scrolls away with the hero instead of floating over the sections below.
   `pointer-events-none` keeps the bar's search/theme/pill clickable underneath.

   ── KILL SWITCH ──────────────────────────────────────────────────────────────
   Flip the one constant below to remove the nudge ENTIRELY (note + arrow),
   leaving the clean top bar + pill untouched:

       const SHOW_STAR_NUDGE = false;
   ────────────────────────────────────────────────────────────────────────────── */

import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';

const SHOW_STAR_NUDGE = false;

/* Hand-drawn arrow: ONE continuous swoop that starts at the text (lower-LEFT),
   travels to the RIGHT, then curves UP, with the arrowhead landing at the
   TOP-RIGHT of the viewBox — i.e. at the pill's bottom edge once positioned.
   Wide viewBox so the dominant motion is right-then-up, not a short diagonal.
   The gentle wobble keeps it reading as drawn by hand. */
function HandArrow({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 150 96"
      fill="none"
      className={className}
      aria-hidden="true"
    >
      {/* shaft: from the text end (~6,84) sweep RIGHT across the bottom, then
          curve UP, with the LAST segment rising near-vertically so the arrow
          arrives straight up into the pill's underside. Tip at (~132,12). */}
      <path
        d="M6 84 C 36 94, 78 92, 106 76 C 126 64, 132 40, 132 12"
        stroke="currentColor"
        strokeWidth="2.6"
        strokeLinecap="round"
      />
      {/* arrowhead at the tip (~132,12): two barbs BELOW the tip so it reads as
          an upward-pointing arrowhead arriving at the pill from underneath. */}
      <path
        d="M123 26 L 132 11 L 142 24"
        stroke="currentColor"
        strokeWidth="2.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function StarNudge() {
  // Portal target is only available after mount (no SSR document). Render nothing
  // on the server / first paint, then portal into <body> on the client.
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);

  if (!SHOW_STAR_NUDGE || !mounted) return null;

  return createPortal(
    <div
      aria-hidden="true"
      className="pointer-events-none absolute right-[99px] top-[44px] z-[60] hidden select-none lg:block"
    >
      {/* Text on the LEFT, arrow swooping right-and-up from it. Items aligned to
          the bottom so the text baseline sits at the swoop's starting height. */}
      <div className="flex items-end gap-1">
        <span className="mb-1 -rotate-3 whitespace-nowrap font-[family-name:var(--font-caveat)] text-xl leading-none text-ember/90">
          Leave a star
        </span>
        <HandArrow className="h-24 w-[150px] text-ember/80" />
      </div>
    </div>,
    document.body,
  );
}
