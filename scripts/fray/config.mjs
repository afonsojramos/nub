// @ts-check
/**
 * fray — the SHARED, type-safe config + vocab module. Every fray hook
 * (.claude/hooks/*.mjs) and the board tool (scripts/fray/index.mjs) import from
 * here, so there is exactly ONE source of truth for: the config schema + parse,
 * and the thread-status vocabulary.
 *
 * Dependency-free by design (no `yaml` package): Node ships no built-in YAML
 * parser, and fray must stay portable + runnable by bare `node` with zero install.
 * We hand-parse the SMALL, FLAT shape of `.fray/config.yml` (top-level scalars
 * plus the one nested `state:` block) — not a general YAML parser, just enough.
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

/**
 * The thread-status vocabulary.
 * - `todo` — not started; no agent dispatched, nothing blocking it.
 * - `enqueued` — READY to run (work fully scoped + decided) but deliberately held
 *   until a NAMED in-flight agent/thread completes — a sequencing dependency
 *   (same-file serialization, or it needs the prior agent's output). Distinct from
 *   `blocked`: an `enqueued` thread has a concrete auto-trigger (agent X returns →
 *   dispatch it), it is NOT waiting on a human/decision. The thread's `## Next step`
 *   must name the agent/thread it is waiting on. PREFER messaging the in-flight
 *   agent to fold the work in over enqueuing-then-dispatching, when the work fits
 *   that agent's scope (see the fray skill — steer-in-flight beats spawn-fresh).
 * - `blocked` — cannot proceed; waiting on a human decision, an answer, or an
 *   external event with no in-session auto-trigger.
 * - `needs-decision` — surfaced a question the human owns; recommend-only until answered.
 * - `planned` — DECIDED to do, but deliberately deferred (not immediate). Not blocked,
 *   not awaiting a decision, not ready-to-fire — a committed-to backlog item with a
 *   written plan, to pick up in a later cycle. Distinct from `todo` (which is "could
 *   start now, just hasn't") and `needs-decision` (which is gated on a human call).
 * - `done` / `dismissed` — TERMINAL (completed / decided-against): kept, never
 *   deleted, excluded from the active board's pending views.
 * @type {readonly string[]}
 */
export const STATUS = ['todo', 'planned', 'enqueued', 'active', 'blocked', 'needs-decision', 'done', 'dismissed'];

/**
 * The terminal subset of {@link STATUS}: completed OR decided-against. Both are
 * kept on disk and both are excluded from the pending/board views.
 * @type {readonly string[]}
 */
export const TERMINAL = ['done', 'dismissed'];

/**
 * @typedef {Object} FrayConfig
 * @property {boolean} enabled       Master kill-switch. `false` makes all fray hooks no-op. Default `true` (fail-safe — a botched config never silently disables orchestration).
 * @property {boolean} autonomousMode  Whether autonomous mode is on. Default `false`.
 * @property {Record<string, string>} state  The `state:` block — cross-cutting "what's true now" globals. Default `{}`.
 */

/**
 * The type-safe DEFAULTS, returned when `.fray/config.yml` is absent. Individual
 * malformed lines are simply skipped (we keep whatever parsed), so a partially
 * broken file still yields a fully-populated config.
 * @returns {FrayConfig}
 */
function defaults() {
  return { enabled: true, autonomousMode: false, state: {} };
}

/**
 * Coerce a YAML-ish scalar to a boolean. Accepts the YAML 1.1 truthy/falsey
 * spellings fray actually uses (`true`/`false`, `on`/`off`, `yes`/`no`).
 * Anything else returns `fallback` so an unparseable value can't flip a default.
 * @param {string} raw
 * @param {boolean} fallback
 * @returns {boolean}
 */
function toBool(raw, fallback) {
  const v = raw.trim().toLowerCase();
  if (v === 'true' || v === 'on' || v === 'yes') return true;
  if (v === 'false' || v === 'off' || v === 'no') return false;
  return fallback;
}

/**
 * Strip surrounding single/double quotes and trailing inline `# …` comments.
 * @param {string} raw
 * @returns {string}
 */
function scalar(raw) {
  // Drop an inline comment only when the `#` is preceded by whitespace (so a `#`
  // inside a quoted value or a bare token isn't clobbered). Then trim + unquote.
  let v = raw.replace(/\s+#.*$/, '').trim();
  return v.replace(/^["']|["']$/g, '');
}

/**
 * Read + parse `.fray/config.yml` from `projectDir` into a fully-populated,
 * type-safe {@link FrayConfig}. The file is absent/unreadable → DEFAULTS.
 * A single malformed line → that line is skipped; everything else still parses.
 *
 * Parser shape (intentionally narrow — matches fray's flat config, NOT general YAML):
 *   - `key: value`         top-level scalar (e.g. `enabled: true`, `autonomous_mode: off`)
 *   - `state:`             opens the one nested block
 *     `  key: "value"`     two-space-indented entries become `state[key] = value`
 *   - `# …` lines + blanks are ignored.
 *
 * @param {string} projectDir  The repo root (e.g. `process.env.CLAUDE_PROJECT_DIR`).
 * @returns {FrayConfig}
 */
export function loadConfig(projectDir) {
  const cfg = defaults();
  let src;
  try {
    src = readFileSync(join(projectDir, '.fray', 'config.yml'), 'utf8');
  } catch {
    return cfg; // absent / unreadable → type-safe defaults
  }

  let inState = false;
  for (const line of src.split('\n')) {
    if (!line.trim() || line.trim().startsWith('#')) continue; // blank / comment

    // A nested `state:` entry: two-or-more leading spaces + `key: value`.
    const nested = line.match(/^[ \t]+([\w-]+):\s*(.*)$/);
    if (inState && nested) {
      cfg.state[nested[1]] = scalar(nested[2]);
      continue;
    }

    // A top-level `key: value` (or bare `key:` opening a block).
    const top = line.match(/^([\w-]+):\s*(.*)$/);
    if (!top) continue; // malformed → skip this line, keep parsing

    const key = top[1];
    const val = top[2];

    if (key === 'state') {
      inState = true; // open the nested block; `val` is empty for `state:`
      continue;
    }
    inState = false; // any other top-level key closes the state block

    // scalar() FIRST — strip any trailing inline `# …` comment before coercing,
    // else `autonomous_mode: on  # note` reads as garbage → silently falls back to
    // the default. (Bug found 2026-06-14: an inline comment flipped autonomous mode
    // back off. The nested `state:` entries already go through scalar(); the
    // top-level bools must too.)
    if (key === 'enabled') cfg.enabled = toBool(scalar(val), cfg.enabled);
    else if (key === 'autonomous_mode') cfg.autonomousMode = toBool(scalar(val), cfg.autonomousMode);
    // unrecognized top-level keys are ignored by design (forward-compatible)
  }

  return cfg;
}
