'use client';

import { useState, type ReactNode } from 'react';

/* Copy/checkmark icons cribbed from migration-prompt.tsx / install-tabs.tsx so the
   interaction reads identically across the site. */
function CopyIcon({ copied }: { copied: boolean }) {
  return copied ? (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
      <path d="M20 6 9 17l-5-5" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ) : (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" aria-hidden>
      <rect x="9" y="9" width="11" height="11" rx="2" stroke="currentColor" strokeWidth="2" />
      <path d="M5 15V5a2 2 0 0 1 2-2h10" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
    </svg>
  );
}

const DEFAULT_INSTALL = 'npm i -D @nubjs/types @types/node@26';
const DEFAULT_TSCONFIG = `{ "compilerOptions": { "types": ["node", "@nubjs/types"] } }`;

export function TypesSetup({
  install = DEFAULT_INSTALL,
  tsconfig = DEFAULT_TSCONFIG,
  children,
}: {
  /** The install command shown + copied. */
  install?: string;
  /** The tsconfig.json snippet shown + copied. */
  tsconfig?: string;
  /** Page-specific one-liner: which types come from where. */
  children?: ReactNode;
}) {
  const [copied, setCopied] = useState(false);

  /* Agent-ready directive: a short, pasteable instruction the user drops into
     their AI coding agent. Plain imperative prose + the two snippets. */
  const prompt = [
    'Set up TypeScript types for nub in this project:',
    '',
    `1. Install the type packages: \`${install}\``,
    '',
    `2. In tsconfig.json, set compilerOptions.types: \`${tsconfig}\``,
  ].join('\n');

  async function copy() {
    try {
      await navigator.clipboard.writeText(prompt);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      /* clipboard unavailable */
    }
  }

  return (
    <div className="relative my-6 rounded-lg border border-fd-border bg-fd-card/40 px-5 py-4">
      <button
        type="button"
        onClick={copy}
        aria-label="Copy TypeScript setup instructions for your AI agent"
        title="Copy setup instructions"
        className="group absolute right-3 top-3 inline-flex h-7 w-7 items-center justify-center rounded-md text-fd-muted-foreground hover:bg-fd-muted hover:text-ember"
      >
        <CopyIcon copied={copied} />
      </button>

      <div className="mb-3 pr-8 font-medium text-fd-foreground">TypeScript setup</div>

      {children ? <div className="mb-3 text-fd-muted-foreground [&>p]:m-0">{children}</div> : null}

      <pre className="mb-3 overflow-x-auto rounded-md border border-fd-border bg-fd-card/60 px-4 py-3 font-mono text-[0.9rem] leading-relaxed text-fd-foreground">
        <code>
          <span className="select-none text-ember">$ </span>
          {install}
        </code>
      </pre>

      <pre className="overflow-x-auto rounded-md border border-fd-border bg-fd-card/60 px-4 py-3 font-mono text-[0.9rem] leading-relaxed text-fd-foreground">
        <code>
          <span className="select-none text-fd-muted-foreground">{'// tsconfig.json\n'}</span>
          {tsconfig}
        </code>
      </pre>
    </div>
  );
}
