//! `nub agent` — make AI coding agents reliably reach for nub.
//!
//! The command group (today: one verb, `init`) generates the artifacts that
//! teach an agent to use `nub` instead of `node`/`npm`/`npx`. The PRIMARY
//! artifact is a SKILL (least-invasive, auto-surfacing, additive); a secondary,
//! opt-in `AGENTS.md` stanza is offered only with explicit approval.
//!
//! Design + decisions: `.fray/ai-friendliness.md`. This is a non-forwarding
//! group handled by a manual sub-verb match (like `nub node` / `nub pm`), so its
//! bare-usage and invalid-verb messages read consistently.

mod agents_md;
mod artifacts;
mod detect;
mod prompt;
mod tsconfig;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use detect::{Agent, Detection};
use prompt::{Confirm, Mode};

/// The in-repo ambient-declarations fallback, bundled from `assets/nub-env.d.ts`
/// (kept byte-identical to the `@nubjs/types` package content). Written into the
/// project as the offline / no-install-step pickup path.
const NUB_ENV_DTS: &str = include_str!("../../assets/nub-env.d.ts");

/// Entry point for `nub agent …`, dispatched from `dispatch_subcommand`.
pub fn run(args: &[String]) -> Result<i32> {
    let verb = args.first().map(String::as_str);
    if matches!(verb, None | Some("help") | Some("--help") | Some("-h")) {
        print_usage();
        return Ok(0);
    }
    match verb.expect("verb present after the help/bare guard") {
        "init" => run_init(&args[1..]),
        other => bail!(
            "nub agent takes a subcommand (init). Unknown verb '{other}'. \
             See `nub agent --help`."
        ),
    }
}

fn print_usage() {
    println!(
        "nub agent — make AI coding agents reach for nub\n\n\
         Usage: nub agent <command>\n\n\
         Commands:\n\
         \x20 init    set up the current project so its AI agent uses nub\n\
         \x20         (generates a skill; offers an AGENTS.md stanza + TS types)\n\n\
         Options (init):\n\
         \x20 -y, --yes   accept every offer without prompting\n\
         \x20     --no    decline every optional offer (skill only)\n\
         \x20 -C, --dir <DIR>  operate on <DIR> instead of the cwd"
    );
}

/// Parsed `nub agent init` flags. Hand-parsed (not clap) so the group keeps the
/// manual-dispatch shape of `nub node`/`nub pm`; the flag set is tiny.
struct InitFlags {
    mode: Mode,
    dir: Option<PathBuf>,
}

fn parse_init_flags(args: &[String]) -> Result<InitFlags> {
    let mut mode = Mode::Interactive;
    let mut dir = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-y" | "--yes" => mode = Mode::AssumeYes,
            "--no" => mode = Mode::Defaults,
            "-C" | "--dir" => {
                i += 1;
                let Some(d) = args.get(i) else {
                    bail!("`nub agent init {}` requires a directory", args[i - 1]);
                };
                dir = Some(PathBuf::from(d));
            }
            other if other.starts_with("--dir=") => {
                dir = Some(PathBuf::from(&other["--dir=".len()..]));
            }
            other => bail!("unknown flag '{other}' for `nub agent init`"),
        }
        i += 1;
    }
    Ok(InitFlags { mode, dir })
}

fn run_init(args: &[String]) -> Result<i32> {
    let flags = parse_init_flags(args)?;
    let cwd = match flags.dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let det = detect::detect(&cwd);
    let confirm = Confirm::new(flags.mode);
    let mut written: Vec<String> = Vec::new();

    println!("nub agent init — {}", cwd.display());
    report_detection(&det);

    // ── Primary: the skill (and any rules files). Skill-based agents get a
    //    skill; Cursor gets a rule; Codex routes to the stanza below. When NO
    //    agent is detected we still default to a Claude-Code skill (the most
    //    common target) — it's additive and harmless if unused.
    write_primary_artifacts(&cwd, &det, &confirm, &mut written)?;

    // ── Secondary (opt-in, default NO): the AGENTS.md stanza. Mutating a file
    //    the user authored is invasive, so this is decline-by-default.
    let stanza_default = false;
    let stanza_q = if det.has_agents_md {
        "Append nub's guidance to AGENTS.md?"
    } else {
        "Create an AGENTS.md with nub's guidance?"
    };
    if confirm.ask(stanza_q, stanza_default) {
        write_agents_md(&cwd, &mut written)?;
    }

    // ── TypeScript types pickup (offered when a tsconfig exists OR the project
    //    looks TS-shaped). Default YES — it's additive and makes nub's surfaces
    //    typecheck.
    maybe_wire_types(&cwd, &det, &confirm, &mut written)?;

    print_summary(&written);
    Ok(0)
}

fn report_detection(det: &Detection) {
    if det.is_empty() {
        println!("  detected: no agent markers — defaulting to a Claude Code skill");
        return;
    }
    let mut parts: Vec<&str> = det.agents.iter().map(|a| a.label()).collect();
    if det.has_agents_md {
        parts.push("AGENTS.md");
    }
    println!("  detected: {}", parts.join(", "));
}

