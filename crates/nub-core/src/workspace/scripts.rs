//! Script resolution from package.json and npm_* env injection.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve a script name from package.json#scripts.
pub fn resolve_script(manifest: &serde_json::Value, name: &str) -> Option<String> {
    manifest
        .get("scripts")
        .and_then(|s| s.get(name))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Build the npm_* environment variables from package.json.
///
/// `user_agent_product` is the role-aware UA *product tokens* (everything
/// before the `<os> <arch>` tail) composed by the PM engine's role resolver
/// (`crates/nub-cli/src/pm_engine/mod.rs::run_lifecycle_ua_product`), so a
/// `nub run`/`nub exec` script reports the incumbent PM's role exactly like an
/// engine lifecycle spawn does (e.g. `pnpm/9.1.0 nub/<v> node/v<ver>`). The
/// platform tail is appended here in Node's `process.platform`/`process.arch`
/// vocabulary so postinstall sniffers parse one format. nub-core has no PM
/// identity logic, so the role-aware product is threaded in rather than
/// recomputed — keeping the UA composition centralized in one place.
pub fn npm_env(
    manifest: &serde_json::Value,
    project_root: &Path,
    lifecycle_event: &str,
    lifecycle_script: Option<&str>,
    node_execpath: &str,
    user_agent_product: &str,
) -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    if let Some(name) = manifest.get("name").and_then(|v| v.as_str()) {
        // Also the recursion-guard key in `run_one_workspace_pkg` (workspace member
        // names are unique), so no new — and brand-forbidden NUB_* — env sentinel
        // is needed to stop a `"build": "nub run -r build"` script looping forever.
        env_vars.insert("npm_package_name".to_string(), name.to_string());
    }
    if let Some(version) = manifest.get("version").and_then(|v| v.as_str()) {
        env_vars.insert("npm_package_version".to_string(), version.to_string());
    }

    // pnpm/npm export the manifest's `engines`, `config`, and `bin` fields
    // deep-flattened into the `npm_package_*` namespace so dep postinstalls
    // and `package.json#config` consumers (`node-pre-gyp`, scripts reading
    // `$npm_package_config_<key>`, …) behave identically under nub. The
    // allowlist mirrors pnpm 11.5 exactly (name/version above, then
    // engines/config/bin); whole-manifest flattening was dropped by pnpm and
    // is intentionally NOT replicated. A string `bin` flattens to an
    // unsuffixed `npm_package_bin`; an object `bin` to `npm_package_bin_<key>`.
    for field in ["engines", "config", "bin"] {
        if let Some(value) = manifest.get(field) {
            flatten_npm_package_env(&format!("npm_package_{field}"), value, &mut env_vars);
        }
    }

    env_vars.insert(
        "npm_lifecycle_event".to_string(),
        lifecycle_event.to_string(),
    );

    if let Some(script) = lifecycle_script {
        env_vars.insert("npm_lifecycle_script".to_string(), script.to_string());
    }

    env_vars.insert(
        "npm_package_json".to_string(),
        project_root
            .join("package.json")
            .to_string_lossy()
            .to_string(),
    );

    // npm_node_execpath is the resolved Node binary, threaded in from discovery
    // (A13/A38) — no `node -e process.execPath` subprocess per `nub run`. This is
    // also more correct than shelling out to a bare `node`, which would ignore an
    // .nvmrc/.node-version pin that discovery honors.
    if !node_execpath.is_empty() {
        env_vars.insert("npm_node_execpath".to_string(), node_execpath.to_string());
    }

    env_vars.insert("npm_command".to_string(), "run-script".to_string());

    // pnpm's UA shape (`<product tokens> <platform> <arch>`) so postinstall
    // sniffers (which-pm-runs, only-allow, create-* scaffolders) parse it. The
    // product tokens are ROLE-AWARE — composed by the PM engine and threaded in
    // (incumbent-first in compat mode, e.g. `pnpm/9.1.0 nub/<v> node/v<ver>`;
    // nub-first under nub identity / fresh). Platform tokens use Node's
    // process.platform/process.arch vocabulary (darwin/win32, x64/arm64), not
    // Rust's, so parsers see the same words npm/pnpm send.
    env_vars.insert(
        "npm_config_user_agent".to_string(),
        format!("{user_agent_product} {} {}", node_platform(), node_arch()),
    );

    if let Ok(exe) = env::current_exe() {
        env_vars.insert(
            "npm_execpath".to_string(),
            exe.to_string_lossy().to_string(),
        );
    }

    env_vars.insert(
        "INIT_CWD".to_string(),
        env::current_dir()
            .unwrap_or_else(|_| project_root.to_path_buf())
            .to_string_lossy()
            .to_string(),
    );

    env_vars
}

