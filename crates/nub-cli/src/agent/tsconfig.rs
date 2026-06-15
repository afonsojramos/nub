//! tsconfig type-pickup merge for `nub agent init`.
//!
//! Wires nub's ambient TypeScript declarations into the project's tsconfig so
//! the augmented surfaces (data-format imports, `import.meta.hot`, `Temporal`,
//! the Worker triad, …) typecheck. The decided shape (`.fray/ts-declarations.md`):
//!
//!   * `compilerOptions.types` ⊇ `["node", "@nubjs/types"]` — the published
//!     ambient-declarations package, auto-included via the `types` array (the
//!     pickup mechanism, since `@nubjs/types` can't ride the `@types/*` magic).
//!   * `compilerOptions.lib` ⊇ `["es2024"]`, dropping a `"dom"` placeholder — the
//!     audit found `@types/node` declares its web-platform globals unconditionally,
//!     so a project that only added `"dom"` to silence those squiggles no longer
//!     needs it under nub.
//!
//! The merge is value-level (serde with `preserve_order`) and additive: it only
//! touches `types`/`lib`, preserving every other key in insertion order. JSONC
//! comments are NOT preserved by a serde round-trip — the caller is told when the
//! source has comments so it can confirm before clobbering them.

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// The ambient-declarations package nub's `agent init` wires in. Working name
/// (`.fray/ts-declarations.md`); a sibling agent owns the final public name.
pub const TYPES_PACKAGE: &str = "@nubjs/types";

/// The outcome of computing the tsconfig merge. `changed == false` means the
/// project already had everything nub wants — a no-op (idempotent re-run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePlan {
    /// The new tsconfig text to write (pretty-printed). Only meaningful when
    /// `changed`.
    pub new_text: String,
    /// Whether anything actually changed vs. the input.
    pub changed: bool,
    /// `types` entries nub added (for the report).
    pub added_types: Vec<String>,
    /// `lib` entries nub added.
    pub added_lib: Vec<String>,
    /// Whether the `"dom"` lib placeholder was dropped.
    pub dropped_dom: bool,
    /// True when the source contained comments / trailing commas that the serde
    /// round-trip will NOT preserve — the caller should warn + confirm.
    pub had_comments: bool,
}

/// Compute the merge for an existing tsconfig's `text`. Pure — no IO.
pub fn plan(text: &str) -> Result<MergePlan> {
    let value = jsonc_parser::parse_to_serde_value(text, &Default::default())
        .context("tsconfig.json is not valid JSON/JSONC")?
        .unwrap_or(Value::Object(Map::new()));
    let had_comments = source_has_comments(text);
    plan_from_value(value, had_comments)
}

fn plan_from_value(mut value: Value, had_comments: bool) -> Result<MergePlan> {
    let root = value
        .as_object_mut()
        .context("tsconfig.json top level must be a JSON object")?;

    // Ensure compilerOptions exists (preserving its position if already present).
    if !root.contains_key("compilerOptions") {
        root.insert("compilerOptions".into(), Value::Object(Map::new()));
    }
    let co = root
        .get_mut("compilerOptions")
        .and_then(Value::as_object_mut)
        .context("compilerOptions must be an object")?;

    let mut added_types = Vec::new();
    let mut added_lib = Vec::new();
    let mut dropped_dom = false;

    // types: ⊇ ["node", "@nubjs/types"], appended in order, de-duped.
    {
        let want = ["node", TYPES_PACKAGE];
        let mut arr = string_array(co.get("types"));
        for w in want {
            if !arr.iter().any(|e| e == w) {
                arr.push(w.to_string());
                added_types.push(w.to_string());
            }
        }
        if !added_types.is_empty() || co.contains_key("types") {
            co.insert("types".into(), to_value_array(&arr));
        }
    }

    // lib: ⊇ ["es2024"], dropping a "dom" placeholder (case-insensitive).
    {
        let mut arr = string_array(co.get("lib"));
        let had_lib = co.contains_key("lib");
        let before_len = arr.len();
        arr.retain(|e| {
            let drop = e.eq_ignore_ascii_case("dom");
            if drop {
                dropped_dom = true;
            }
            !drop
        });
        let dropped_any = arr.len() != before_len;
        if !arr.iter().any(|e| e.eq_ignore_ascii_case("es2024")) {
            arr.push("es2024".to_string());
            added_lib.push("es2024".to_string());
        }
        if !added_lib.is_empty() || dropped_any || had_lib {
            co.insert("lib".into(), to_value_array(&arr));
        }
    }

    let changed = !added_types.is_empty() || !added_lib.is_empty() || dropped_dom;
    let new_text =
        serde_json::to_string_pretty(&value).context("re-serializing tsconfig failed")? + "\n";

    Ok(MergePlan {
        new_text,
        changed,
        added_types,
        added_lib,
        dropped_dom,
        had_comments,
    })
}