/// Write the skill (or rule) for each skill/rule-capable agent. When the repo has
/// no skill/rule-capable agent at all, default to a Claude Code skill.
fn write_primary_artifacts(
    cwd: &Path,
    det: &Detection,
    confirm: &Confirm,
    written: &mut Vec<String>,
) -> Result<()> {
    let mut targets: Vec<Agent> = det
        .agents
        .iter()
        .copied()
        .filter(|a| *a != Agent::Codex) // Codex has no rules dir — it uses the stanza.
        .collect();
    if targets.is_empty() {
        // No skill/rule agent detected → default skill target.
        targets.push(Agent::ClaudeCode);
    }

    for agent in targets {
        let (rel, body) = match agent {
            Agent::ClaudeCode => artifacts::skill(),
            Agent::Opencode => artifacts::opencode_skill(),
            Agent::Cursor => artifacts::cursor_rule(),
            Agent::Codex => unreachable!("Codex filtered out above"),
        };
        let noun = if agent.is_skill_based() { "skill" } else { "rule" };
        let q = format!("Create the nub {noun} for {} ({rel})?", agent.label());
        if confirm.ask(&q, true) {
            write_file(cwd, rel, &body)?;
            written.push(rel.to_string());
        }
    }
    Ok(())
}

fn write_agents_md(cwd: &Path, written: &mut Vec<String>) -> Result<()> {
    let path = cwd.join("AGENTS.md");
    let existing = std::fs::read_to_string(&path).ok();
    let (new_text, action) = agents_md::merge(existing.as_deref());
    std::fs::write(&path, new_text)?;
    let verb = match action {
        agents_md::MergeAction::Create => "created",
        agents_md::MergeAction::Append => "updated (appended nub stanza)",
        agents_md::MergeAction::Replace => "updated (replaced nub stanza)",
    };
    written.push(format!("AGENTS.md ({verb})"));
    Ok(())
}

/// Offer the TypeScript types pickup: merge tsconfig + drop the `nub-env.d.ts`
/// fallback. Only offered when there's a tsconfig (or the user opts in for a
/// TS-shaped project). The tsconfig merge is value-level + additive.
fn maybe_wire_types(
    cwd: &Path,
    det: &Detection,
    confirm: &Confirm,
    written: &mut Vec<String>,
) -> Result<()> {
    // Offer only when a tsconfig exists — that's the unambiguous TS signal. (A
    // no-tsconfig project that nonetheless runs `.ts` files still benefits, but
    // creating a tsconfig from nothing is more invasive than this command should
    // be by default.)
    if !det.has_tsconfig {
        return Ok(());
    }

    let tsconfig_path = cwd.join("tsconfig.json");
    let text = std::fs::read_to_string(&tsconfig_path)?;
    let plan = match tsconfig::plan(&text) {
        Ok(p) => p,
        Err(e) => {
            println!("  skipping tsconfig: {e}");
            return Ok(());
        }
    };

    if plan.changed {
        let mut q = format!(
            "Wire nub's types into tsconfig.json (types += {}, lib += es2024{})?",
            tsconfig::TYPES_PACKAGE,
            if plan.dropped_dom { ", drop dom" } else { "" }
        );
        if plan.had_comments {
            q.push_str(" [note: comments in tsconfig.json will be removed]");
        }
        if confirm.ask(&q, true) {
            std::fs::write(&tsconfig_path, &plan.new_text)?;
            written.push("tsconfig.json (types wired)".to_string());
        }
    } else {
        println!("  tsconfig.json already wired for nub's types — no change");
    }

    // The in-repo fallback `.d.ts` — works without installing `@nubjs/types`
    // (offline / no-install). Default NO: it duplicates the package's content, so
    // it's only wanted when the package can't be added.
    if confirm.ask(
        "Also drop an in-repo nub-env.d.ts fallback (for offline / no-install)?",
        false,
    ) {
        write_file(cwd, "nub-env.d.ts", NUB_ENV_DTS)?;
        written.push("nub-env.d.ts (types fallback)".to_string());
    }
    Ok(())
}

fn print_summary(written: &[String]) {
    if written.is_empty() {
        println!("\nNothing written.");
        return;
    }
    println!("\nWrote:");
    for w in written {
        println!("  + {w}");
    }
}

