'use client';

import { useState } from 'react';

/* The prompt copied to the clipboard. Points an AI coding agent at the
   agent-onboarding doc (served at https://nubjs.com/start.md) and tells it to
   adopt nub in the current project. Kept to two sentences — DRAFT wording,
   the maintainer owns the final voice. */
const PROMPT =
  'Read https://nubjs.com/start.md and adopt nub in this Node.js project — use it to run files, scripts, and local CLIs, and as the package manager. Set things up freely, but ask me before refactoring (removing deps or rewriting package.json scripts).';

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
      <span className="text-fd-foreground">
        {copied ? 'Copied — paste into your agent' : 'Copy agent prompt'}
      </span>
      <span className="inline-flex w-4 shrink-0 justify-center text-fd-muted-foreground group-hover:text-ember">
        {copied ? (
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
            <path d="M20 6 9 17l-5-5" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        ) : (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" aria-hidden>
            <rect x="9" y="9" width="11" height="11" rx="2" stroke="currentColor" strokeWidth="2" />
            <path d="M5 15V5a2 2 0 0 1 2-2h10" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
          </svg>
        )}
      </span>
    </button>
  );
}
