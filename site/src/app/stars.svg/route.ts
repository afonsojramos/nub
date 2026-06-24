// `/stars.svg` — a dynamically-generated animated star-count badge for the README.
//
// Returns `image/svg+xml` with the live nubjs/nub star count baked in plus a CSS
// `@keyframes` count-up that plays once on render. The animation MUST be declarative
// CSS (or SMIL): GitHub embeds README images as `<img>`, which is script-disabled, so
// no JS runs — but internal `<style>` `@keyframes` animate (the readme-typing-svg
// precedent). Each digit column is a vertical "tape" of glyphs that slides up so the
// final digit lands in the viewport, easing + freezing (`forwards`, iteration-count 1).
//
// Freshness is camo-bound: GitHub proxies + caches README images (~31 days default), so
// "fresh on every load" is structurally impossible. We send a 1-hour s-maxage with a long
// stale-while-revalidate so the count is approximately current while never approaching the
// 60/hr unauthenticated GitHub API limit. The animation replays whenever a viewer's
// browser actually (re)fetches + renders the SVG — a cache-miss render, not literally
// every visit.

export const revalidate = 3600; // ISR: re-fetch the count at most hourly on the origin.

const GITHUB_API = 'https://api.github.com/repos/nubjs/nub';
const FALLBACK = 1700; // sensible floor if the API call fails — never error the image.

// Brand (from site/src/app/global.css). Warm cream + ember on a card surface.
const COLOR = {
  bg: '#fffdf8',
  border: '#e4dccb',
  fg: '#1a1714',
  ember: '#ff5d3b',
  muted: '#6b6358',
};

async function getStarCount(): Promise<number> {
  try {
    const res = await fetch(GITHUB_API, {
      headers: { Accept: 'application/vnd.github+json' },
      // Next ISR cache — bounded by `revalidate` above; keeps us off the rate limit.
      next: { revalidate: 3600 },
    });
    if (!res.ok) return FALLBACK;
    const data = (await res.json()) as { stargazers_count?: number };
    return typeof data.stargazers_count === 'number' ? data.stargazers_count : FALLBACK;
  } catch {
    return FALLBACK;
  }
}

const DIGIT_H = 30; // px advance per glyph in a tape — matches the text line-height below.

/**
 * A single rolling digit column. Renders glyphs 0..final stacked top-to-bottom, then
 * slides the stack up by `final * DIGIT_H` so the final glyph sits at baseline. A short
 * per-column delay (left digits settle first) gives the cascading odometer feel.
 */
function digitTape(finalDigit: number, x: number, colIndex: number): string {
  const glyphs: string[] = [];
  for (let d = 0; d <= finalDigit; d++) {
    glyphs.push(`<text x="${x}" y="${21 + d * DIGIT_H}">${d}</text>`);
  }
  const travel = finalDigit * DIGIT_H;
  const delay = colIndex * 0.08;
  return `<g class="tape" style="--travel:-${travel}px;animation-delay:${delay}s">${glyphs.join('')}</g>`;
}

function renderSvg(count: number): string {
  const text = count.toLocaleString('en-US'); // e.g. "1,742"
  // Lay out each character left-to-right. Digits get a rolling tape clipped to their own
  // column; the comma is static. Each digit carries its absolute x so its clipPath aligns.
  const charW = 14;
  const startX = 44; // after the star glyph
  let x = startX;
  let colIndex = 0;
  const cols: string[] = [];
  const statics: string[] = [];
  for (const ch of text) {
    if (ch >= '0' && ch <= '9') {
      cols.push(
        `<clipPath id="clip${colIndex}"><rect x="${x - 2}" y="3" width="${charW}" height="28"/></clipPath>` +
          `<g clip-path="url(#clip${colIndex})">${digitTape(Number(ch), x, colIndex)}</g>`,
      );
      colIndex++;
      x += charW;
    } else {
      // comma / separator — static, slightly narrower
      statics.push(`<text x="${x}" y="21" class="sep">${ch}</text>`);
      x += charW * 0.45;
    }
  }
  const labelX = x + 8;
  const width = labelX + 56;

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="40" viewBox="0 0 ${width} 40" role="img" aria-label="${text} GitHub stars">
  <style>
    @keyframes roll {
      0%   { transform: translateY(0); }
      100% { transform: translateY(var(--travel)); }
    }
    .frame { fill: ${COLOR.bg}; stroke: ${COLOR.border}; }
    text { font: 600 22px ui-monospace, "SF Mono", "IBM Plex Mono", Menlo, monospace; fill: ${COLOR.fg}; }
    .star { fill: ${COLOR.ember}; }
    .sep { fill: ${COLOR.fg}; }
    .label { font: 600 13px ui-sans-serif, system-ui, sans-serif; fill: ${COLOR.muted}; letter-spacing: .04em; }
    .tape {
      animation: roll 1.4s cubic-bezier(.16,1,.3,1) forwards;
      animation-iteration-count: 1;
    }
  </style>
  <rect class="frame" x="0.5" y="0.5" width="${width - 1}" height="39" rx="9"/>
  <path class="star" transform="translate(16 9) scale(0.9)" d="M11 0l3.09 6.26L21 7.27l-5 4.87 1.18 6.88L11 15.77 4.82 19.02 6 12.14 1 7.27l6.91-1.01z"/>
  ${cols.join('\n  ')}
  ${statics.join('\n  ')}
  <text x="${labelX}" y="20" class="label">STARS</text>
</svg>`;
}

export async function GET() {
  const count = await getStarCount();
  const svg = renderSvg(count);
  return new Response(svg, {
    headers: {
      'Content-Type': 'image/svg+xml; charset=utf-8',
      // ~1h fresh at the edge, serve-stale-while-revalidating for a day. Keeps the count
      // approximately current without hammering the unauthenticated GitHub API.
      'Cache-Control': 'public, max-age=0, s-maxage=3600, stale-while-revalidate=86400',
    },
  });
}
