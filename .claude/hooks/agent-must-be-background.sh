#!/usr/bin/env bash
# PreToolUse hook on the `Agent` tool — ENFORCES the Interactive-Workflow rule
# that sub-agents are ALWAYS dispatched with run_in_background:true, never
# foreground/blocking. A foreground agent blocks the orchestrator turn, and a
# human interjection mid-run orphans its work. Documentation kept failing to
# change the behavior, so this denies the call outright (2026-06-13).
#
# Allow  → exit 0, no output.
# Deny   → emit PreToolUse permissionDecision=deny with a re-send reminder.
set -euo pipefail
in=$(cat)
bg=$(printf '%s' "$in" | jq -r '.tool_input.run_in_background // false')
if [ "$bg" = "true" ]; then
  exit 0
fi
printf '%s' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"fray mode (hook-enforced): Agent sub-agents MUST be dispatched with run_in_background:true — never foreground/blocking. A foreground agent blocks the orchestrator turn and a human interjection orphans its work. Re-send this Agent call with run_in_background:true."}}'
