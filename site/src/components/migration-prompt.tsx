'use client';

import { useState } from 'react';

/* The prompt copied to the clipboard. Points an AI coding agent at the
   agent-onboarding doc (https://nubjs.com/start.md) and carries the core
   directives so it acts before reading the doc in full. DRAFT wording —
   the maintainer owns the final voice. */
const PROMPT =
  'Read https://nubjs.com/start.md to learn nub’s full surface. Then install the nub skill: get its text by running `nub agent skill` (which just prints the skill body — it does not install anything) or by fetching https://nubjs.com/skill.md, then write it into wherever this project keeps agent skills or rules, following the project’s conventions (for example .claude/skills, .cursor/rules, or your agent’s equivalent). Installing the skill is the only thing to do on your own; make no other changes without my go-ahead. Next, in an education pass (no edits), study this project’s existing tooling — its dependencies, scripts, TypeScript runner, env loading, and version and package managers — and explain to me, above all, how nub fits into it and simplifies it: which tools nub consolidates or replaces here, and how and when to use it (run files with `nub <file>`, package.json scripts with `nub run`, local CLIs with `nubx`, plus `nub --node` for plain Node), and what nub removes from the setup. Then point out the dependencies or devDependencies nub makes redundant (for example tsx, ts-node, dotenv, nodemon, tsconfig-paths) and offer to migrate off them — but make no change until I opt in. There’s no need to switch off the existing package manager: nub respects its lockfile bidirectionally, so nub and the current package manager work interchangeably.';

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