/// Write `body` to `cwd/rel`, creating parent dirs. Overwrites (re-run = refresh).
fn write_file(cwd: &Path, rel: &str, body: &str) -> Result<()> {
    let path = cwd.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn parse_flags_defaults_to_interactive_cwd() {
        let f = parse_init_flags(&[]).unwrap();
        assert_eq!(f.mode, Mode::Interactive);
        assert!(f.dir.is_none());
    }

    #[test]
    fn parse_flags_yes_and_dir() {
        let f = parse_init_flags(&["--yes".into(), "--dir".into(), "/tmp/x".into()]).unwrap();
        assert_eq!(f.mode, Mode::AssumeYes);
        assert_eq!(f.dir, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn parse_flags_no_is_defaults_mode() {
        assert_eq!(parse_init_flags(&["--no".into()]).unwrap().mode, Mode::Defaults);
    }

    #[test]
    fn parse_flags_rejects_unknown() {
        assert!(parse_init_flags(&["--frobnicate".into()]).is_err());
    }

    #[test]
    fn bundled_dts_is_a_global_script_file() {
        // The fallback MUST stay a global script (no *top-level* import/export)
        // or the `declare module "*.yaml"` wildcards silently stop resolving —
        // the load-bearing gotcha from the ts-declarations audit. An `export`
        // *inside* a `declare module { … }` block is fine (it's the module's own
        // export, not a top-level one); the un-indented form at column 0 is what
        // turns the file into a module. So flag only column-0 import/export.
        for line in NUB_ENV_DTS.lines() {
            assert!(
                !line.starts_with("import ") && !line.starts_with("export "),
                "nub-env.d.ts must be a global script (no top-level import/export); offending line: {line}"
            );
        }
        assert!(NUB_ENV_DTS.contains("declare module \"*.yaml\""));
    }

    // ── End-to-end: --yes drives the whole flow non-interactively ──

    #[test]
    fn yes_run_with_no_markers_writes_default_claude_skill() {
        let d = td();
        let code = run_init(&["--yes".into(), "--dir".into(), d.path().display().to_string()])
            .unwrap();
        assert_eq!(code, 0);
        // Default skill for an unmarked repo.
        assert!(d.path().join(".claude/skills/nub/SKILL.md").is_file());
        // --yes also accepts the stanza offer → AGENTS.md created.
        assert!(d.path().join("AGENTS.md").is_file());
        let agents = std::fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        assert!(agents.contains("nub install"));
    }

    #[test]
    fn yes_run_writes_skill_for_detected_claude_and_rule_for_cursor() {
        let d = td();
        std::fs::create_dir(d.path().join(".claude")).unwrap();
        std::fs::create_dir(d.path().join(".cursor")).unwrap();
        run_init(&["--yes".into(), "--dir".into(), d.path().display().to_string()]).unwrap();
        assert!(d.path().join(".claude/skills/nub/SKILL.md").is_file());
        assert!(d.path().join(".cursor/rules/nub.mdc").is_file());
    }

    #[test]
    fn no_mode_writes_only_the_skill_default() {
        // `--no` declines every optional offer; the skill defaults to YES, the
        // stanza + fallback default to NO.
        let d = td();
        std::fs::create_dir(d.path().join(".claude")).unwrap();
        run_init(&["--no".into(), "--dir".into(), d.path().display().to_string()]).unwrap();
        assert!(d.path().join(".claude/skills/nub/SKILL.md").is_file());
        assert!(
            !d.path().join("AGENTS.md").exists(),
            "stanza must NOT be written under --no"
        );
    }

    #[test]
    fn yes_run_wires_tsconfig_when_present() {
        let d = td();
        std::fs::create_dir(d.path().join(".claude")).unwrap();
        std::fs::write(
            d.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"strict":true}}"#,
        )
        .unwrap();
        run_init(&["--yes".into(), "--dir".into(), d.path().display().to_string()]).unwrap();
        let ts = std::fs::read_to_string(d.path().join("tsconfig.json")).unwrap();
        assert!(ts.contains("@nubjs/types"), "types must be wired");
        assert!(ts.contains("es2024"));
        // --yes also accepts the fallback .d.ts.
        assert!(d.path().join("nub-env.d.ts").is_file());
    }

    #[test]
    fn no_tsconfig_means_no_types_wiring_offered() {
        let d = td();
        std::fs::create_dir(d.path().join(".claude")).unwrap();
        run_init(&["--yes".into(), "--dir".into(), d.path().display().to_string()]).unwrap();
        assert!(!d.path().join("tsconfig.json").exists());
        assert!(
            !d.path().join("nub-env.d.ts").exists(),
            "no tsconfig → no types offer, so no fallback either"
        );
    }

    #[test]
    fn rerun_is_idempotent_for_agents_md_and_tsconfig() {
        let d = td();
        std::fs::create_dir(d.path().join(".claude")).unwrap();
        std::fs::write(d.path().join("tsconfig.json"), "{}").unwrap();
        let args = vec![
            "--yes".to_string(),
            "--dir".to_string(),
            d.path().display().to_string(),
        ];
        run_init(&args).unwrap();
        let agents1 = std::fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        run_init(&args).unwrap();
        let agents2 = std::fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        assert_eq!(agents1, agents2, "AGENTS.md re-run must be idempotent");
        assert_eq!(
            agents2.matches(artifacts::STANZA_BEGIN).count(),
            1,
            "exactly one nub stanza after a re-run"
        );
    }

    #[test]
    fn bad_verb_errors() {
        assert!(run(&["bogus".into()]).is_err());
    }

    #[test]
    fn bare_and_help_print_usage_ok() {
        assert_eq!(run(&[]).unwrap(), 0);
        assert_eq!(run(&["help".into()]).unwrap(), 0);
        assert_eq!(run(&["--help".into()]).unwrap(), 0);
    }
}
