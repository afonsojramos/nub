#!/usr/bin/env bash
# UserPromptSubmit hook — injects a per-turn orchestrator reminder so the
# Interactive-Workflow discipline doesn't decay across long sessions. the maintainer
# added this (2026-06-14) because (a) the epic todo.md keeps going stale and
# failing as persistent memory — work falls out of context and is lost — and
# (b) the orchestrator keeps doing project work (GitHub writes, edits, builds)
# in the foreground instead of delegating to sub-agents.
#
# It emits `additionalContext` (model-only; the human never sees it) on every
# user/heartbeat turn. Phrased conditionally so it's a no-op nudge when not in
# IW mode.
cat <<'JSON'
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"⟦orchestrator reminder⟧ If you are operating in the Interactive Workflow / autonomous mode: (1) UPDATE the epic tracker (epics/*/todo.md) THIS turn — fold every returned sub-agent's facts into it, advance statuses, log open questions/decisions. It is your persistent memory; a stale tracker = work lost when context drops. (2) You are the ORCHESTRATOR — delegate ALL project work (code edits, GitHub writes like comments/PR edits/resolves, builds, tests, investigations) to BACKGROUND sub-agents; do not do them yourself in the foreground. Your foreground is: dispatch, synthesize returns, decide, and edit your own control surfaces (tracker/memory/skill/settings). (3) Reconcile EVERY in-flight sub-agent before moving on — never drop a thread."}}
JSON
