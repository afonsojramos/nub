//! Tiny interactive-prompt helpers for `nub agent init`. No prompt dependency —
//! a yes/no read off stdin is all we need, and adding `dialoguer` for it isn't
//! worth the dep weight.
//!
//! All prompting routes through [`Confirm`], which carries the run mode so the
//! same call site behaves correctly interactively, under `--yes`, and headless
//! (non-TTY): a headless run never blocks on stdin — it takes the default.

use std::io::{self, IsTerminal, Write};

/// How confirmations resolve for this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Prompt the user on a TTY; non-TTY falls back to each question's default.
    Interactive,
    /// `--yes`: take the affirmative for every confirmation, no prompting.
    AssumeYes,
    /// `--no`/dry-ish: take each question's stated default, no prompting.
    Defaults,
}

/// A confirmation gate. Construct once per run from the CLI flags; call
/// [`Confirm::ask`] per question.
pub struct Confirm {
    mode: Mode,
}

impl Confirm {
    pub fn new(mode: Mode) -> Self {
        Self { mode }
    }

    /// Ask `question`; `default_yes` is the answer used when not prompting (under
    /// `--yes` the answer is always yes regardless of `default_yes`; under
    /// `Defaults` or a non-TTY interactive run it's `default_yes`).
    pub fn ask(&self, question: &str, default_yes: bool) -> bool {
        match self.mode {
            Mode::AssumeYes => true,
            Mode::Defaults => default_yes,
            Mode::Interactive => {
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                    // Headless / piped: never block on stdin — take the default.
                    return default_yes;
                }
                let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
                print!("{question} {suffix} ");
                let _ = io::stdout().flush();
                let mut line = String::new();
                if io::stdin().read_line(&mut line).is_err() {
                    return default_yes;
                }
                match line.trim().to_ascii_lowercase().as_str() {
                    "" => default_yes,
                    "y" | "yes" => true,
                    "n" | "no" => false,
                    _ => default_yes,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assume_yes_always_affirms_even_against_a_no_default() {
        let c = Confirm::new(Mode::AssumeYes);
        assert!(c.ask("write AGENTS.md?", false));
    }

    #[test]
    fn defaults_mode_returns_the_stated_default() {
        let c = Confirm::new(Mode::Defaults);
        assert!(c.ask("create skill?", true));
        assert!(!c.ask("mutate AGENTS.md?", false));
    }

    #[test]
    fn interactive_falls_back_to_default_when_headless() {
        // The test harness has no TTY, so interactive must NOT block — it takes
        // the default. (This is the headless-safety contract.)
        let c = Confirm::new(Mode::Interactive);
        assert!(c.ask("skill?", true));
        assert!(!c.ask("stanza?", false));
    }
}
