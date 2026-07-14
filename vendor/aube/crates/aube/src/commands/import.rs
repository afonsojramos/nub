use clap::Args;
use miette::{Context, IntoDiagnostic, miette};

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Overwrite an existing aube-lock.yaml
    #[arg(long)]
    pub force: bool,
    /// Skip lifecycle scripts when the follow-up install runs.
    ///
    /// Accepted for compatibility — `aube import` today only writes the
    /// lockfile and does not chain into install, so this is a
    /// no-op, kept so wrappers that already pass it keep working.
    #[arg(long, hide = true)]
    pub ignore_scripts: bool,
    /// Write only the converted lockfile and skip linking
    /// `node_modules` afterwards.
    ///
    /// `aube import` already exits without touching `node_modules`
    /// today, so this flag is a no-op kept for compatibility — CI
    /// scripts that pass `--lockfile-only` keep working without
    /// complaint.
    #[arg(long)]
    pub lockfile_only: bool,
}

/// Convert an existing supported lockfile into aube-lock.yaml.
///
/// Detects `pnpm-lock.yaml`, `bun.lock`, `yarn.lock`,
/// `npm-shrinkwrap.json`, or `package-lock.json` in the current project
/// and writes an equivalent `aube-lock.yaml`. Normal `aube install`
/// already reads and updates supported existing lockfiles in place, so
/// `import` is only needed when a project intentionally wants to switch
/// to `aube-lock.yaml`.
pub async fn run(args: ImportArgs) -> miette::Result<()> {
    let _ = args.ignore_scripts; // parity no-op: import doesn't chain into install yet
    let _ = args.lockfile_only; // parity no-op: import already only writes the lockfile
    let cwd = crate::dirs::project_root()?;
    let _lock = crate::commands::take_project_lock(&cwd)?;

    let manifest = super::load_manifest(&cwd.join("package.json"))?;

    // Honor `gitBranchLockfile`: the destination is whichever aube
    // lockfile this branch would actually write through, so the
    // existence check matches what `write_lockfile` will produce.
    let aube_lock_name = aube_lockfile::aube_lock_filename(&cwd);
    let aube_lock = cwd.join(&aube_lock_name);
    if aube_lock.exists() && !args.force {
        return Err(miette!(
            "{aube_lock_name} already exists\n\
             Remove it first, or pass --force to overwrite"
        ));
    }

    let (mut graph, kind) = match aube_lockfile::parse_for_import(&cwd, &manifest) {
        Ok(pair) => pair,
        Err(aube_lockfile::Error::NotFound(_)) => {
            return Err(miette!(
                "no source lockfile found\n\
                 Expected one of: pnpm-lock.yaml, bun.lock, yarn.lock, npm-shrinkwrap.json, package-lock.json"
            ));
        }
        Err(e) => {
            return Err(miette::Report::new(e)).wrap_err("failed to parse source lockfile");
        }
    };

    // A suffix-less source (npm / bun) carries no peer-context suffixes and no
    // mirrored peer edges, but install treats aube-lock.yaml as an already
    // peer-resolved incumbent and skips its peer pass — writing the parsed graph
    // verbatim would leave store-resident packages with no sibling peer links at
    // runtime (#453). Same pass, gating, and rationale as the pnpm-lock import
    // path in nub's install_family (#471): hoist → apply with pnpm-default peer
    // options → strip the hoist scaffolding; `filter_graph` intentionally
    // omitted so the imported lockfile stays cross-platform; yarn excluded
    // because yarn.lock records no per-entry `peerDependencies`.
    if matches!(
        kind,
        aube_lockfile::LockfileKind::Npm
            | aube_lockfile::LockfileKind::NpmShrinkwrap
            | aube_lockfile::LockfileKind::Bun
    ) {
        let (hoisted, auto_installed) = aube_resolver::hoist_auto_installed_peers(graph);
        graph = aube_resolver::apply_peer_contexts(
            hoisted,
            &aube_resolver::PeerContextOptions::default(),
        )
        .map_err(|e| miette!("peer-context pass failed: {e}"))?;
        aube_resolver::remove_auto_installed_peers(&mut graph, &auto_installed);
    }

    let pkg_count = graph.packages.len();
    aube_lockfile::write_lockfile(&cwd, &graph, &manifest)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {aube_lock_name}"))?;

    eprintln!(
        "Imported {pkg_count} packages from {} to {aube_lock_name}",
        kind.filename()
    );

    Ok(())
}