/// Node's `process.platform` vocabulary for the UA string (`darwin`, `win32`,
/// `linux`), mapped from Rust's `std::env::consts::OS`.
fn node_platform() -> &'static str {
    match env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Node's `process.arch` vocabulary (`x64`, `arm64`, `ia32`), mapped from
/// Rust's `std::env::consts::ARCH`.
fn node_arch() -> &'static str {
    match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// Envify a manifest key the npm way: every character outside `[A-Za-z0-9_]`
/// becomes `_`, so `config.my-key` → `npm_package_config_my_key`.
fn envify_env_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Deep-flatten a JSON value into `prefix`-rooted `npm_package_*` pairs,
/// npm-style: objects recurse with `_`-joined envified keys, arrays index with
/// `_<i>`, scalars stringify, `null` is skipped. Matches aube's
/// `flatten_json_env` (the lifecycle path) so the run and lifecycle paths emit
/// byte-identical `npm_package_*` environments.
fn flatten_npm_package_env(
    prefix: &str,
    value: &serde_json::Value,
    out: &mut HashMap<String, String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                flatten_npm_package_env(&format!("{prefix}_{}", envify_env_key(k)), v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                flatten_npm_package_env(&format!("{prefix}_{i}"), v, out);
            }
        }
        serde_json::Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        serde_json::Value::Number(n) => {
            out.insert(prefix.to_string(), n.to_string());
        }
        serde_json::Value::Bool(b) => {
            out.insert(prefix.to_string(), b.to_string());
        }
        serde_json::Value::Null => {}
    }
}

/// Build the PATH with node_modules/.bin directories prepended.
pub fn bin_path(project_root: &Path, workspace_root: Option<&Path>) -> String {
    let mut dirs = Vec::new();

    // Walk from project root up, adding each node_modules/.bin.
    let mut dir = project_root.to_path_buf();
    for _ in 0..16 {
        let bin_dir = dir.join("node_modules").join(".bin");
        if bin_dir.is_dir() {
            dirs.push(bin_dir.to_string_lossy().to_string());
        }
        if workspace_root.is_some() && Some(dir.as_path()) == workspace_root {
            break;
        }
        if !dir.pop() {
            break;
        }
    }

    let existing = env::var("PATH").unwrap_or_default();
    if dirs.is_empty() {
        existing
    } else {
        dirs.push(existing);
        dirs.join(crate::PATH_LIST_SEPARATOR)
    }
}

