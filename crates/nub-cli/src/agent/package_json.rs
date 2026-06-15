//! `package.json` devDependency merge for `nub agent init`.
//!
//! When the user accepts the `@nubjs/types` types offer (same offer that wires
//! the tsconfig `types`/`lib`), this module also records the package in
//! `devDependencies`. The version written matches the running nub binary —
//! the `@nubjs/types` package is versioned in lock-step with the CLI.
//!
//! The merge is value-level (`serde_json` with `preserve_order`) and additive:
//! it only touches `devDependencies`, inserting the key if missing or leaving it
//! unchanged if already correct, and never touches `dependencies`.

use anyhow::{Context, Result};
use serde_json::{Map, Value};

pub use crate::agent::tsconfig::TYPES_PACKAGE;

/// The version string written into devDependencies. The `@nubjs/types` package
/// is versioned in lock-step with the CLI binary (workspace version), so the
/// binary version is the exact version to request.
pub const TYPES_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Outcome of computing the devDep merge. `changed == false` means the
/// package.json already had `@nubjs/types` in devDependencies at the right
/// version — idempotent re-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePlan {
    /// The new package.json text (pretty-printed, trailing newline).
    /// Only meaningful when `changed`.
    pub new_text: String,
    /// Whether anything actually changed vs. the input.
    pub changed: bool,
}

/// Compute the devDep merge for an existing `package.json` text. Pure — no IO.
pub fn plan(text: &str) -> Result<MergePlan> {
    let mut value: Value = serde_json::from_str(text).context("package.json is not valid JSON")?;

    let root = value
        .as_object_mut()
        .context("package.json top level must be a JSON object")?;

    // Ensure devDependencies exists (preserving position if already present).
    if !root.contains_key("devDependencies") {
        root.insert("devDependencies".into(), Value::Object(Map::new()));
    }
    let dev_deps = root
        .get_mut("devDependencies")
        .and_then(Value::as_object_mut)
        .context("devDependencies must be an object")?;

    let want_version = TYPES_VERSION;
    let already_correct = dev_deps.get(TYPES_PACKAGE).and_then(Value::as_str) == Some(want_version);

    if already_correct {
        // Re-serialize anyway so `new_text` is always a valid JSON string,
        // but signal `changed = false` so the caller skips the write.
        let new_text = serde_json::to_string_pretty(&value)
            .context("re-serializing package.json failed")?
            + "\n";
        return Ok(MergePlan {
            new_text,
            changed: false,
        });
    }

    dev_deps.insert(
        TYPES_PACKAGE.into(),
        Value::String(want_version.to_string()),
    );

    let new_text =
        serde_json::to_string_pretty(&value).context("re-serializing package.json failed")? + "\n";

    Ok(MergePlan {
        new_text,
        changed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev_dep_version(text: &str, pkg: &str) -> Option<String> {
        let v: Value = serde_json::from_str(text).unwrap();
        v["devDependencies"][pkg].as_str().map(str::to_string)
    }

    fn has_runtime_dep(text: &str, pkg: &str) -> bool {
        let v: Value = serde_json::from_str(text).unwrap();
        v["dependencies"][pkg].is_string()
    }

    #[test]
    fn empty_package_json_gets_dev_dependency() {
        let plan = plan("{}").unwrap();
        assert!(plan.changed);
        assert_eq!(
            dev_dep_version(&plan.new_text, TYPES_PACKAGE),
            Some(TYPES_VERSION.to_string()),
            "@nubjs/types must appear in devDependencies with the binary version"
        );
    }

    #[test]
    fn adds_to_existing_dev_dependencies() {
        let src = r#"{"devDependencies":{"typescript":"5.0.0"}}"#;
        let plan = plan(src).unwrap();
        assert!(plan.changed);
        // Existing entry preserved.
        assert_eq!(
            dev_dep_version(&plan.new_text, "typescript"),
            Some("5.0.0".to_string())
        );
        // Nub types added.
        assert_eq!(
            dev_dep_version(&plan.new_text, TYPES_PACKAGE),
            Some(TYPES_VERSION.to_string())
        );
    }

    #[test]
    fn idempotent_when_already_present_at_correct_version() {
        // Build a package.json that already has @nubjs/types at the binary version.
        let src = format!(
            r#"{{"devDependencies":{{"{}":{:?}}}}}"#,
            TYPES_PACKAGE, TYPES_VERSION
        );
        let plan = plan(&src).unwrap();
        assert!(
            !plan.changed,
            "package.json already has the right devDep — must be a no-op"
        );
    }

    #[test]
    fn updates_stale_version_to_current() {
        // An older version string should be bumped to the current binary version.
        let src = format!(
            r#"{{"devDependencies":{{"{}":{:?}}}}}"#,
            TYPES_PACKAGE, "0.0.1"
        );
        let plan = plan(&src).unwrap();
        assert!(plan.changed, "stale version must trigger a write");
        assert_eq!(
            dev_dep_version(&plan.new_text, TYPES_PACKAGE),
            Some(TYPES_VERSION.to_string())
        );
    }

    #[test]
    fn never_added_to_runtime_dependencies() {
        let src = r#"{"dependencies":{"express":"4.0.0"}}"#;
        let plan = plan(src).unwrap();
        assert!(
            !has_runtime_dep(&plan.new_text, TYPES_PACKAGE),
            "@nubjs/types must NOT appear in runtime dependencies"
        );
        // Still written to devDependencies.
        assert_eq!(
            dev_dep_version(&plan.new_text, TYPES_PACKAGE),
            Some(TYPES_VERSION.to_string())
        );
    }

    #[test]
    fn key_order_is_preserved() {
        // Keys that appear before devDependencies must still be before it; new
        // devDependencies block inserted after the last key present.
        let src = r#"{"name":"my-app","version":"1.0.0","devDependencies":{"jest":"29.0.0"}}"#;
        let plan = plan(src).unwrap();
        let v: Value = serde_json::from_str(&plan.new_text).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        // name and version must come before devDependencies.
        let name_pos = keys.iter().position(|k| *k == "name").unwrap();
        let ver_pos = keys.iter().position(|k| *k == "version").unwrap();
        let dev_pos = keys.iter().position(|k| *k == "devDependencies").unwrap();
        assert!(name_pos < dev_pos, "name must precede devDependencies");
        assert!(ver_pos < dev_pos, "version must precede devDependencies");
        // jest entry preserved within devDependencies.
        assert_eq!(
            dev_dep_version(&plan.new_text, "jest"),
            Some("29.0.0".to_string())
        );
    }

    #[test]
    fn invalid_json_errors() {
        assert!(plan("{ not json").is_err());
    }

    #[test]
    fn non_object_top_level_errors() {
        assert!(plan("[1,2,3]").is_err());
    }
}
