//! HTMLRewriter integration tests: run fixture scripts through the built `nub`
//! binary and assert the rewrite output. Exercises the Cloudflare-Workers-shape
//! global backed by the lol-html binding in nub-native.
//!
//! These require the nub-native addon to be present at `runtime/addons/nub-native.node`
//! (built by `make addon` / `make addon-fast`), the same prerequisite as the
//! data-format loader tests.

use std::path::{Path, PathBuf};
use std::process::Command;

fn nub_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push(format!("nub{}", std::env::consts::EXE_SUFFIX));
    path
}

fn fixture(file: &str) -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    Path::new(&manifest)
        .join("../../tests/fixtures/html-rewriter")
        .join(file)
}

fn run(file: &str, extra_args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let mut cmd = Command::new(nub_binary());
    for a in extra_args {
        cmd.arg(a);
    }
    cmd.arg(fixture(file));
    for &(k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to spawn nub");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// The full surface: the global is present + non-enumerable, attribute/content
/// mutation, escaped-vs-raw insertion, element removal, document-end append,
/// doctype read, text rewriting, streaming over a Response, synchronous
/// selector-error, and the first-cut async-handler rejection.
#[test]
fn rewrites_html_across_the_workers_api_surface() {
    let (stdout, stderr, code) = run("rewrite.mjs", &[], &[]);
    assert_eq!(code, 0, "fixture must exit 0\nstderr: {stderr}");

    // Attribute injection + setInnerContent.
    assert!(
        stdout.contains(r#"ATTR: <h1>Hi</h1><a href="/" rel="noopener">link</a>"#),
        "attribute/content rewrite wrong:\n{stdout}"
    );
    // Raw HTML inserted verbatim; text-mode insertion HTML-escaped.
    assert!(
        stdout.contains("CONTENT: <p>x<b>raw</b>&lt;i&gt;esc&lt;/i&gt;</p>"),
        "escaped-vs-raw insertion wrong:\n{stdout}"
    );
    // Doctype name is readable.
    assert!(
        stdout.contains("DOCTYPE: html"),
        "doctype name wrong:\n{stdout}"
    );
    // Element removed; document-end content appended.
    assert!(
        stdout.contains("REMOVE: <!DOCTYPE html><div>keep</div><!--end-->"),
        "remove + document-end append wrong:\n{stdout}"
    );
    // Text rewriting.
    assert!(
        stdout.contains("TEXT: <span>HELLO</span>"),
        "text rewrite wrong:\n{stdout}"
    );
    // Async (Promise-returning) handlers are awaited mid-transform (Asyncify):
    // the element handler awaits before setAttribute, the document-end handler
    // awaits before append, and both land in the output.
    assert!(
        stdout.contains(r#"ASYNC: <a href="/" data-async="1">x</a><!--async-end-->"#),
        "async handlers must be awaited mid-transform:\n{stdout}"
    );
    // Streaming over a Response yields the transformed body.
    assert!(
        stdout.contains("STREAM: <title>Streamed</title>"),
        "Response streaming wrong:\n{stdout}"
    );
    // Cloudflare-exact: a non-Response input throws a TypeError.
    assert!(
        stdout.contains("BADINPUT: true"),
        "non-Response transform input must throw:\n{stdout}"
    );
    // Invalid selector throws synchronously at .on().
    assert!(
        stdout.contains("BADSEL: true"),
        "invalid selector must throw:\n{stdout}"
    );

    assert!(
        stdout.contains("DONE"),
        "fixture did not run to completion:\n{stdout}"
    );
}

/// Under `--node` (zero augmentation) the global is absent.
#[test]
fn absent_under_node_flag() {
    let (stdout, stderr, code) = run("compat-absent.mjs", &["--node"], &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("HTMLREWRITER: undefined"),
        "HTMLRewriter must be absent under --node:\n{stdout}"
    );
}

/// A truthy `NODE_COMPAT` is the persistent tree-wide augmentation opt-out — same
/// effect as `--node`.
#[test]
fn absent_under_node_compat_env() {
    let (stdout, stderr, code) = run("compat-absent.mjs", &[], &[("NODE_COMPAT", "1")]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("HTMLREWRITER: undefined"),
        "HTMLRewriter must be absent under NODE_COMPAT=1:\n{stdout}"
    );
}
