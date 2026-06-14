//! The text artifacts `nub agent init` writes: the skill (primary), the rules
//! file (Cursor/Codex), and the opt-in `AGENTS.md` stanza.
//!
//! All three carry the same core message — *use `nub` instead of `npm`/`npx`/
//! direct `node`* — at three verbosities matched to the host format. The skill
//! is the richest (it auto-surfaces, so it can afford detail); the stanza is the
//! tersest (it's appended to a file the user authored, so it must be unobtrusive).
//!
//! VOICE NOTE: this copy is a DRAFT pending the maintainer's product-voice review. Keep it
//! factual and brand-clean; don't ship marketing flourish here.

/// A sentinel comment that brackets the stanza nub appends to `AGENTS.md`, so a
/// re-run can find + replace its own block idempotently without clobbering the
/// user's authored content.
pub const STANZA_BEGIN: &str = "<!-- nub:begin -->";
pub const STANZA_END: &str = "<!-- nub:end -->";

/// The primary artifact: a Claude-Code / opencode skill. Returns
/// `(relative_path, contents)`. The skill is a single `SKILL.md` with YAML front
/// matter; the `description` is what the agent matches against to decide when to
/// surface it, so it enumerates the trigger surfaces (install, run a script,
/// execute a file, npx) explicitly.
pub fn skill() -> (&'static str, String) {
    (".claude/skills/nub/SKILL.md", SKILL_MD.to_string())
}

/// The opencode skill path (same content, different home). opencode reads skills
/// from `.opencode/skills/` rather than `.claude/skills/`.
pub fn opencode_skill() -> (&'static str, String) {
    (".opencode/skills/nub/SKILL.md", SKILL_MD.to_string())
}

/// The Cursor rule file. Cursor reads `.cursor/rules/*.mdc` with a small front
/// matter; `alwaysApply: true` makes it ambient (the rules-file analogue of a
/// skill's auto-surfacing).
pub fn cursor_rule() -> (&'static str, String) {
    (".cursor/rules/nub.mdc", CURSOR_MDC.to_string())
}

// Codex reads project guidance from `AGENTS.md` (no separate rules dir), so the
// Codex target IS the opt-in stanza below — the dispatcher routes it there.

/// The opt-in `AGENTS.md` stanza — bracketed by [`STANZA_BEGIN`]/[`STANZA_END`]
/// so it can be re-found and replaced. Terse: a handful of lines, neutral, and
/// clearly nub's.
pub fn agents_md_stanza() -> String {
    format!("{STANZA_BEGIN}\n{AGENTS_STANZA_BODY}\n{STANZA_END}")
}

// ── DRAFT skill content (pending the maintainer's voice review) ───────────────────────

const SKILL_MD: &str = r#"---
name: nub
description: >-
  This project uses nub, a drop-in superset of the Node toolchain. ALWAYS use
  nub instead of node, npm, npx, pnpm, yarn, or bun for: installing
  dependencies, running package.json scripts, executing JS/TS files, and the
  npx-equivalent. Surfaces whenever you would otherwise reach for node/npm/npx
  or a package manager.
---

# Use nub, not node / npm / npx

This project runs on **nub** — a Rust CLI that augments the project's Node
instead of replacing it. It runs TypeScript directly (no build step), manages
the Node version, and is a package manager compatible with the project's
existing lockfile. When a task involves Node, a package, a script, or a `.ts`/
`.js` file, reach for `nub` — not the tool you'd default to.

## Translation table — what to STOP using

| Instead of                       | Use                          |
| -------------------------------- | ---------------------------- |
| `node script.js` / `node app.ts` | `nub script.js` / `nub app.ts` |
| `ts-node x.ts` / `tsx x.ts`      | `nub x.ts`                    |
| `npm install` / `pnpm install`   | `nub install`                |
| `npm install <pkg>`              | `nub add <pkg>`              |
| `npm uninstall <pkg>`            | `nub remove <pkg>`           |
| `npm run <script>`               | `nub run <script>`           |
| `npm test` / `npm start`         | `nub run test` / `nub run start` |
| `npx <tool>` / `pnpm dlx <tool>` | `nubx <tool>` (or `nub dlx <tool>`) |
| `npm ci`                         | `nub ci`                     |
| `nvm use` / `nvm install`        | nub provisions the pinned Node automatically |

## Why

- **TypeScript just works.** `nub file.ts` runs TypeScript, JSX, and decorators
  with no `tsconfig`, no build step, no `ts-node`/`tsx`. If `tsc --noEmit`
  accepts the code, nub runs it.
- **Compatible, not a replacement.** nub runs your Node code byte-for-byte; it
  augments through Node's own extension surfaces. The package manager is
  lockfile-compatible with whatever the project already uses (npm / pnpm / bun
  round-trip; yarn read-only).
- **No separate version manager.** nub provisions and pins the project's Node
  version on demand — you never run `nvm` or `corepack`.

## Rules

- Do NOT run `node`, `npm`, `npx`, `pnpm`, `yarn`, or `bun` directly. Use the
  `nub` equivalent from the table above.
- Run package.json scripts with `nub run <script>`, never `npm run <script>`.
- Execute a TypeScript or JavaScript file with `nub <file>` — never `node <file>`
  and never a separate transpiler.
- Install with `nub install`; add/remove a single dependency with `nub add` /
  `nub remove`. nub respects the project's existing lockfile.
