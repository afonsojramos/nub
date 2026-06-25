#!/usr/bin/env node
// Structured fray-thread updater. A superset of the single-op Edit tool: structured
// frontmatter inputs (--status/--status-text/--set) + a multi-patch body editor
// (--patch, repeatable) + --append. Writes .fray/<slug>.md atomically, preserving
// every byte outside the keys/regions it touches.
//
// Enforced invariant: setting `status: needs-decision` REQUIRES a non-empty statusText
// (the decision write-up) — supplied via --status-text or already present on the thread.
// The decisions queue DERIVES from these statusText fields (see statusline-decisions.sh),
// so a needs-decision thread without a write-up would surface as an empty queue row.
import { readFileSync, writeFileSync, existsSync, renameSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const STATUSES = ['todo', 'planned', 'enqueued', 'active', 'blocked', 'needs-decision', 'done', 'dismissed'];
const PATCH_SEP = '===>>';
// Both keys exist in the wild (statusText: 40, status_text: 139). We WRITE the
// canonical `statusText`, but match/replace either when present on a thread.
const STATUS_TEXT_KEYS = ['statusText', 'status_text'];

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

function die(msg) {
  console.error(`error: ${msg}`);
  process.exit(1);
}

const usage = `usage: node scripts/fray/thread-update.mjs <slug> [options]
  --status <s>              set status; one of: ${STATUSES.join(' · ')}
                            (needs-decision REQUIRES a statusText write-up)
  --status-text "<text>"    set the statusText field (decision write-up / gloss)
  --set key=value           set any other frontmatter scalar (repeatable)
  --patch "<find>${PATCH_SEP}<replace>"  body find/replace, must match EXACTLY once (repeatable, applied in order, atomic)
  --append "<text>"         append text to the body`;

function parseArgs(argv) {
  const out = { slug: undefined, status: undefined, statusText: undefined, sets: [], patches: [], appends: [] };
  let i = 0;
  for (; i < argv.length; i++) {
    const a = argv[i];
    const needVal = (name) => {
      if (i + 1 >= argv.length) die(`${name} requires a value`);
      return argv[++i];
    };
    switch (a) {
      case '--status': out.status = needVal('--status'); break;
      case '--status-text': out.statusText = needVal('--status-text'); break;
      case '--set': out.sets.push(needVal('--set')); break;
      case '--patch': out.patches.push(needVal('--patch')); break;
      case '--append': out.appends.push(needVal('--append')); break;
      case '-h': case '--help': console.log(usage); process.exit(0); break;
      default:
        if (a.startsWith('-')) die(`unknown flag: ${a}`);
        if (out.slug !== undefined) die(`unexpected positional: ${a} (slug already set to "${out.slug}")`);
        out.slug = a;
    }
  }
  return out;
}

function today() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

// Quote a frontmatter scalar value the way the existing threads do: double-quoted
// with inner double-quotes escaped. Bare safe scalars (the empty-bracket list,
// plain dates/words) are left unquoted to match the on-disk convention.
function quoteValue(v) {
  if (/^\[.*\]$/.test(v.trim())) return v.trim(); // list literal, e.g. depends_on: []
  if (/^[\w./#:+-]+$/.test(v)) return v; // bare safe scalar (date, single word, slug)
  return `"${v.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
}

// Split the file into [frontmatter-lines, body-string]. Frontmatter is the block
// between the leading `---` and the next `---`. Returns null fm if absent.
function splitFrontmatter(text) {
  const lines = text.split('\n');
  if (lines[0] !== '---') return { fm: null, fmEnd: 0, body: text };
  let end = -1;
  for (let i = 1; i < lines.length; i++) {
    if (lines[i] === '---') { end = i; break; }
  }
  if (end === -1) return { fm: null, fmEnd: 0, body: text };
  const fm = lines.slice(1, end);
  const body = lines.slice(end + 1).join('\n');
  return { fm, fmEnd: end, body };
}

function fmGet(fm, key) {
  const re = new RegExp(`^${key}:\\s*(.*)$`);
  for (const line of fm) {
    const m = line.match(re);
    if (m) return m[1];
  }
  return undefined;
}

// Set a frontmatter key in place (preserving line order); append if absent.
function fmSet(fm, key, rawValue) {
  const value = quoteValue(rawValue);
  const re = new RegExp(`^${key}:\\s*`);
  for (let i = 0; i < fm.length; i++) {
    if (re.test(fm[i])) { fm[i] = `${key}: ${value}`; return fm; }
  }
  fm.push(`${key}: ${value}`);
  return fm;
}

// Set statusText into whichever key the thread already uses; default to canonical
// `statusText` for a thread that has neither.
function setStatusText(fm, rawValue) {
  for (const k of STATUS_TEXT_KEYS) {
    if (fmGet(fm, k) !== undefined) return fmSet(fm, k, rawValue);
  }
  return fmSet(fm, 'statusText', rawValue);
}

function getStatusText(fm) {
  for (const k of STATUS_TEXT_KEYS) {
    const v = fmGet(fm, k);
    if (v !== undefined) return v;
  }
  return undefined;
}

// A statusText value counts as "present" only if it's a non-empty string.
function statusTextNonEmpty(raw) {
  if (raw === undefined) return false;
  let v = raw.trim();
  const m = v.match(/^"((?:[^"\\]|\\.)*)"$/);
  if (m) v = m[1].replace(/\\(.)/g, '$1');
  return v.trim().length > 0;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!args.slug) { console.log(usage); process.exit(args.slug === undefined && process.argv.length <= 2 ? 0 : 1); }

  const path = join(root, '.fray', `${args.slug}.md`);
  if (!existsSync(path)) die(`no thread at ${path}`);

  const original = readFileSync(path, 'utf8');
  const { fm, body } = splitFrontmatter(original);
  if (fm === null) die(`thread ${args.slug}.md has no YAML frontmatter block`);

  // --- Validate status + the needs-decision/statusText invariant BEFORE writing ---
  if (args.status !== undefined && !STATUSES.includes(args.status)) {
    die(`invalid status "${args.status}"; must be one of: ${STATUSES.join(' · ')}`);
  }
  const effectiveStatus = args.status ?? undefined;
  if (effectiveStatus === 'needs-decision') {
    const willHaveStatusText = args.statusText !== undefined
      ? statusTextNonEmpty(`"${args.statusText}"`) || args.statusText.trim().length > 0
      : statusTextNonEmpty(getStatusText(fm));
    if (!willHaveStatusText) {
      die('status needs-decision REQUIRES a decision write-up — pass --status-text "<text>" (or set it on the thread first)');
    }
  }

  // --- Apply body patches (atomic: validate all, then apply) ---
  const patchOps = args.patches.map((p, idx) => {
    const sepAt = p.indexOf(PATCH_SEP);
    if (sepAt === -1) die(`--patch #${idx + 1} missing "${PATCH_SEP}" separator`);
    return { find: p.slice(0, sepAt), replace: p.slice(sepAt + PATCH_SEP.length), idx };
  });
  for (const op of patchOps) {
    if (op.find === '') die(`--patch #${op.idx + 1} has an empty find string`);
    const count = body.split(op.find).length - 1;
    if (count === 0) die(`--patch #${op.idx + 1} find string not found in body (no patches applied)`);
    if (count > 1) die(`--patch #${op.idx + 1} find string occurs ${count} times, must be unique (no patches applied)`);
  }
  let newBody = body;
  for (const op of patchOps) newBody = newBody.replace(op.find, op.replace);

  // --- Apply appends ---
  for (const text of args.appends) {
    const sep = newBody.endsWith('\n') ? '' : '\n';
    newBody = `${newBody}${sep}${text}${text.endsWith('\n') ? '' : '\n'}`;
  }

  // --- Apply frontmatter edits ---
  let lastUpdateExplicit = false;
  if (args.status !== undefined) fmSet(fm, 'status', args.status);
  if (args.statusText !== undefined) setStatusText(fm, args.statusText);
  for (const kv of args.sets) {
    const eq = kv.indexOf('=');
    if (eq === -1) die(`--set requires key=value (got "${kv}")`);
    const key = kv.slice(0, eq).trim();
    const value = kv.slice(eq + 1);
    if (!key) die(`--set has an empty key (got "${kv}")`);
    if (key === 'last_update') lastUpdateExplicit = true;
    fmSet(fm, key, value);
  }
  // Auto-stamp last_update unless explicitly set. Only when SOMETHING changed.
  const mutated = args.status !== undefined || args.statusText !== undefined || args.sets.length || patchOps.length || args.appends.length;
  if (mutated && !lastUpdateExplicit) fmSet(fm, 'last_update', today());

  // body is everything after the closing `---` line (its join started at end+1),
  // so it carries its own leading newline(s) — emit it verbatim after `---\n` to
  // preserve the exact blank-line layout byte-for-byte.
  const out = `---\n${fm.join('\n')}\n---\n${newBody}`;

  // Atomic write: temp file in the same dir, then rename.
  const tmp = `${path}.tmp.${process.pid}`;
  writeFileSync(tmp, out);
  renameSync(tmp, path);

  // --- Report ---
  const finalStatus = fmGet(fm, 'status') ?? '(unset)';
  const finalStatusText = getStatusText(fm);
  console.log(`updated .fray/${args.slug}.md`);
  console.log(`  status: ${finalStatus}`);
  if (finalStatusText !== undefined) {
    const display = finalStatusText.replace(/^"(.*)"$/, '$1');
    console.log(`  statusText: ${display.length > 120 ? display.slice(0, 117) + '…' : display}`);
  }
  if (patchOps.length) console.log(`  patches applied: ${patchOps.length}`);
  if (args.appends.length) console.log(`  appended: ${args.appends.length} block(s)`);
}

main();
