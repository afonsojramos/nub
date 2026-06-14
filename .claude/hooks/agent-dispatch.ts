// PreToolUse hook on the `Agent` tool. Two jobs in one place:
//   1) ENFORCE background dispatch — deny any Agent call lacking run_in_background:true
//      (a foreground agent blocks the orchestrator turn; a human interjection orphans it).
//   2) AUTO-APPEND an ORCHESTRATION EPILOGUE to every backgrounded sub-agent's prompt, so
//      sub-agents always hand back the next links in the chain (follow-ups / self-review /
//      push-to-CI / next-step). This is the multi-agent chaining pattern (the maintainer, 2026-06-13:
//      "you often lose the role of a given sub-agent in a broader implementational plan").
// Run via nub (dogfood). Supersedes agent-must-be-background.sh.
// FAIL OPEN: any parse error → allow unmodified. A broken dispatch hook must never halt
// orchestration (the overnight heartbeat itself dispatches through here).
import { readFileSync, appendFileSync } from "node:fs";

const EPILOGUE = `

---
[ORCHESTRATION EPILOGUE — auto-appended by the dispatch hook] End your final report with a \`## Follow-ups\` section so the orchestrator can chain the next steps:
1. Concrete FOLLOW-UP work your findings/changes imply.
2. If you implemented something substantial → recommend a SELF-REVIEW pass (a fresh adversarial sub-agent reviewing your diff for correctness/regressions).
3. If you added/changed code or tests CI should exercise → recommend cutting a push to \`main\` + a CI-watch follow-up to confirm green.
4. The single most important NEXT STEP, and whether it needs the maintainer (a default/security/product/brand/API-config-env call → recommend-only) or can proceed autonomously.
If you COMMITTED: verify the tree COMPILES at your commit (a parallel agent may share a file — build before committing so you don't ship a broken HEAD). If there are no follow-ups, write "Follow-ups: none."`;

function emit(obj: unknown): never {
  process.stdout.write(JSON.stringify(obj));
  process.exit(0);
}

try {
  const input = JSON.parse(readFileSync(0, "utf8"));
  const ti = input.tool_input ?? {};

  if (ti.run_in_background !== true) {
    emit({
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason:
          "IW mode (hook-enforced): Agent sub-agents MUST be dispatched with run_in_background:true — never foreground/blocking. A foreground agent blocks the orchestrator turn and a human interjection orphans its work. Re-send this Agent call with run_in_background:true.",
      },
    });
  }

  const prompt = typeof ti.prompt === "string" ? ti.prompt : "";

  // fray pointer-back: if the dispatch names a THREAD (a `THREAD: <name>` line the
  // orchestrator puts at the top of the prompt), log it to the dispatch ledger so the
  // orchestrator has a durable record of which thread each agent serves — survives
  // compaction. Fail open: a ledger error must never block a dispatch.
  const m = prompt.match(/^THREAD:\s*([\w./-]+)/m);
  const thread = m ? m[1].replace(/^\.fray\//, "").replace(/\.md$/, "") : null;
  if (thread) {
    try {
      appendFileSync(
        `${process.env.CLAUDE_PROJECT_DIR ?? "."}/.fray/.dispatch-ledger.jsonl`,
        JSON.stringify({ ts: new Date().toISOString(), agent_type: ti.subagent_type ?? "", thread, reconciled: false }) + "\n",
      );
    } catch {
      /* fail open — never block a dispatch on the ledger */
    }
  }

  const updatedInput = prompt.includes("[ORCHESTRATION EPILOGUE")
    ? ti
    : { ...ti, prompt: prompt + EPILOGUE };

  emit({
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "allow",
      updatedInput,
    },
  });
} catch {
  emit({}); // fail open — allow unmodified
}
