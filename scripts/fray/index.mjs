#!/usr/bin/env node
/**
 * fray — derive the `## Threads` board table from the per-thread `.fray/*.md` files.
 *
 * Each thread file carries YAML frontmatter (id, title, status, …) and a body
 * with a `## Next step` section. This reads every `.fray/*.md` (except `_board.md`),
 * pulls `status` + the first line under `## Next step`, and prints a markdown table
 * (thread | status | next step) sorted by thread id. The table is DERIVED — never
 * hand-edit it in `_board.md`; edit the thread file and re-run.
 *
 * Usage:
 *   node scripts/fray/index.mjs            # print the table to stdout
 *   node scripts/fray/index.mjs --write    # splice it into .fray/_board.md
 *                                            between the FRAY:THREADS markers
 *
 * Anti-drift: `--write` rewrites only the region between
 *   <!-- FRAY:THREADS:START -->  …  <!-- FRAY:THREADS:END -->
 * leaving the rest of `_board.md` byte-untouched.
 */

import { readFileSync, writeFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const FRAY_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '.fray');
const BOARD = join(FRAY_DIR, '_board.md');
const START = '<!-- FRAY:THREADS:START -->';
const END = '<!-- FRAY:THREADS:END -->';

// Pull a top-of-file `--- … ---` YAML frontmatter block (flat key: value only).
function frontmatter(src) {
  const m = src.match(/^---\n([\s\S]*?)\n---/);
  if (!m) return {};
  const out = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^(\w[\w-]*):\s*(.*)$/);
    if (kv) out[kv[1]] = kv[2].trim().replace(/^["']|["']$/g, '');
  }
  return out;
}

// First non-blank line under `## Next step` (one row, collapsed to a cell).
function nextStep(src) {
  const lines = src.split('\n');
  const i = lines.findIndex(l => /^##\s+Next step\s*$/i.test(l));
  if (i === -1) return '';
  for (let j = i + 1; j < lines.length; j++) {
    if (/^#{1,6}\s/.test(lines[j])) break; // hit the next heading
    if (lines[j].trim()) return lines[j].trim();
  }
  return '';
}

const threads = readdirSync(FRAY_DIR)
  .filter(f => f.endsWith('.md') && f !== '_board.md')
  .sort()
  .map(f => {
    const src = readFileSync(join(FRAY_DIR, f), 'utf8');
    const fm = frontmatter(src);
    return { id: fm.id ?? f.replace(/\.md$/, ''), status: fm.status ?? '?', next: nextStep(src) };
  });

const header = `<!-- DERIVED by \`node scripts/fray/index.mjs --write\` from .fray/<thread>.md frontmatter + first line of each thread's \`## Next step\`. Do NOT hand-edit this table — edit the thread file, then re-derive. -->`;
const rows = threads.map(t => `| ${t.id} | ${t.status} | ${t.next} |`).join('\n');
const table = `${header}\n| thread | status | next step |\n|--------|--------|-----------|\n${rows}`;

if (process.argv.includes('--write')) {
  const board = readFileSync(BOARD, 'utf8');
  const re = new RegExp(`${START}[\\s\\S]*?${END}`);
  if (!re.test(board)) {
    console.error(`Markers ${START} / ${END} not found in ${BOARD}`);
    process.exit(1);
  }
  writeFileSync(BOARD, board.replace(re, `${START}\n${table}\n${END}`));
  console.log(`Wrote ${threads.length} thread rows into ${BOARD}`);
} else {
  console.log(table);
}