/// Read an array-of-strings option (`types`/`lib`), tolerating absent/non-array.
fn string_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn to_value_array(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

/// Cheap heuristic: does the source contain `//` or `/* */` comments (which a
/// serde round-trip drops)? Scans outside string literals. Trailing commas are
/// also serde-incompatible but harmless to drop, so we only flag comments.
fn source_has_comments(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'/' if i + 1 < bytes.len() => match bytes[i + 1] {
                b'/' | b'*' => return true,
                _ => {}
            },
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn co_types(text: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(text).unwrap();
        string_array(v["compilerOptions"].get("types"))
    }
    fn co_lib(text: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(text).unwrap();
        string_array(v["compilerOptions"].get("lib"))
    }

    #[test]
    fn empty_tsconfig_gets_both_types_and_es2024_lib() {
        let plan = plan("{}").unwrap();
        assert!(plan.changed);
        assert_eq!(co_types(&plan.new_text), vec!["node", "@nubjs/types"]);
        assert_eq!(co_lib(&plan.new_text), vec!["es2024"]);
        assert_eq!(plan.added_types, vec!["node", "@nubjs/types"]);
        assert_eq!(plan.added_lib, vec!["es2024"]);
        assert!(!plan.dropped_dom);
    }

    #[test]
    fn existing_types_are_preserved_and_nub_appended_without_dupes() {
        let src = r#"{"compilerOptions":{"types":["node","jest"]}}"#;
        let plan = plan(src).unwrap();
        assert_eq!(
            co_types(&plan.new_text),
            vec!["node", "jest", "@nubjs/types"],
            "existing entries kept in order; only the missing nub package appended"
        );
        // `node` already present → not re-added.
        assert_eq!(plan.added_types, vec!["@nubjs/types"]);
    }

    #[test]
    fn dom_placeholder_is_dropped_and_es2024_added() {
        let src = r#"{"compilerOptions":{"lib":["DOM","ES2022"]}}"#;
        let plan = plan(src).unwrap();
        assert!(plan.dropped_dom, "case-insensitive dom must be dropped");
        // ES2022 stays; es2024 appended; DOM gone.
        assert_eq!(co_lib(&plan.new_text), vec!["ES2022", "es2024"]);
    }

    #[test]
    fn idempotent_when_already_wired() {
        let src = r#"{"compilerOptions":{"types":["node","@nubjs/types"],"lib":["es2024"]}}"#;
        let plan = plan(src).unwrap();
        assert!(!plan.changed, "a fully-wired tsconfig is a no-op");
        assert!(plan.added_types.is_empty());
        assert!(plan.added_lib.is_empty());
    }

    #[test]
    fn other_compiler_options_and_keys_are_preserved() {
        let src = r#"{"compilerOptions":{"strict":true,"target":"es2022"},"include":["src"]}"#;
        let plan = plan(src).unwrap();
        let v: Value = serde_json::from_str(&plan.new_text).unwrap();
        assert_eq!(v["compilerOptions"]["strict"], Value::Bool(true));
        assert_eq!(
            v["compilerOptions"]["target"],
            Value::String("es2022".into())
        );
        assert_eq!(v["include"], serde_json::json!(["src"]));
    }

    #[test]
    fn jsonc_with_comments_parses_and_is_flagged() {
        let src = "{\n  // base config\n  \"compilerOptions\": { \"strict\": true }\n}";
        let plan = plan(src).unwrap();
        assert!(plan.had_comments, "comment-bearing source must be flagged");
        assert!(plan.changed);
        // Still merges correctly.
        assert_eq!(co_types(&plan.new_text), vec!["node", "@nubjs/types"]);
    }

    #[test]
    fn comment_detection_ignores_slashes_inside_strings() {
        // A URL in a string value must not be mistaken for a comment.
        let src = r#"{"compilerOptions":{"baseUrl":"https://x/y"}}"#;
        assert!(!source_has_comments(src));
    }

    #[test]
    fn invalid_json_errors() {
        assert!(plan("{ not json").is_err());
    }
}
