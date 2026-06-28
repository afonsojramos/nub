#!/usr/bin/env node
// @ts-check
// fray-notify-surface (SELF-CONTAINED project-local copy) — Stop hook that surfaces the
// durable notification queue (`.fray/notify-queue.jsonl`) to the HUMAN when the orchestrator
// goes idle. This is a project-local mirror of the fray plugin's hook; it inlines the render
// (no plugin import) so it works regardless of where the plugin lives. The plugin's
// versioned copy is canonical — once a plugin reload picks up fray ≥1.14.0, this dup can be
// removed (the two coexist safely via the shared `surfaced` flag).
//
// Behavior: NON-BLOCKING. `systemMessage` shows to the user even on exit 0 without a block,
// so we surface the queue WITHOUT forcing the orchestrator into another turn (which is what
// stops it from re-typing the items back). Each open item is shown exactly once (stamp
// `surfaced:true`), then quiet until dismissed. Any error → allow the stop.
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

function allow() {
  process.exit(0);
}

const SECTIONS = [
  ['BLOCKER', 'Blockers'],
  ['DECISION', 'Decisions'],
  ['WIN', 'Wins'],
  ['FYI', 'FYI'],
];

function titleOf(i) {
  if (i.title) return String(i.title).trim();
  const t = String(i.text || '').trim();
  return (t.split(/ — | – |: |\. /)[0] || t).slice(0, 90).trim();
}
function bodyOf(i) {
  if (i.body != null) return String(i.body).trim();
  if (i.title) return '';
  const t = String(i.text || '').trim();
  const ti = titleOf(i);
  return t.startsWith(ti) ? t.slice(ti.length).replace(/^[\s—–:.-]+/, '').trim() : t;
}
function renderMarkdown(open) {
  const n = open.length;
  const parts = [
    `📌 ${n} item${n === 1 ? '' : 's'} waiting on you — surfaced here so they don't scroll away. ` +
      `Nothing to run in a terminal: just tell me your call in chat and I'll clear each one.`,
  ];
  for (const [kind, heading] of SECTIONS) {
    const rows = open.filter((i) => i.kind === kind);
    if (!rows.length) continue;
    parts.push(`\n## ${heading}`);
    for (const i of rows) {
      let block = `\n### ${titleOf(i)} · ${i.id}`;
      const body = bodyOf(i);
      if (body) block += `\n\n${body}`;
      parts.push(block);
    }
  }
  return parts.join('\n');
}

try {
  const root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  const queue = join(root, '.fray', 'notify-queue.jsonl');
  if (!existsSync(queue)) allow();

  const items = readFileSync(queue, 'utf8')
    .split('\n')
    .filter((l) => l.trim())
    .map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    })
    .filter(Boolean);

  const open = items.filter((i) => i.status === 'open');
  if (!open.length) allow();
  if (!open.some((i) => !i.surfaced)) allow(); // already shown once; quiet until dismissed

  for (const i of items) if (i.status === 'open' && !i.surfaced) i.surfaced = true;
  writeFileSync(queue, items.map((i) => JSON.stringify(i)).join('\n') + '\n');

  process.stdout.write(JSON.stringify({ systemMessage: renderMarkdown(open) }) + '\n');
  process.exit(0);
} catch {
  allow();
}
