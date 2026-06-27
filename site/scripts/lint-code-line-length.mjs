#!/usr/bin/env node
/* Lints fenced code-block line length across the MDX content. The docs render
   code in a fixed-width column (`#nd-page`, ~720px), so an over-long line forces
   a horizontal scrollbar — never acceptable. This flags every fenced code line
   over the limit so it can be shortened (move a trailing comment to its own
   line, drop alignment padding, reflow long output) BEFORE it ships.

   Budget: 80 columns for a normal full-width block. IBM Plex Mono at the docs'
   code size fits ~80 chars in the 720px column; staying at/under 80 keeps every
   block scroll-free.

   CALLOUT EXCEPTION: a code block nested inside a <Callout> is inset by the
   callout's padding, so its column is ~42px narrower and fits only ~75 chars.
   Fences inside a <Callout>…</Callout> region get the tighter CALLOUT_LIMIT (74,
   a hair under the measured 75-char fit) so callout-nested code never scrolls.

   Scope: fenced ``` blocks in the content tree. The hand-built terminal
   components (Terminal/ShimDemo/Source) take their lines as props, not fenced
   text, so they aren't covered here — keep those lines short by the same budget.

   Usage: node scripts/lint-code-line-length.mjs [--limit N]
   Exits non-zero if any violation is found (CI gate). */

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { globSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const limitArg = process.argv.indexOf('--limit');
const LIMIT = limitArg !== -1 ? Number(process.argv[limitArg + 1]) : 80;
const CALLOUT_LIMIT = Math.min(LIMIT, 74);

const files = globSync('content/**/*.{md,mdx}', { cwd: root });

let violations = 0;
const byFile = new Map();

for (const rel of files.sort()) {
  const text = readFileSync(join(root, rel), 'utf8');
  const lines = text.split('\n');
  let inFence = false;
  let fenceMarker = '';
  let calloutDepth = 0;
  lines.forEach((line, i) => {
    // Track <Callout>…</Callout> nesting (only meaningful outside a fence — a
    // fence body never opens a JSX component). Code inside a callout gets the
    // tighter column, so it gets the tighter limit.
    if (!inFence) {
      if (/<Callout(\s|>)/.test(line)) calloutDepth++;
      if (/<\/Callout>/.test(line)) calloutDepth = Math.max(0, calloutDepth - 1);
    }
    const fenceOpen = line.match(/^(\s*)(`{3,}|~{3,})/);
    if (fenceOpen) {
      const marker = fenceOpen[2][0].repeat(3);
      if (!inFence) {
        inFence = true;
        fenceMarker = marker;
        return; // the opening fence line itself isn't content
      }
      // a fence line while open: close only if the marker family matches
      if (fenceOpen[2][0].repeat(3) === fenceMarker) {
        inFence = false;
        return;
      }
    }
    if (!inFence) return;
    // Measure the raw authored length (Array.from → count code points, so an
    // emoji/box-drawing glyph counts as one column, matching the eye).
    const len = Array.from(line).length;
    const limit = calloutDepth > 0 ? CALLOUT_LIMIT : LIMIT;
    if (len > limit) {
      violations++;
      if (!byFile.has(rel)) byFile.set(rel, []);
      byFile.get(rel).push({ line: i + 1, len, limit, text: line });
    }
  });
}

if (violations === 0) {
  console.log(`✓ code line-length: all fenced code lines ≤ ${LIMIT} cols`);
  process.exit(0);
}

console.error(`✗ code line-length: ${violations} line(s) over ${LIMIT} cols\n`);
for (const [file, rows] of byFile) {
  console.error(`  ${file}`);
  for (const r of rows) {
    console.error(`    ${r.line}:  ${r.len} cols (>${r.limit})  ${JSON.stringify(r.text)}`);
  }
  console.error('');
}
process.exit(1);
