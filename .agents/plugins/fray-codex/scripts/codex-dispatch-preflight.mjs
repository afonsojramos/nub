#!/usr/bin/env node
// @ts-check
/**
 * Codex Fray dispatch preflight.
 *
 * Usage:
 *   printf '%s' "$PROMPT" | node .agents/plugins/fray-codex/scripts/codex-dispatch-preflight.mjs \
 *     --thread <slug> --agent-type <explorer|worker|default> --label "Plain English task"
 *   # Add --dry-run to emit the prompt without writing the ledger.
 *
 * It validates that the thread file exists, prefixes THREAD when needed,
 * adds a durable FRAY_DISPATCH_ID, appends the orchestration epilogue, records
 * the dispatch ledger, and emits the prompt to pass to multi_agent_v1.spawn_agent.
 */

import { appendFileSync, existsSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { loadConfig } from '../../../../scripts/fray/config.mjs';

const PROJECT_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', '..');

const EPILOGUE = `

---
[ORCHESTRATION EPILOGUE - appended by .agents/plugins/fray-codex/scripts/codex-dispatch-preflight.mjs]
End your final report with a \`## Follow-ups\` section so the orchestrator can chain the next steps:
1. Concrete follow-up work your findings or changes imply.
2. If you implemented something substantial, recommend a fresh adversarial self-review pass.
3. If you changed code or tests CI should exercise, recommend a push-to-main and CI-watch follow-up.
4. The single most important next step, and whether it needs the maintainer because it is a default/security/product/brand/API-config-env call.
If you committed: verify the tree compiles at your commit. If there are no follow-ups, write "Follow-ups: none."`;

/**
 * @param {string} name
 * @returns {string | null}
 */
function arg(name) {
  const i = process.argv.indexOf(name);
  return i === -1 ? null : process.argv[i + 1] ?? null;
}

const threadRaw = arg('--thread');
const agentType = arg('--agent-type') ?? '';
const label = arg('--label') ?? '';
const model = arg('--model') ?? '';
const reasoningEffort = arg('--reasoning-effort') ?? '';
const serviceTier = arg('--service-tier') ?? '';
const forkContext = arg('--fork-context');
const dispatchId = arg('--dispatch-id') ?? `fray-${new Date().toISOString().replace(/[-:.TZ]/g, '')}-${randomUUID().slice(0, 8)}`;
const dryRun = process.argv.includes('--dry-run');

if (!threadRaw) {
  console.error('codex-dispatch-preflight: --thread <slug> is required for thread-scoped dispatches');
  process.exit(2);
}

const thread = threadRaw.replace(/^\.fray\//, '').replace(/\.md$/, '');
const cfg = loadConfig(PROJECT_DIR);

if (cfg.enabled === false) {
  console.error('codex-dispatch-preflight: Fray is disabled in .fray/config.yml');
  process.exit(2);
}

const threadPath = join(PROJECT_DIR, '.fray', `${thread}.md`);
if (!existsSync(threadPath)) {
  console.error(
    `codex-dispatch-preflight: .fray/${thread}.md does not exist. Create the thread file before dispatching.`,
  );
  process.exit(1);
}

let prompt = '';
try {
  prompt = readFileSync(0, 'utf8');
} catch {
  prompt = '';
}

if (!prompt.trim()) {
  console.error('codex-dispatch-preflight: prompt on stdin is empty');
  process.exit(2);
}

if (!new RegExp(`^THREAD:\\s*${thread.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*$`, 'm').test(prompt)) {
  prompt = `THREAD: ${thread}\n\n${prompt}`;
}

if (!new RegExp(`^FRAY_DISPATCH_ID:\\s*${dispatchId.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*$`, 'm').test(prompt)) {
  prompt = prompt.replace(/^(THREAD:\s*.+)$/m, `$1\nFRAY_DISPATCH_ID: ${dispatchId}`);
}

if (!prompt.includes('[ORCHESTRATION EPILOGUE')) {
  prompt += EPILOGUE;
}

if (!dryRun) {
  appendFileSync(
    join(PROJECT_DIR, '.fray', '.dispatch-ledger.jsonl'),
    `${JSON.stringify({
      ts: new Date().toISOString(),
      tool: 'codex.spawn_agent',
      dispatch_id: dispatchId,
      agent_type: agentType,
      label,
      thread,
      model,
      reasoning_effort: reasoningEffort,
      service_tier: serviceTier,
      fork_context: forkContext,
      agent_id: '',
      nickname: '',
      reconciled: false,
    })}\n`,
  );
}

process.stdout.write(prompt.endsWith('\n') ? prompt : `${prompt}\n`);