/// Find a binary in node_modules/.bin by name, walking up from `cwd`.
///
/// On Unix the entry is the extensionless name (a symlink to the package's JS,
/// `#!/usr/bin/env node`). On Windows npm/pnpm write `<name>.cmd` (the runnable
/// shim), `<name>.ps1`, and an extensionless Bash script that Windows can't run
/// — so we look for the executable extensions, in PATHEXT-ish preference, and
/// never return the unrunnable Bash stub (A40).
pub fn find_bin(name: &str, cwd: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates: &[&str] = &[".cmd", ".exe", ".bat", ".ps1"];
    #[cfg(not(windows))]
    let candidates: &[&str] = &[""];

    let mut dir = cwd.to_path_buf();
    for _ in 0..16 {
        let bin_dir = dir.join("node_modules").join(".bin");
        for ext in candidates {
            let candidate = bin_dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Read a single `key = value` setting from `.npmrc` (project-level, then
/// user-level `~/.npmrc`), returning the first match's value with surrounding
/// quotes stripped. The one `.npmrc` reader in the crate — `script_shell` and
/// (P1) the PM registry lookup both go through it.
pub fn npmrc_value(project_root: &Path, key: &str) -> Option<String> {
    // Check project .npmrc first, then ~/.npmrc
    let candidates = [
        project_root.join(".npmrc"),
        dirs_next::home_dir()
            .map(|h| h.join(".npmrc"))
            .unwrap_or_default(),
    ];

    for path in &candidates {
        if let Ok(content) = fs::read_to_string(path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some((k, value)) = trimmed.split_once('=') {
                    if k.trim() == key {
                        let value = value.trim().trim_matches('"').trim_matches('\'');
                        if !value.is_empty() {
                            return Some(value.to_string());
                        }
                    }
                }
            }
        }
    }

    None
}

/// Read `script-shell` from `.npmrc` (project-level, then user-level).
/// Returns the shell path if set, or None to use the platform default.
pub fn script_shell(project_root: &Path) -> Option<String> {
    npmrc_value(project_root, "script-shell")
}

#[cfg(test)]
mod tests {
    use super::{find_bin, npm_env, npmrc_value, script_shell};
    use std::fs;

    #[test]
    fn npm_env_flattens_engines_config_and_bin() {
        // pnpm/npm export the manifest's engines/config/bin deep-flattened into
        // the `npm_package_*` namespace; a script reading `$npm_package_config_foo`
        // must see the value. Mirrors pnpm 10.15: an object `bin` →
        // `npm_package_bin_<key>` (verbatim path), non-word chars in keys → `_`.
        let manifest = serde_json::json!({
            "name": "pkg",
            "version": "1.0.0",
            "engines": { "node": ">=18" },
            "config": { "foo": "barval", "my-key": "v" },
            "bin": { "mytool": "./cli.js" },
        });
        let tmp = std::env::temp_dir();
        let env = npm_env(
            &manifest,
            &tmp,
            "test",
            None,
            "/usr/bin/node",
            "nub/0 node/v0",
        );

        assert_eq!(
            env.get("npm_package_config_foo").map(String::as_str),
            Some("barval")
        );
        assert_eq!(
            env.get("npm_package_config_my_key").map(String::as_str),
            Some("v")
        );
        assert_eq!(
            env.get("npm_package_engines_node").map(String::as_str),
            Some(">=18")
        );
        assert_eq!(
            env.get("npm_package_bin_mytool").map(String::as_str),
            Some("./cli.js")
        );
    }

    #[test]
    fn npm_env_string_bin_is_unsuffixed() {
        // A string `bin` flattens to a bare `npm_package_bin` (no key suffix),
        // matching pnpm. (npm normalizes the value to the unscoped package name;
        // nub matches pnpm, which keeps the verbatim path.)
        let manifest = serde_json::json!({ "name": "pkg", "bin": "./cli.js" });
        let env = npm_env(&manifest, &std::env::temp_dir(), "test", None, "", "ua");
        assert_eq!(
            env.get("npm_package_bin").map(String::as_str),
            Some("./cli.js")
        );
    }

    #[test]
    fn npmrc_value_reads_keys_and_script_shell_delegates() {
        let tmp = std::env::temp_dir().join(format!("nub-npmrc-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join(".npmrc"),
            "registry=https://example.test/\nscript-shell = \"/bin/dash\"\n",
        )
        .unwrap();

        // Arbitrary keys round-trip, with surrounding quotes/whitespace stripped.
        assert_eq!(
            npmrc_value(&tmp, "registry").as_deref(),
            Some("https://example.test/")
        );
        // script_shell is now a thin delegate over npmrc_value (quote-stripped).
        assert_eq!(script_shell(&tmp).as_deref(), Some("/bin/dash"));
        // A key absent from the project .npmrc falls through to None (no ~/.npmrc
        // key named this in CI).
        assert!(npmrc_value(&tmp, "nub-no-such-key").is_none());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_bin_resolves_platform_entry() {
        let tmp = std::env::temp_dir().join(format!("nub-a40-{}", std::process::id()));
        let bin_dir = tmp.join("node_modules").join(".bin");
        fs::create_dir_all(&bin_dir).unwrap();

        // The resolvable entry is the runnable `.cmd` on Windows, the
        // extensionless symlink/script on Unix (A40).
        #[cfg(windows)]
        let entry = bin_dir.join("tool.cmd");
        #[cfg(not(windows))]
        let entry = bin_dir.join("tool");
        fs::write(&entry, "x").unwrap();

        assert_eq!(find_bin("tool", &tmp).as_deref(), Some(entry.as_path()));
        assert!(find_bin("missing", &tmp).is_none());

        // On Windows a lone extensionless Bash stub (no `.cmd`) is not runnable,
        // so it must NOT be returned — better to fall through to the PM delegate.
        #[cfg(windows)]
        {
            let stub_root = tmp.join("stub-only");
            let stub_dir = stub_root.join("node_modules").join(".bin");
            fs::create_dir_all(&stub_dir).unwrap();
            fs::write(stub_dir.join("stubtool"), "#!/bin/sh\n").unwrap();
            assert!(find_bin("stubtool", &stub_root).is_none());
        }

        let _ = fs::remove_dir_all(&tmp);
    }
}
