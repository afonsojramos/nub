//! macOS backend — Seatbelt (SBPL) via `sandbox-exec`.
//!
//! This is the fully-implemented reference backend (the primary dev OS). It
//! evolves aube's profile in two security-meaningful ways while keeping aube's
//! proven-bootable base:
//!   1. **Read deny-set** — `(deny file-read* ...)` for the secret paths +
//!      a recursive `.env*` regex deny. aube's `(allow default)` left reads
//!      wide open; this closes the credential-read channel (defense-in-depth).
//!   2. **Tight write** — `(deny file-write*)` then re-allow ONLY the package
//!      dir + jail-home + extra-write roots. aube also re-allowed `/tmp` and
//!      `/private/tmp`; we DROP those (match Linux: world-writable tmp invites
//!      symlink races — only the private jail-home is scratch).
//!   3. **Net** — `(deny network*)` with a loopback-only carve-out to the
//!      egress proxy port when one is set; otherwise full network-deny. Per-host
//!      filtering lives in the proxy (§3), not the kernel.
//!
//! On the base policy: a true `(deny default)` profile needs the verbatim
//! Chrome-derived essential-permissions block (SRT) to even spawn a process —
//! that hardening is a documented follow-on (see DESIGN-NOTES below). This
//! backend ships the `(allow default)` base (so programs reliably start) +
//! the read-deny / write-confine / net-deny layers, which deliver the §8.5
//! load-bearing kills (FRD + FWC + NET) today, at-or-above aube parity.
//!
//! DESIGN-NOTES (follow-on phases, tracked in `.fray/build-jail-design.md` §8):
//!   - Port SRT's verbatim essential-permissions block + flip base to
//!     `(deny default)` for full read-confine parity with the read-allow set.
//!   - Wire the localhost egress proxy + 302-redirect re-check (§3); until then
//!     `proxy_port` is None and net is full-deny (coarse but fail-safe).

use crate::backend::Degradation;
use crate::policy::SandboxPolicy;
use std::path::Path;
use std::process::Command;

/// Egress-proxy port to carve out (loopback-only). `None` until the proxy is
/// wired — net then degrades to full-deny (fail-safe).
// The proxy is a follow-on phase; thread it here when it lands.
const PROXY_PORT: Option<u16> = None;

fn sbpl_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Build the SBPL profile string for `policy`.
pub(crate) fn build_profile(policy: &SandboxPolicy) -> String {
    let mut rules = vec!["(version 1)".to_string(), "(allow default)".to_string()];

    // ── NET ──────────────────────────────────────────────────────────────
    if policy.net.enforce {
        rules.push("(deny network*)".to_string());
        // AF_UNIX: gate to nothing by default (aube allowed it blanket, which
        // lets a script reach /var/run/docker.sock). Re-allow only loopback IP
        // to the proxy when wired.
        if let Some(port) = PROXY_PORT {
            // Seatbelt requires the host in a `remote ip` literal to be `*` or
            // `localhost` — a `127.0.0.1` literal is a PARSE ERROR that fails the
            // WHOLE profile load (caught in review). `localhost` covers the
            // loopback proxy on both 127.0.0.1 and ::1.
            rules.push(format!(
                "(allow network-outbound (remote ip \"localhost:{port}\"))"
            ));
        }
    }

    // ── FS READ DENY (secret set + .env* regex) ──────────────────────────
    // Placed BEFORE the write rules so write re-allows can override read-denies
    // on writable roots if they overlap (last-specific-match-wins in SBPL).
    for p in &policy.fs.read_deny {
        push_read_deny_rule(&mut rules, p);
        // The kernel evaluates rules against the CANONICAL path (macOS resolves
        // /var -> /private/var, /tmp -> /private/tmp, and any symlinked HOME).
        // A literal subpath that doesn't match the canonical form silently fails
        // to deny — so emit the canonicalized form too. (Found by the e2e
        // secret-read test: a tempdir under /var/folders leaked because only the
        // /var literal was denied, not /private/var.)
        if let Ok(canon) = p.canonicalize()
            && canon != *p
        {
            push_read_deny_rule(&mut rules, &canon);
        }
    }
    // recursive .env* deny at any depth (matches the Seatbelt native-deny path
    // in sandbox-fs-deny-list.md). A single regex covers every directory level.
    //
    // FIRST-CUT LIMITATION (data/enforcement mismatch — documented, not a bug):
    // `read_deny_glob` is consumed only as a BOOLEAN here (`!is_empty()`); the
    // individual pattern VALUES in `secrets::read_deny_globs()` are NOT
    // translated to SBPL — a single hardcoded `.env`/`.env.*` regex is emitted.
    // So on macOS this axis is `.env*`-ONLY: adding a new glob (e.g. `*.pem`,
    // `secrets/**`) to the policy would SILENTLY NOT be enforced on macOS until
    // per-pattern glob→SBPL-regex translation lands (a tracked follow-up). The
    // secret-DIR denies (`~/.ssh` etc.) go through the `read_deny` subpath path
    // above and ARE fully enforced; only the glob axis is `.env`-limited.
    if !policy.fs.read_deny_glob.is_empty() {
        // `/\.env($|\.)` matches `<…>/.env` and `<…>/.env.<anything>`.
        rules.push("(deny file-read* (regex #\"/\\.env($|\\.)\"))".to_string());
    }

    // ── FS WRITE (allow-only) ────────────────────────────────────────────
    if policy.fs.write_enforce {
        rules.push("(deny file-write*)".to_string());
        // /dev needs write for the standard char devices many builds touch.
        push_write_rule(&mut rules, Path::new("/dev"));
        for p in &policy.fs.write_allow {
            push_write_rule(&mut rules, p);
            if let Ok(canon) = p.canonicalize()
                && canon != *p
            {
                push_write_rule(&mut rules, &canon);
            }
        }
    }

    rules.join("\n")
}

