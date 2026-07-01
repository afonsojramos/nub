//! Loose-mode `minimumReleaseAge` auto-persist (fixes #262).
//!
//! With `minimumReleaseAge` set, pnpm's default (loose) mode installs a version
//! younger than the cutoff when nothing older satisfies the range, and
//! co-writes that package to `minimumReleaseAgeExclude` so pnpm's own
//! install-time lockfile verifier — which a project's `pnpm <script>` triggers —
//! accepts the pin. nub's engine does the same lowest-satisfying fallback but
//! never recorded the exclude, so a `nub install`/`add`/`update` produced a
//! lockfile pnpm later rejected with `ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION`
//! ([#262](https://github.com/nubjs/nub/issues/262)). This module closes that
//! gap: the resolver surfaces each immature fallback pick (armed via
//! [`arm`]), and after a successful mutating install nub appends the packages
//! to `minimumReleaseAgeExclude` in the surface the project's config identity
//! dictates.
//!
//! Write target — the same yaml-if-present-else-fallback rule aube's own
//! `config_write_target` uses, adapted to where `minimumReleaseAgeExclude` is
//! read from (`.npmrc` / the workspace yaml, never the manifest namespace):
//!
//! - a pnpm-compat project with a `pnpm-workspace.yaml` → the YAML sequence
//!   there. pnpm 10 and 11 both read it (and it's pnpm 11's own auto-persist
//!   home). [`workspace_yaml_existing`] is already gated on the
//!   `read_branded_pnpm_config` posture, so a nub-identity project's stray
//!   `pnpm-workspace.yaml` is never chosen.
//! - everything else (nub identity, a pnpm project with no workspace yaml,
//!   npm/yarn/bun compat) → the project `.npmrc`, comma-separated. This is the
//!   neutral surface nub already reads `minimumReleaseAge*` from, so nub reads
//!   back exactly what it wrote; it is brand-clean (`.npmrc` is the npm-shared
//!   config, and the key is an unbranded setting name, not another PM's
//!   namespaced field).
//!
//! Strict mode (`minimumReleaseAgeStrict=true`) never reaches here: the resolver
//! hard-aborts on `AgeGated` before returning a fallback pick, so nothing is
//! recorded.

use super::output::OutputFlags;
use aube_manifest::workspace::{add_to_workspace_yaml_string_list, workspace_yaml_existing};
use std::path::Path;

/// The setting key, in both its `.npmrc`/manifest spelling and its resolver
/// exclude-list semantics.
const EXCLUDE_KEY: &str = "minimumReleaseAgeExclude";

/// Arm the resolver's age-gate fallback sink for the upcoming resolve. Called
/// before an install/add/update engine run; [`persist`] drains it afterward.
pub(crate) fn arm() {
    aube_util::arm_age_gated_fallback_pick_collection();
}

/// Drain the resolver's immature fallback picks and, on a successful mutating
/// install, append them to `minimumReleaseAgeExclude`. Always drains the sink
/// (even on failure or when disarmed) so nothing leaks into a later in-process
/// command. `success` gates the write to a completed install; `output` gates
/// the notice on `--silent`.
pub(crate) fn persist(cwd: &Path, success: bool, output: &OutputFlags) {
    let picks = aube_util::take_age_gated_fallback_picks();
    if !success || picks.is_empty() {
        return;
    }

    // `name@version` exact-version entries, in first-seen order, deduped. The
    // resolver only records a pick that wasn't already age-exempt, so a pick is
    // never a duplicate of an existing exclude entry — dedup here only collapses
    // the same version recorded twice within one resolve.
    let mut entries: Vec<String> = Vec::with_capacity(picks.len());
    for (name, version) in &picks {
        let entry = format!("{name}@{version}");
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }

    let (target, added) = match write_excludes(cwd, &entries) {
        Ok(result) => result,
        Err(err) => {
            super::present::warn(&format!(
                "warn: installed {n} package(s) newer than minimumReleaseAge but could not \
                 record them in {EXCLUDE_KEY} ({err}); add {list} manually, or pnpm may reject \
                 this lockfile.",
                n = entries.len(),
                list = entries.join(", "),
            ));
            return;
        }
    };

    if added == 0 || output.is_silent() {
        return;
    }
    emit_notice(&target, &entries);
}

/// Where the exclude entries were persisted, for the user-facing notice.
enum Target {
    WorkspaceYaml(String),
    Npmrc,
}

impl Target {
    fn display(&self) -> &str {
        match self {
            Target::WorkspaceYaml(name) => name,
            Target::Npmrc => ".npmrc",
        }
    }
}

/// Route to the config surface and append `entries`, returning the target and
/// how many were newly written (0 = all already present).
fn write_excludes(cwd: &Path, entries: &[String]) -> anyhow::Result<(Target, usize)> {
    match workspace_yaml_existing(cwd) {
        Some(path) => {
            let added = add_to_workspace_yaml_string_list(&path, EXCLUDE_KEY, entries)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("pnpm-workspace.yaml")
                .to_string();
            Ok((Target::WorkspaceYaml(name), added))
        }
        None => {
            let added = append_npmrc(cwd, entries)?;
            Ok((Target::Npmrc, added))
        }
    }
}

