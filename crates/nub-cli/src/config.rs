//! nub's global settings file — `~/.config/nub/nub.toml` (`$XDG_CONFIG_HOME/nub`,
//! `%APPDATA%\nub` on Windows).
//!
//! This is nub's OWN durable settings home, distinct from the registry/PM tuning
//! that rides `.npmrc` and the ephemeral `NUB_*` env knobs: a setting lands here
//! only when no neutral standard field expresses it AND it must survive a `nub
//! cache clear` (the config-home ladder). Today the sole key is the dlx consent
//! kill-switch `exec.implicit-dlx`. It lives under `[exec]` because dlx literally
//! means *download and exec* — a fetch-then-exec variant of local-binary exec,
//! the same behavior class, not a separate domain — so `[exec]` holds config for
//! both exec and dlx. (Config sections split by behavior class, not the nubx tier
//! chain: `[run]` = scripts, `[exec]` = tool/binary execution; this matches pnpm,
//! where exec/dlx are tools and run is scripts.)
//!
//! Read/modify/write goes through `toml_edit::DocumentMut` — NOT serde/`toml::Table`
//! — so an existing file's comments, whitespace, and key order survive a `set`
//! that touches one key. Writes are atomic (temp + rename via `aube_util`).
//!
//! The `nub config get/set exec.implicit-dlx …` surface is NOT a separate clap
//! verb (the `config` verb already exists as the engine's `.npmrc` config): the
//! nub-namespaced dotted key is intercepted in `pm_engine::store_config_family`
//! and routed here, while every other key stays on the `.npmrc` path.

use std::path::PathBuf;

use toml_edit::{DocumentMut, Item, Table, Value};

/// The `[exec]` table name and the key within it. One `const` pair so the reader,
/// the writer, and the config-verb interception can't drift.
const TABLE: &str = "exec";
const KEY: &str = "implicit-dlx";

/// The dlx consent tier. Values are `prompt` (default) and `never`; `never`
/// mirrors the interactive select's `Never` label. Reserves `allow`
/// (auto-consent) as a future value — NOT valid today.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImplicitDlx {
    /// Ask (the interactive select) on the first implicit registry fetch.
    Prompt,
    /// The implicit tier is disabled globally — fail closed, no prompt/network.
    Never,
    // Allow — reserved: auto-consent without a prompt. NOT implemented yet.
}

impl ImplicitDlx {
    pub fn as_str(self) -> &'static str {
        match self {
            ImplicitDlx::Prompt => "prompt",
            ImplicitDlx::Never => "never",
        }
    }

    pub fn parse(s: &str) -> Option<ImplicitDlx> {
        match s {
            "prompt" => Some(ImplicitDlx::Prompt),
            "never" => Some(ImplicitDlx::Never),
            _ => None,
        }
    }
}

/// Path to `~/.config/nub/nub.toml`. `None` only when no home/config root
/// resolves at all (a broken environment) — every caller treats that as "use the
/// default and don't persist."
pub fn config_path() -> Option<PathBuf> {
    Some(nub_core::node::discovery::config_dir()?.join("nub.toml"))
}

/// Read `exec.implicit-dlx`. Absent file / absent key / unparseable value all mean
/// the default (`Prompt`) — config is best-effort and never fails the gate.
pub fn implicit_dlx() -> ImplicitDlx {
    let Some(path) = config_path() else {
        return ImplicitDlx::Prompt;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return ImplicitDlx::Prompt;
    };
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return ImplicitDlx::Prompt;
    };
    doc.get(TABLE)
        .and_then(Item::as_table)
        .and_then(|t| t.get(KEY))
        .and_then(Item::as_str)
        .and_then(ImplicitDlx::parse)
        .unwrap_or(ImplicitDlx::Prompt)
}

/// Write `exec.implicit-dlx = <value>`, preserving every other key/comment in the
/// file (read-modify-write on the live `DocumentMut`). Creates the file + `nub/`
/// dir if absent. Returns an error only on an I/O failure the caller should
/// surface — an in-memory edit never fails.
pub fn set_implicit_dlx(value: ImplicitDlx) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not resolve nub's config directory",
        )
    })?;

    let mut doc = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.parse::<DocumentMut>().ok())
        .unwrap_or_default();

    // Ensure `[exec]` exists as a table, then set the key. `entry(..).or_insert`
    // keeps a pre-existing `[exec]` table (and its comments) untouched.
    let table = doc
        .entry(TABLE)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("`{TABLE}` in nub.toml is not a table"),
            )
        })?;
    table[KEY] = Item::Value(Value::from(value.as_str()));

    aube_util::fs_atomic::atomic_write(&path, doc.to_string().as_bytes())
}