- For a one-off package binary (the `npx` use case), use `nubx <tool>`.
- If you genuinely need plain Node behavior for a single run (differential
  debugging), use `nub --node <file>` — it runs the project's pinned Node with
  nub's augmentation disabled.

## Escape hatches (rare)

- `nub node` — Node version management (`install` / `ls` / `pin`). You normally
  never need this; nub provisions the pin automatically.
- `nub pm` — package-manager management (`use` / `which`). Bailouts for when you
  must inspect or change the project's PM identity.
"#;

const CURSOR_MDC: &str = r#"---
description: This project uses nub — use nub instead of node/npm/npx/pnpm/yarn/bun.
alwaysApply: true
---

# Use nub, not node / npm / npx

This project runs on **nub**, a drop-in superset of the Node toolchain (runs
TypeScript directly, manages the Node version, package manager compatible with
the existing lockfile). Use `nub` for anything you'd otherwise do with
`node`/`npm`/`npx`/`pnpm`/`yarn`/`bun`.

- `node file.js` / `node app.ts` / `ts-node` / `tsx`  →  `nub <file>`
- `npm install` / `pnpm install`  →  `nub install`
- `npm install <pkg>` / `npm uninstall <pkg>`  →  `nub add <pkg>` / `nub remove <pkg>`
- `npm run <script>` / `npm test`  →  `nub run <script>`
- `npx <tool>` / `pnpm dlx <tool>`  →  `nubx <tool>` (or `nub dlx <tool>`)
- `npm ci`  →  `nub ci`
- `nvm use` / `nvm install`  →  not needed; nub provisions the pinned Node automatically

Do NOT invoke `node`, `npm`, `npx`, `pnpm`, `yarn`, or `bun` directly — use the
`nub` equivalent. For plain-Node behavior on a single run, use `nub --node <file>`.
"#;

const AGENTS_STANZA_BODY: &str = r#"## Use nub, not node / npm / npx

This project uses **nub**, a drop-in superset of the Node toolchain — it runs
TypeScript directly, manages the Node version, and is a package manager
compatible with the project's existing lockfile. Use `nub` for anything you'd
otherwise do with `node` / `npm` / `npx` / `pnpm` / `yarn` / `bun`:

- Run a file: `nub <file>` (TS and JS, no build step) — never `node <file>`, `ts-node`, or `tsx`.
- Install: `nub install`; add/remove one dep: `nub add <pkg>` / `nub remove <pkg>` — never `npm`/`pnpm install`.
- Run a script: `nub run <script>` — never `npm run <script>`.
- One-off tool (npx): `nubx <tool>` or `nub dlx <tool>`.
- Clean CI install: `nub ci`. Plain-Node for one run: `nub --node <file>`.

nub provisions the project's pinned Node automatically — you don't need `nvm` or `corepack`."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_path_is_under_claude_skills_and_names_nub() {
        let (path, body) = skill();
        assert_eq!(path, ".claude/skills/nub/SKILL.md");
        // The skill must carry YAML front matter (name + description) so the
        // agent can match it; the description is the trigger surface.
        assert!(body.starts_with("---\n"), "skill needs YAML front matter");
        assert!(body.contains("name: nub"));
        assert!(
            body.contains("description:"),
            "the description drives auto-surfacing"
        );
    }

    #[test]
    fn skill_tells_the_agent_what_to_stop_using() {
        let (_, body) = skill();
        // The load-bearing instruction: STOP using the default tools.
        for stop in ["node", "npm", "npx", "pnpm", "yarn", "bun"] {
            assert!(
                body.contains(stop),
                "skill must mention {stop} (the tool to stop using)"
            );
        }
        // And the nub replacements.
        for use_ in ["nub install", "nub run", "nubx", "nub add"] {
            assert!(body.contains(use_), "skill must point at `{use_}`");
        }
    }

    #[test]
    fn skill_is_brand_clean_no_prohibited_public_surfaces() {
        let (_, body) = skill();
        // Brand boundary: the skill is nub's own artifact (branding is fine), but
        // it must NOT teach a prohibited PUBLIC surface.
        assert!(
            !body.contains("globalThis.nub"),
            "no public globalThis.nub"
        );
        assert!(!body.contains("nub:"), "no nub:* import namespace");
        assert!(!body.contains("@nub/"), "no @nub/* scope");
    }

    #[test]
    fn opencode_skill_shares_content_at_its_own_path() {
        let (cc_path, cc_body) = skill();
        let (oc_path, oc_body) = opencode_skill();
        assert_eq!(oc_path, ".opencode/skills/nub/SKILL.md");
        assert_ne!(cc_path, oc_path, "different homes");
        assert_eq!(cc_body, oc_body, "same content");
    }

    #[test]
    fn cursor_rule_is_an_mdc_with_always_apply() {
        let (path, body) = cursor_rule();
        assert_eq!(path, ".cursor/rules/nub.mdc");
        assert!(body.contains("alwaysApply: true"), "rule must be ambient");
        assert!(body.contains("nub install"));
    }

    #[test]
    fn stanza_is_bracketed_for_idempotent_replacement() {
        let s = agents_md_stanza();
        assert!(s.starts_with(STANZA_BEGIN));
        assert!(s.trim_end().ends_with(STANZA_END));
        // Body must carry the core instruction.
        assert!(s.contains("nub install"));
        assert!(s.contains("nubx"));
    }
}