/// Merge `entries` into the project `.npmrc`'s comma-separated
/// `minimumReleaseAgeExclude` key, preserving the rest of the file. Returns how
/// many were newly added.
fn append_npmrc(cwd: &Path, entries: &[String]) -> anyhow::Result<usize> {
    use aube::commands::npmrc::NpmrcEdit;
    let path = cwd.join(".npmrc");
    let mut edit = NpmrcEdit::load(&path).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Existing value is last-write-wins at read time, so merge into the last
    // occurrence's list and rewrite a single canonical key.
    let mut list: Vec<String> = edit
        .entries()
        .into_iter()
        .rfind(|(k, _)| k == EXCLUDE_KEY)
        .map(|(_, v)| parse_comma_list(&v))
        .unwrap_or_default();

    let mut added = 0usize;
    for entry in entries {
        if !list.iter().any(|e| e == entry) {
            list.push(entry.clone());
            added += 1;
        }
    }
    if added == 0 {
        return Ok(0);
    }
    edit.set(EXCLUDE_KEY, &list.join(","));
    edit.save(&path).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(added)
}

/// Split a `.npmrc` list value on commas, trimming and dropping empties. Mirrors
/// how the engine reads a `list<string>` setting from `.npmrc`.
fn parse_comma_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Factual notice naming what was recorded and where, plus the strict opt-out.
/// Stderr (engine convention: stdout is data), suppressed under `--silent` by
/// the caller.
fn emit_notice(target: &Target, entries: &[String]) {
    let lead = if entries.len() == 1 {
        "1 version newer than minimumReleaseAge was installed".to_string()
    } else {
        format!(
            "{} versions newer than minimumReleaseAge were installed",
            entries.len()
        )
    };
    super::present::info(&format!(
        "{lead} and recorded in {EXCLUDE_KEY} ({}):\n  {}\n\
         Set minimumReleaseAgeStrict=true to fail on these instead.",
        target.display(),
        entries.join("\n  "),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_comma_list_trims_and_drops_empties() {
        assert_eq!(
            parse_comma_list("a@1.0.0, b@2.0.0 ,, c@3.0.0"),
            vec!["a@1.0.0", "b@2.0.0", "c@3.0.0"]
        );
        assert!(parse_comma_list("").is_empty());
    }

    #[test]
    fn append_npmrc_creates_merges_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let npmrc = dir.path().join(".npmrc");
        std::fs::write(&npmrc, "registry=https://example.test/\n").unwrap();

        // First write creates the key alongside the existing line.
        let added = append_npmrc(
            dir.path(),
            &["caniuse-lite@1.0.0".to_string(), "vite@6.0.0".to_string()],
        )
        .unwrap();
        assert_eq!(added, 2);
        let content = std::fs::read_to_string(&npmrc).unwrap();
        assert!(content.contains("registry=https://example.test/"));
        assert!(
            content.contains("minimumReleaseAgeExclude=caniuse-lite@1.0.0,vite@6.0.0"),
            "unexpected .npmrc:\n{content}"
        );

        // Merge: one existing (deduped) + one new, into a single canonical key.
        let added = append_npmrc(
            dir.path(),
            &["vite@6.0.0".to_string(), "esbuild@0.20.0".to_string()],
        )
        .unwrap();
        assert_eq!(added, 1);
        let content = std::fs::read_to_string(&npmrc).unwrap();
        assert_eq!(
            content.matches("minimumReleaseAgeExclude").count(),
            1,
            "must rewrite one canonical key, not duplicate:\n{content}"
        );
        assert!(content.contains("esbuild@0.20.0"));
    }

    #[test]
    fn write_excludes_routes_to_npmrc_without_workspace_yaml() {
        // No pnpm-workspace.yaml → the neutral .npmrc surface.
        let dir = tempfile::tempdir().unwrap();
        let (target, added) =
            write_excludes(dir.path(), &["caniuse-lite@1.0.0".to_string()]).unwrap();
        assert!(matches!(target, Target::Npmrc));
        assert_eq!(added, 1);
        assert!(dir.path().join(".npmrc").exists());
        assert!(!dir.path().join("pnpm-workspace.yaml").exists());
    }

    #[test]
    fn write_excludes_routes_to_existing_workspace_yaml() {
        // `workspace_yaml_existing` reads the process-global `EngineContext`
        // posture; serialize against other tests that mutate it.
        let _guard = super::super::ENGINE_GLOBAL_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The pnpm-compat surface must be readable for the yaml to be chosen.
        aube_util::update_engine_context(|c| c.read_branded_pnpm_config = true);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'src/*'\n",
        )
        .unwrap();
        let (target, added) = write_excludes(dir.path(), &["vite@6.0.0".to_string()]).unwrap();
        assert!(matches!(target, Target::WorkspaceYaml(_)));
        assert_eq!(added, 1);
        assert!(!dir.path().join(".npmrc").exists());
        let content = std::fs::read_to_string(dir.path().join("pnpm-workspace.yaml")).unwrap();
        assert!(content.contains("vite@6.0.0"), "yaml not written:\n{content}");
    }
}
