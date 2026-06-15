#!/usr/bin/env node
// @ts-check
/**
 * Codex-facing Fray pulse. Run this at the start of a Fray turn/checkpoint to
 * surface pending threads, validation errors, and the reconciliation reminder.
 */

import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadConfig, STATUS, TERMINAL } from '../../../../scripts/fray/config.mjs';

const PROJECT_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', '..');
const FRAY_DIR = join(PROJECT_DIR, '.fray');
const asJson = process.argv.includes('--json');

/**
 * @param {string} path
 * @returns {string}
 */
function read(path) {
  try {
    return readFileSync(path, 'utf8');
  } catch {
    return '';
  }
}

const cfg = loadConfig(PROJECT_DIR);
const pending = [];
const errors = [];

try {
  for (const file of readdirSync(FRAY_DIR).sort()) {
    if (!file.endsWith('.md') || file.startsWith('_')) continue;
    const slug = file.replace(/\.md$/, '');
    const src = read(join(FRAY_DIR, file));
    const title = src.match(/^title:\s*(.+)$/m)?.[1]?.trim() ?? '';
    const status = src.match(/^status:\s*(\S+)/m)?.[1] ?? '';
    const next = (() => {
      const lines = src.split('\n');
      const i = lines.findIndex((line) => /^##\s+Next step\s*$/i.test(line));
      if (i === -1) return '';
      for (let j = i + 1; j < lines.length; j++) {
        if (/^#{1,6}\s/.test(lines[j])) break;
        if (lines[j].trim()) return lines[j].trim();
      }
      return '';
    })();

    if (!title) errors.push(`${slug}: missing title`);
    if (!status) errors.push(`${slug}: missing status`);
    else if (!STATUS.includes(status)) errors.push(`${slug}: invalid status "${status}"`);

    if (!TERMINAL.includes(status)) pending.push({ slug, title, status: status || '?', next });
  }
} catch {
  errors.push('missing or unreadable .fray directory');
}

const payload = {
  enabled: cfg.enabled,
  autonomous_mode: cfg.autonomousMode,
  pending,
  errors,
};

if (asJson) {
  process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
  process.exit(errors.length ? 1 : 0);
}

if (cfg.enabled === false) {
  console.log('FRAY disabled: .fray/config.yml has enabled: false');
  process.exit(0);
}

console.log(
  `FRAY pulse for Codex: autonomous_mode=${cfg.autonomousMode ? 'on' : 'off'}; ${pending.length} pending thread(s).`,
);
if (pending.length) {
  for (const item of pending) {
    console.log(`- ${item.slug} [${item.status}] - ${item.title}`);
    if (item.next) console.log(`  next: ${item.next}`);
  }
}
if (errors.length) {
  console.log(`VALIDATION ERRORS: ${errors.join('; ')}`);
  process.exit(1);
}

console.log(
  'Reminder: reconcile returned agents first; update Status/Decisions/Open questions/Steps/Next step; use node scripts/fray/index.mjs for the board.',
);
