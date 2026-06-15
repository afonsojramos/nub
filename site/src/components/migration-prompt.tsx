'use client';

import { useState } from 'react';

/* The prompt copied to the clipboard. Points an AI coding agent at the
   agent-onboarding doc (https://nubjs.com/start.md) and carries the core
   directives so it acts before reading the doc in full. DRAFT wording —
   the maintainer owns the final voice. */
const PROMPT =
  'Read https://nubjs.com/start.md and learn nub’s full surface first. Then, without asking, install the nub skill (run `nub agent skill`, or fetch https://nubjs.com/skill.md) into wherever this project keeps agent skills or rules, following the project’s own conventions — for example .claude/skills, .cursor/rules, or your agent’s equivalent — putting it everywhere it should go. After the skill is installed, ASK me before anything more invasive: adding a nub note to AGENTS.md, migrating off tsx / dotenv / bun, or replacing hardcoded package-manager references in package.json scripts (for example `pnpm run` → `nub run`). Going forward, prefer nub over node, bun, and the existing package manager: run files with `nub <file>`, run scripts with `nub run`, run local CLIs with `nubx`, and manage packages with `nub install` / `nub add` — nub respects the existing lockfile bidirectionally, so that is safe. Reach for `nub --node` when you need plain, unaugmented Node.';

export function MigrationPrompt({ className = '' }: { className?: string }) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(PROMPT);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      /* clipboard unavailable */
    }
  }

  return (
    <button
      type="button"
      onClick={copy}
      aria-label="Copy agent prompt"
      className={`group inline-flex items-center gap-2.5 rounded-full border border-fd-border bg-fd-card/60 py-2 pl-4 pr-3.5 text-sm leading-none text-fd-muted-foreground backdrop-blur hover:border-ember/50 hover:bg-ember/[0.06] ${className}`}
    >
      <span className="text-fd-foreground">Copy agent prompt</span>
      <span className="inline-flex h-4 w-4 shrink-0 items-center justify-center text-fd-muted-foreground group-hover:text-ember">
        {copied ? (
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
            <path d="M20 6 9 17l-5-5" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        ) : (
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
            <rect x="9" y="9" width="11" height="11" rx="2" stroke="currentColor" strokeWidth="2" />
            <path d="M5 15V5a2 2 0 0 1 2-2h10" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
          </svg>
        )}
      </span>
    </button>
  );
}
