#!/usr/bin/env bash
# Statusline: glanceable LIST of decisions awaiting the maintainer.
# Reads .fray/decisions.md (agent-maintained), prints a header + one row per
# decision. Claude Code statuslines render each printed line as a separate row.
# Only "- " lines count; frontmatter (---), headings (#), comments (<!--) are skipped.
# Pure file read, no network — must stay fast. stdin JSON is ignored.
set -euo pipefail

# Resolve repo root relative to this script so the statusline works from any cwd.
dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
file="$dir/.fray/decisions.md"

# Drain stdin (Claude pipes session JSON in) without blocking.
cat >/dev/null 2>&1 || true

if [ ! -s "$file" ]; then
  printf '✓ no pending decisions\n'
  exit 0
fi

width="${COLUMNS:-100}"
[ "$width" -ge 24 ] 2>/dev/null || width=100

# Decision lines only: start with "- ", with the leading marker stripped.
decisions=$(grep '^- ' "$file" 2>/dev/null | sed -E 's/^- //') || true

if [ -z "$decisions" ]; then
  printf '✓ no pending decisions\n'
  exit 0
fi

n=$(printf '%s\n' "$decisions" | grep -c .)

printf '⚖ %s decision(s) awaiting you:\n' "$n"

cap=10
printf '%s\n' "$decisions" | awk -v w="$width" -v cap="$cap" -v total="$n" '
  NR <= cap {
    line = " • " $0
    if (length(line) > w) line = substr(line, 1, w - 1) "…"
    print line
  }
  END {
    if (total > cap) printf " …(+%d more — see .fray/decisions.md)\n", total - cap
  }
'