/// Remove `exec.implicit-dlx` (restoring the `prompt` default), preserving the
/// rest of the file. A `config unset`/`delete` on this key routes here rather
/// than the engine's `.npmrc` delete. Absent file/key → a no-op success (nothing
/// to clear is not an error).
pub fn unset_implicit_dlx() -> std::io::Result<()> {
    let Some(path) = config_path() else {
        return Ok(());
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(mut doc) = text.parse::<DocumentMut>() else {
        return Ok(());
    };
    if let Some(table) = doc.get_mut(TABLE).and_then(Item::as_table_mut) {
        table.remove(KEY);
    }
    aube_util::fs_atomic::atomic_write(&path, doc.to_string().as_bytes())
}

/// ONE process-wide lock every test that mutates a shared env var (`XDG_*`, `CI`)
/// must hold. Both this module's `with_config_home` and `nubx_consent`'s
/// `with_isolated_env` set process-global env; if each guarded with its OWN
/// mutex they wouldn't serialize against each other and would race under cargo's
/// multi-thread runner (leaked isolation, poisoned locks). This single lock is the
/// serialization point across BOTH modules.
#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &ENV_LOCK
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Point the config path at a temp dir for the duration of the closure.
    /// `XDG_CONFIG_HOME` wins in `config_dir()`, so this fully isolates the file.
    /// Holds the process-wide [`test_env_lock`] because it mutates a global env var.
    fn with_config_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: guarded by test_env_lock; restored before the guard drops.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let out = f(dir.path());
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        out
    }

    #[test]
    fn defaults_to_prompt_when_absent() {
        with_config_home(|_| {
            assert_eq!(implicit_dlx(), ImplicitDlx::Prompt);
        });
    }

    #[test]
    fn set_never_then_read_never_roundtrips() {
        with_config_home(|home| {
            set_implicit_dlx(ImplicitDlx::Never).unwrap();
            assert_eq!(implicit_dlx(), ImplicitDlx::Never);

            // The written file is the sectioned `[exec]` form we document.
            let body = std::fs::read_to_string(home.join("nub").join("nub.toml")).unwrap();
            assert!(body.contains("[exec]"), "wrote an [exec] table: {body}");
            assert!(
                body.contains("implicit-dlx = \"never\""),
                "wrote the key: {body}"
            );

            // Re-enabling flips it back.
            set_implicit_dlx(ImplicitDlx::Prompt).unwrap();
            assert_eq!(implicit_dlx(), ImplicitDlx::Prompt);
        });
    }

    #[test]
    fn unset_clears_the_key_back_to_default() {
        with_config_home(|_| {
            set_implicit_dlx(ImplicitDlx::Never).unwrap();
            assert_eq!(implicit_dlx(), ImplicitDlx::Never);
            unset_implicit_dlx().unwrap();
            assert_eq!(implicit_dlx(), ImplicitDlx::Prompt, "cleared to default");
            // Unset on an already-clear key is a no-op success.
            unset_implicit_dlx().unwrap();
        });
    }

    #[test]
    fn set_preserves_existing_comments_and_unrelated_keys() {
        with_config_home(|home| {
            // A pre-existing file with a comment, an unrelated top-level key, and
            // an unrelated key inside [exec]. A comment-dropping serde round-trip
            // would lose the comment + reorder; toml_edit must keep all of it.
            let path = home.join("nub").join("nub.toml");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut f = std::fs::File::create(&path).unwrap();
            write!(
                f,
                "# nub settings — hand-authored\ntelemetry = false\n\n[exec]\n# an unrelated exec knob\nshell = \"bash\"\n"
            )
            .unwrap();
            drop(f);

            set_implicit_dlx(ImplicitDlx::Never).unwrap();

            let body = std::fs::read_to_string(&path).unwrap();
            assert!(
                body.contains("# nub settings — hand-authored"),
                "top comment preserved: {body}"
            );
            assert!(
                body.contains("telemetry = false"),
                "unrelated top key preserved: {body}"
            );
            assert!(
                body.contains("# an unrelated exec knob"),
                "in-table comment preserved: {body}"
            );
            assert!(
                body.contains("shell = \"bash\""),
                "unrelated [exec] key preserved: {body}"
            );
            assert!(
                body.contains("implicit-dlx = \"never\""),
                "new key written: {body}"
            );
        });
    }
}
