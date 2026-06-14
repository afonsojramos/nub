//! Agent auto-detection: inspect the cwd for the markers each coding-agent
//! leaves behind and decide which artifact `nub agent init` should produce.
//!
//! The detection is intentionally cheap (filesystem `exists` checks only) and
//! order-independent — a repo can host several agents at once (Claude Code +
//! Cursor + a shared `AGENTS.md`), so we report *every* agent we find rather
//! than picking one. The caller decides what to write for each.

use std::path::Path;

/// A coding agent nub knows how to target. The variant determines the *primary*
/// artifact: a skill (Claude Code / opencode), a rules file (Cursor / Codex),
/// or — for the generic case — the opt-in `AGENTS.md` stanza.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code — `.claude/`. Primary artifact: a skill under `.claude/skills/`.
    ClaudeCode,
    /// opencode — `.opencode/`. Skill-capable, same skills layout as Claude Code.
    Opencode,
    /// Cursor — `.cursor/`. Primary artifact: a rule under `.cursor/rules/`.
    Cursor,
    /// Codex / generic `AGENTS.md` consumer — `.codex/` or a bare `AGENTS.md`.
    /// Primary artifact: the (opt-in) `AGENTS.md` stanza.
    Codex,
}

impl Agent {
    /// Human label for prompts / summaries.
    pub fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Opencode => "opencode",
            Agent::Cursor => "Cursor",
            Agent::Codex => "Codex",
        }
    }

    /// True when this agent's primary artifact is a skill (skills live under a
    /// `skills/` dir and auto-surface — the least-invasive injection).
    pub fn is_skill_based(self) -> bool {
        matches!(self, Agent::ClaudeCode | Agent::Opencode)
    }
}

/// The full detection result for a directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    /// Every agent whose marker dir/file is present, in a stable order.
    pub agents: Vec<Agent>,
    /// Whether a bare `AGENTS.md` exists at the root (drives whether the stanza
    /// offer is "append to your AGENTS.md" vs "create one").
    pub has_agents_md: bool,
    /// Whether a `tsconfig.json` exists at the root (gates the types-pickup offer).
    pub has_tsconfig: bool,
}

impl Detection {
    /// True when nothing agent-shaped was found — the caller falls back to
    /// offering the generic `AGENTS.md` stanza + a Claude-Code skill default.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty() && !self.has_agents_md
    }
}

/// Inspect `dir` and report which agents are present. Pure filesystem probing —
/// no writes, no network. The order is stable (skill-based agents first) so the
/// caller's "primary artifact" choice is deterministic.
pub fn detect(dir: &Path) -> Detection {
    let mut agents = Vec::new();

    // Skill-capable agents first (their skill is the preferred primary artifact).
    if dir.join(".claude").is_dir() {
        agents.push(Agent::ClaudeCode);
    }
    if dir.join(".opencode").is_dir() {
        agents.push(Agent::Opencode);
    }
    if dir.join(".cursor").is_dir() {
        agents.push(Agent::Cursor);
    }
    if dir.join(".codex").is_dir() {
        agents.push(Agent::Codex);
    }

    Detection {
        agents,
        has_agents_md: dir.join("AGENTS.md").is_file(),
        has_tsconfig: dir.join("tsconfig.json").is_file(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn td() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn empty_dir_detects_nothing() {
        let d = td();
        let det = detect(d.path());
        assert!(det.is_empty(), "a bare dir has no agents and no AGENTS.md");
        assert!(det.agents.is_empty());
        assert!(!det.has_agents_md);
        assert!(!det.has_tsconfig);
    }

    #[test]
    fn claude_dir_detects_claude_code_as_skill_based() {
        let d = td();
        fs::create_dir(d.path().join(".claude")).unwrap();
        let det = detect(d.path());
        assert_eq!(det.agents, vec![Agent::ClaudeCode]);
        assert!(det.agents[0].is_skill_based());
        assert!(!det.is_empty());
    }

    #[test]
    fn cursor_and_codex_are_not_skill_based() {
        let d = td();
        fs::create_dir(d.path().join(".cursor")).unwrap();
        fs::create_dir(d.path().join(".codex")).unwrap();
        let det = detect(d.path());
        assert_eq!(det.agents, vec![Agent::Cursor, Agent::Codex]);
        assert!(!Agent::Cursor.is_skill_based());
        assert!(!Agent::Codex.is_skill_based());
    }

    #[test]
    fn multiple_agents_reported_skill_based_first() {
        // A repo can host several agents at once; we report every one, with the
        // skill-based agents ahead of the rules/stanza ones (stable ordering).
        let d = td();
        for marker in [".cursor", ".claude", ".codex", ".opencode"] {
            fs::create_dir(d.path().join(marker)).unwrap();
        }
        let det = detect(d.path());
        assert_eq!(
            det.agents,
            vec![
                Agent::ClaudeCode,
                Agent::Opencode,
                Agent::Cursor,
                Agent::Codex
            ]
        );
    }

    #[test]
    fn bare_agents_md_alone_is_not_empty_even_with_no_agent_dirs() {
        // A repo with only AGENTS.md still has a target — the stanza offer.
        let d = td();
        fs::write(d.path().join("AGENTS.md"), "# guidance\n").unwrap();
        let det = detect(d.path());
        assert!(det.agents.is_empty());
        assert!(det.has_agents_md);
        assert!(!det.is_empty());
    }

    #[test]
    fn tsconfig_presence_is_reported() {
        let d = td();
        fs::write(d.path().join("tsconfig.json"), "{}\n").unwrap();
        assert!(detect(d.path()).has_tsconfig);
    }
}
