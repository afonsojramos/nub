'use client';

import { useState } from 'react';

/* The prompt copied to the clipboard. People read this before running it, so
   it's formatted as a numbered checklist (newlines survive the copy). It points
   an AI coding agent at the onboarding doc, installs the skill, then runs an
   education pass before any opt-in migration. */
const PROMPT = [
  'Adopt Nub (https://nubjs.com) in this Node.js project by working through these steps:',
  '',
  '1. Read https://nubjs.com/start.md to learn Nub’s full surface. Read the helptext (`-h`) for specific commands as needed. The full docs are at https://nubjs.com/llms.txt.',
  '',
  '2. Install the Nub skill. Run `nub agent skill` to print the skill body (it only prints it — it does not install anything), or fetch https://nubjs.com/skill.md. Then write it into every skills or rules location this project uses — for example .claude/skills/ for Claude Code and .cursor/rules/ for Cursor — covering all of them, not just one. Installing the skill is the only step to take unprompted; everything after it is opt-in, so pause for approval before making any change.',
  '',
  '3. Investigate the current project. Concisely explain how Nub would simplify the project’s dev dependencies and improve developer experience. Highlight any dev dependencies that Nub makes redundant (for example tsx, ts-node, dotenv, nodemon, or tsconfig-paths). In an education pass with no edits, study this project’s existing tooling — its dependencies, scripts, TypeScript runner, env loading, and version and package managers — then lay out, above all, how Nub fits into it and simplifies it: which tools it consolidates or replaces here, what it removes from the setup, and how and when to use it (`nub <file>` to run files, `nub run` for package.json scripts, `nubx` for local CLIs, and `nub --node` for plain Node).',
  '',
  '4. Offer to migrate off of the dependencies or devDependencies Nub makes redundant (for example tsx, ts-node, dotenv, nodemon, or tsconfig-paths) and offer to migrate off them — but make no change without explicit approval.',
  '',
  'There’s no need to switch off the existing package manager: Nub respects its lockfile bidirectionally, so Nub and the current package manager work interchangeably.',
].join('\n');

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
      className={`group inline-flex items-center gap-1.5 text-sm leading-none text-fd-muted-foreground hover:text-ember ${className}`}
    >
      <span className="inline-flex h-4 w-4 shrink-0 -translate-y-px items-center justify-center">
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
      <span className="underline-offset-4 group-hover:underline">Copy agent prompt</span>
    </button>
  );
}
