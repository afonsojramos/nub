//! scan-bench — load-robust A/B of the phantom extraction ladder over a real
//! installed tree. Enumerates every unique package version root under a given
//! `node_modules`, then times the full reachable-graph scan of the whole set
//! REPEATS times per mode (min-of-N reflects the least-preempted, true-CPU run on
//! this always-contended host), for the baseline full-AST extractor vs the
//! optimized ladder. Reports total scan ms + throughput per mode and the win.
//!
//! Usage: `scan-bench <node_modules-dir> [repeats]`
//!   The `NUB_PHANTOM_BASELINE_EXTRACT` env toggle is set/cleared internally to
//!   flip `nub_phantom_scan::graph`'s extractor between runs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: scan-bench <node_modules-dir> [repeats]");
    let repeats: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(9);
    let roots = enumerate(Path::new(&dir));
    eprintln!("unique package roots: {}", roots.len());

    // Warm the page cache / filesystem once so neither mode eats first-read I/O.
    for r in &roots {
        let _ = nub_phantom_scan::scan_extracted(r);
    }

    let (base_ms, base_files) = time_mode(&roots, repeats, true);
    let (opt_ms, opt_files) = time_mode(&roots, repeats, false);
    assert_eq!(
        base_files, opt_files,
        "both modes must parse the same reachable file count"
    );

    let speedup = base_ms / opt_ms;
    println!("files_parsed={base_files}");
    println!(
        "baseline_full_ast : min {base_ms:8.2} ms  ({:.0} files/s)",
        base_files as f64 / (base_ms / 1000.0)
    );
    println!(
        "optimized_ladder  : min {opt_ms:8.2} ms  ({:.0} files/s)",
        opt_files as f64 / (opt_ms / 1000.0)
    );
    println!(
        "speedup           : {speedup:.2}x   (saved {:.1} ms / scan)",
        base_ms - opt_ms
    );
}

/// Min-of-`repeats` total wall-ms to scan all `roots` once, in the given mode.
fn time_mode(roots: &[PathBuf], repeats: usize, baseline: bool) -> (f64, usize) {
    // Flip `graph::extract_file`'s extractor. Default (unset) = baseline
    // `extract`; `=optimized` = the ladder. MUST match the env name graph.rs
    // reads (`NUB_PHANTOM_EXTRACT_MODE`) or both legs silently run baseline.
    if baseline {
        // SAFETY: single-threaded bench, no concurrent env readers.
        unsafe { std::env::remove_var("NUB_PHANTOM_EXTRACT_MODE") };
    } else {
        unsafe { std::env::set_var("NUB_PHANTOM_EXTRACT_MODE", "optimized") };
    }
    let mut best = f64::MAX;
    let mut files = 0;
    for _ in 0..repeats {
        let t = Instant::now();
        let mut f = 0;
        for r in roots {
            if let Some(res) = nub_phantom_scan::scan_extracted(r) {
                f += res.files_analyzed;
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms < best {
            best = ms;
        }
        files = f;
    }
    (best, files)
}

fn enumerate(node_modules: &Path) -> Vec<PathBuf> {
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut out = Vec::new();
    let mut stack = vec![node_modules.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            let is_dir = ft.is_dir()
                || (ft.is_symlink() && std::fs::metadata(&path).is_ok_and(|m| m.is_dir()));
            if !is_dir {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".bin" || name == ".cache" {
                continue;
            }
            // Only IMMEDIATE node_modules children are package roots: intra-package
            // subpath dirs like @apollo/client/core each ship a package.json but
            // are not separate installed packages.
            let parent_name = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str());
            let is_root = parent_name == Some("node_modules")
                || (parent_name.is_some_and(|n| n.starts_with('@'))
                    && path
                        .parent()
                        .and_then(|p| p.parent())
                        .and_then(|gp| gp.file_name())
                        .and_then(|n| n.to_str())
                        == Some("node_modules"));
            if is_root
                && path.join("package.json").is_file()
                && let Ok(canon) = std::fs::canonicalize(&path)
                && seen.insert(canon.clone())
            {
                out.push(canon);
            }
            stack.push(path);
        }
    }
    out
}
