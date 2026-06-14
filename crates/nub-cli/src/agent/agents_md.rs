//! Idempotent `AGENTS.md` stanza writer. The stanza is bracketed by sentinel
//! comments ([`super::artifacts::STANZA_BEGIN`] / `STANZA_END`) so a re-run
//! replaces nub's own block in place and never duplicates it or touches the
//! user's authored content.

use super::artifacts::{STANZA_BEGIN, STANZA_END, agents_md_stanza};

/// What an `AGENTS.md` merge would do, computed purely from the current file
/// contents (or `None` if the file doesn't exist). Lets the caller report the
/// action and lets tests assert without filesystem effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeAction {
    /// File doesn't exist → create it with just the stanza.
    Create,
    /// File exists, no nub block → append the stanza (preserving existing text).
    Append,
    /// File already has a nub block → replace it (idempotent re-run / upgrade).
    Replace,
}

/// Compute the new `AGENTS.md` contents and the action taken. `existing` is the
/// current file text, or `None` when the file is absent. Pure — no IO.
pub fn merge(existing: Option<&str>) -> (String, MergeAction) {
    let stanza = agents_md_stanza();
    match existing {
        None => (format!("{stanza}\n"), MergeAction::Create),
        Some(text) => {
            if let (Some(start), Some(end)) = (text.find(STANZA_BEGIN), text.find(STANZA_END)) {
                // Replace the existing block (inclusive of the end sentinel).
                let end_inclusive = end + STANZA_END.len();
                if end_inclusive >= start {
                    let mut out = String::with_capacity(text.len());
                    out.push_str(&text[..start]);
                    out.push_str(&stanza);
                    out.push_str(&text[end_inclusive..]);
                    return (out, MergeAction::Replace);
                }
                // Malformed (end before begin) — fall through to append.
            }
            // No block — append after the existing text with a blank-line gap.
            let sep = if text.ends_with("\n\n") {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            (format!("{text}{sep}{stanza}\n"), MergeAction::Append)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_file_is_created_with_stanza() {
        let (out, action) = merge(None);
        assert_eq!(action, MergeAction::Create);
        assert!(out.contains(STANZA_BEGIN));
        assert!(out.contains(STANZA_END));
        assert!(out.contains("nub install"));
    }

    #[test]
    fn existing_file_without_block_is_appended_preserving_content() {
        let original = "# My project\n\nSome guidance for agents.\n";
        let (out, action) = merge(Some(original));
        assert_eq!(action, MergeAction::Append);
        assert!(
            out.starts_with("# My project"),
            "must preserve the user's authored content verbatim at the top"
        );
        assert!(out.contains(STANZA_BEGIN));
        // A blank line separates the user's text from nub's block.
        assert!(out.contains("agents.\n\n<!-- nub:begin -->"));
    }

    #[test]
    fn rerun_replaces_the_block_idempotently() {
        // First run appends; a second run with a *changed* stanza body must
        // replace exactly the old block, leaving one block — not two.
        let original = "# Project\n";
        let (after_first, _) = merge(Some(original));
        let (after_second, action) = merge(Some(&after_first));
        assert_eq!(action, MergeAction::Replace);
        assert_eq!(
            after_second.matches(STANZA_BEGIN).count(),
            1,
            "exactly one nub block after a re-run"
        );
        // The user's heading survives.
        assert!(after_second.starts_with("# Project"));
    }

    #[test]
    fn replace_preserves_text_after_the_block() {
        // Content the user added *below* nub's block must survive a re-run.
        let original = format!(
            "# Top\n\n{}\nnub body\n{}\n\n## My own section below\n",
            STANZA_BEGIN, STANZA_END
        );
        let (out, action) = merge(Some(&original));
        assert_eq!(action, MergeAction::Replace);
        assert!(out.contains("## My own section below"));
        assert_eq!(out.matches(STANZA_BEGIN).count(), 1);
    }
}
