'use client';

/* An OPTIONAL "scribbled onto the page" nudge: a handwritten "Leave a star" note
   with a hand-drawn arrow that swoops RIGHT then UP from the word "star",
   its tip landing on the CENTER of the GitHub star pill's bottom edge — as if
   someone annotated the site by hand.

   ── WHY IT'S MEASURED, NOT HARDCODED ─────────────────────────────────────────
   The pill is right-anchored in the nav, so its position shifts with viewport
   width. A baked-in SVG only lines up at one width. We MEASURE the pill and
   recompute the arrow path so the tip sits on its center-bottom at EVERY width.
   If the pill isn't rendered (collapsed mobile nav), measurement yields null and
   the nudge hides itself.

   ── STICKY: BOUNCES ON OVERSCROLL, PINS WHEN SCROLLING, THEN FADES ───────────
   The arrow shares the nav's behavior by sharing its mechanism: we mount a
   `position: sticky; top: 0` node at the very TOP of <body>'s flow and draw the
   arrow inside it. At rest it sits in flow at the top, so a rubber-band
   overscroll bounces it DOWN together with the (also-sticky) nav — they never
   separate. The instant you scroll down, sticky pins it to the viewport top
   (like the nav), and we FADE it out so it reads as a hero scribble, not a
   persistent overlay. The compositor moves the stuck node and the stuck nav by
   the same amount during the elastic bounce, so the tip stays on the pill with
   no per-frame JS. The node is a direct <body> child (outside the hero's
   `overflow-x-hidden`, so sticky isn't trapped); `pointer-events-none` +
   `z-[60]` keep the bar clickable and let the arrow paint over the nav (z-40).

   ── KILL SWITCH ──────────────────────────────────────────────────────────────
   Flip the one constant below to remove the nudge ENTIRELY (note + arrow),
   leaving the clean top bar + pill untouched:

       const SHOW_STAR_NUDGE = false;
   ────────────────────────────────────────────────────────────────────────────── */

import { useCallback, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';

const SHOW_STAR_NUDGE = true;

/* The pill carries `data-github-star-pill` (see github-star-pill.tsx). The repo
   is also linked from the footer/docs, so target the attribute, not the href. */
const PILL_SELECTOR = '[data-github-star-pill]';

/* Scroll-fade: starts the moment you scroll down and completes quickly. */
const FADE_START = 0;
const FADE_END = 90;

type Geom = {
  textX: number;
  textY: number;
  shaft: string;
  head: string;
  /* svg box height — enough to contain the drawing (overflow is visible anyway) */
  height: number;
};

function computeGeom(rect: DOMRect): Geom {
  // Viewport coords: the sticky mount sits at the viewport top (top:0) once
  // pinned and at the page top at rest, so getBoundingClientRect coords line up.
  const tipX = rect.left + rect.width / 2;
  const tipY = rect.bottom + 3; // a hair below the pill's underside

  // Label sits below-and-left of the pill; the arrow leaves the END of "star".
  const textX = tipX - 92; // right edge of the (right-anchored) label
  const textY = tipY + 60; // baseline, ~60px under the pill

  // Shaft: starts just PAST "star" (~12px gap so it never touches the 'r'),
  // sweeps RIGHT then rises near-vertically into the tip.
  const sx = textX + 12;
  const shaft = `M ${sx} ${textY - 4} C ${sx + 38} ${textY + 2}, ${tipX - 6} ${tipY + 34}, ${tipX} ${tipY}`;

  // Arrowhead: two barbs BELOW the tip → reads as pointing UP into the pill.
  const head = `M ${tipX - 8} ${tipY + 13} L ${tipX} ${tipY} L ${tipX + 9} ${tipY + 12}`;

  return { textX, textY, shaft, head, height: textY + 30 };
}

function scrollOpacity(y: number): number {
  return Math.max(0, Math.min(1, 1 - (y - FADE_START) / (FADE_END - FADE_START)));
}

export function StarNudge() {
  const [geom, setGeom] = useState<Geom | null>(null);
  const [opacity, setOpacity] = useState(1);
  const mountRef = useRef<HTMLDivElement | null>(null);
  const [mounted, setMounted] = useState(false);

  // The sticky host: a height-0 node prepended to <body> so its flow position is
  // the very top of the page. top:0 makes it pin like the nav once you scroll.
  useEffect(() => {
    if (!SHOW_STAR_NUDGE) return;
    const node = document.createElement('div');
    node.setAttribute('aria-hidden', 'true');
    Object.assign(node.style, {
      position: 'sticky',
      top: '0',
      height: '0',
      zIndex: '60',
      pointerEvents: 'none',
    });
    document.body.prepend(node);
    mountRef.current = node;
    setMounted(true);
    return () => {
      node.remove();
      mountRef.current = null;
    };
  }, []);

  const measure = useCallback(() => {
    const pill = document.querySelector(PILL_SELECTOR);
    if (!pill) return setGeom(null);
    const rect = pill.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return setGeom(null);
    setGeom(computeGeom(rect));
  }, []);

  useEffect(() => {
    if (!SHOW_STAR_NUDGE) return;
    measure();
    setOpacity(scrollOpacity(window.scrollY));

    // Geometry only on layout changes — the sticky host + compositor handle
    // scroll/overscroll movement on their own; no per-scroll recompute needed.
    window.addEventListener('resize', measure);
    const ro = new ResizeObserver(measure);
    ro.observe(document.documentElement);
    document.fonts?.ready.then(measure).catch(() => {});

    // Scroll: fade only.
    const onScroll = () => setOpacity(scrollOpacity(window.scrollY));
    window.addEventListener('scroll', onScroll, { passive: true });

    return () => {
      window.removeEventListener('resize', measure);
      window.removeEventListener('scroll', onScroll);
      ro.disconnect();
    };
  }, [measure]);

  if (!SHOW_STAR_NUDGE || !mounted || !geom || !mountRef.current) return null;

  return createPortal(
    <svg
      aria-hidden="true"
      className="pointer-events-none absolute left-0 top-0 w-full select-none text-ember/85"
      style={{ height: geom.height, opacity, overflow: 'visible' }}
      fill="none"
    >
      <text
        x={geom.textX}
        y={geom.textY}
        textAnchor="end"
        transform={`rotate(-4 ${geom.textX} ${geom.textY})`}
        className="fill-ember/90 font-[family-name:var(--font-caveat)]"
        style={{ fontSize: '21px' }}
      >
        Leave a star!
      </text>
      <path
        d={geom.shaft}
        stroke="currentColor"
        strokeWidth="1.9"
        strokeLinecap="round"
      />
      <path
        d={geom.head}
        stroke="currentColor"
        strokeWidth="1.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>,
    mountRef.current,
  );
}
