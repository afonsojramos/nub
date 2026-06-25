#!/usr/bin/env node
// Decisions view, DERIVED from fray threads (no static store). Scans .fray/*.md,
// selects threads with `status: needs-decision`, and prints each thread's slug +
// its FULL statusText (the decision write-up) — the rich inline-reading view that
// complements the one-line-per-thread statusline (scripts/statusline-decisions.sh).
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const frayDir = join(root, '.fray');

const STATUS_TEXT_KEYS = ['statusText', 'status_text'];

// Parse the leading `---` frontmatter block into a flat map. Only single-line
// `key: value` pairs are read (the thread frontmatter is flat scalars + a list).
function parseFrontmatter(text) {
  const lines = text.split('\n');
  if (lines[0] !== '---') return null;
  const fm = {};
  for (let i = 1; i < lines.length; i++) {
    if (lines[i] === '---') return fm;
    const m = lines[i].match(/^([\w-]+):\s*(.*)$/);
    if (m) fm[m[1]] = m[2];
  }
  return null; // unterminated frontmatter
}

function unquote(raw) {
  if (raw === undefined) return '';
  let v = raw.trim();
  const m = v.match(/^"((?:[^"\\]|\\.)*)"$/);
  if (m) v = m[1].replace(/\\(.)/g, '$1');
  return v;
}

export function collectDecisions() {
  let files;
  try {
    files = readdirSync(frayDir).filter((f) => f.endsWith('.md'));
  } catch {
    return [];
  }
  const out = [];
  for (const f of files.sort()) {
    let text;
    try {
      text = readFileSync(join(frayDir, f), 'utf8');
    } catch {
      continue;
    }
    const fm = parseFrontmatter(text);
    if (!fm || fm.status !== 'needs-decision') continue;
    const rawText = STATUS_TEXT_KEYS.map((k) => fm[k]).find((v) => v !== undefined);
    out.push({ slug: basename(f, '.md'), statusText: unquote(rawText) });
  }
  return out;
}

function main() {
  const items = collectDecisions();
  if (items.length === 0) {
    console.log('✓ no pending decisions');
    return;
  }
  console.log(`⚖ ${items.length} decision(s) awaiting you:\n`);
  items.forEach((d, i) => {
    console.log(`[${d.slug}]`);
    console.log(d.statusText || '(no statusText written up)');
    if (i < items.length - 1) console.log('');
  });
}

// Run only when invoked directly (it's also imported by other scripts).
if (process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url)) main();