fn push_read_deny_rule(rules: &mut Vec<String>, path: &Path) {
    let path = sbpl_escape(&path.to_string_lossy());
    let rule = format!("(deny file-read* (subpath \"{path}\"))");
    if !rules.iter().any(|r| r == &rule) {
        rules.push(rule);
    }
}

fn push_write_rule(rules: &mut Vec<String>, path: &Path) {
    let path = sbpl_escape(&path.to_string_lossy());
    let rule = format!("(allow file-write* (subpath \"{path}\"))");
    if !rules.iter().any(|r| r == &rule) {
        rules.push(rule);
    }
}

/// Apply the macOS sandbox to `cmd` by wrapping its program in
/// `sandbox-exec -p <profile> -- <orig program> <orig args>`.
pub fn apply(cmd: &mut Command, policy: &SandboxPolicy) -> std::io::Result<Degradation> {
    let profile = build_profile(policy);

    // Re-home the command under sandbox-exec. We must read the existing program
    // + args off `cmd` and rebuild it; std::process::Command exposes get_program
    // / get_args for exactly this.
    let program = cmd.get_program().to_owned();
    let args: Vec<_> = cmd.get_args().map(|a| a.to_owned()).collect();

    let mut wrapped = Command::new("sandbox-exec");
    wrapped.arg("-p").arg(&profile).arg("--").arg(&program);
    wrapped.args(&args);

    // Carry env + cwd from the original command onto the wrapped one.
    //
    // CRITICAL (caught in adversarial review): `wrapped` is a FRESH Command and
    // would otherwise inherit this process's FULL parent environment at spawn —
    // so replaying only `cmd.get_envs()` (which after the caller's env_clear()
    // yields just the allowlist) would silently re-leak every parent secret the
    // scrub removed. We MUST start the wrapped command from a cleared env and
    // apply exactly what `cmd` carries. `env_clear()` here makes the wrapped
    // command's env authoritative; `get_envs()` then re-applies the scrubbed
    // allowlist (or the full inherited set when env enforcement is off — in which
    // case `cmd` was never cleared and carries no explicit keys, so we instead
    // do NOT clear and let inheritance stand). The discriminator: if the caller
    // scrubbed, `cmd` has explicit keys; if not, it has none.
    if let Some(dir) = cmd.get_current_dir() {
        wrapped.current_dir(dir);
    }
    let explicit: Vec<_> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned())))
        .collect();
    // env enforcement is signaled by the policy axis, not inferred — clear iff
    // the env axis is enforced so the scrubbed allowlist is the WHOLE child env.
    if policy.env.enforce {
        wrapped.env_clear();
    }
    for (k, v) in explicit {
        match v {
            Some(val) => {
                wrapped.env(k, val);
            }
            None => {
                wrapped.env_remove(k);
            }
        }
    }

    *cmd = wrapped;

    // net is enforced only coarsely until the proxy lands — report honestly.
    let mut deg = Degradation::full();
    if policy.net.enforce && PROXY_PORT.is_none() && !policy.net.allow_hosts.is_empty() {
        deg.lost.push("net-per-host".into());
        deg.reason = Some("egress proxy not yet wired — network fully denied".into());
    }
    Ok(deg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_jail::{self, BuildJailParams};
    use std::path::PathBuf;

    fn jail_policy() -> SandboxPolicy {
        build_jail::policy(&BuildJailParams {
            package_dir: PathBuf::from("/proj/node_modules/dep"),
            project_root: PathBuf::from("/proj"),
            jail_home: PathBuf::from("/tmp/nub-jail/1/dep"),
            user_home: PathBuf::from("/Users/me"),
            extra_write: vec![PathBuf::from("/Users/me/.cache/node-gyp")],
            registry_hosts: vec!["registry.npmjs.org".into()],
            extra_hosts: vec![],
            bundle_browser_cdns: false,
        })
    }

    #[test]
    fn profile_denies_secrets_and_confines_write() {
        let prof = build_profile(&jail_policy());
        assert!(prof.contains("(version 1)"));
        // secret read-deny present
        assert!(prof.contains("(deny file-read* (subpath \"/Users/me/.ssh\"))"));
        // recursive .env* deny present
        assert!(prof.contains(".env($|\\.)"));
        // write confined: deny-all then re-allow the package dir, NOT /tmp
        assert!(prof.contains("(deny file-write*)"));
        assert!(prof.contains("(allow file-write* (subpath \"/proj/node_modules/dep\"))"));
        assert!(!prof.contains("(allow file-write* (subpath \"/tmp\"))"));
        // net denied (no proxy wired yet)
        assert!(prof.contains("(deny network*)"));
    }

    #[test]
    fn profile_does_not_grant_write_to_project_source() {
        let prof = build_profile(&jail_policy());
        // /proj is readable but must NOT be writable (source is read-only)
        assert!(!prof.contains("(allow file-write* (subpath \"/proj\"))"));
    }
}
