//! The net-new oxc `VisitMut` pass that rewrites a STRING-LITERAL `new Worker(...)`
//! specifier to resolve like a top-level `import` against the COMPILING module —
//! instead of `node:worker_threads`' cwd-relative behavior.
//!
//! Run from `Compiler::transform` (transform.rs) AFTER the TS transform, so it has
//! the live post-transform `Scoping`. It fires ONLY when:
//!   * the callee is the bare identifier `Worker` whose reference is FREE/global
//!     (`symbol_id().is_none()`) — i.e. the browser-`Worker` global the polyfill
//!     installs, NOT an `import { Worker } from "node:worker_threads"` (a BOUND
//!     reference) nor a shadowed local. Rewriting the worker_threads `Worker`
//!     (genuinely cwd-relative) would be an additivity regression, so the scope
//!     guard is load-bearing.
//!   * the first argument is a `StringLiteral`.
//!   * the emit target is ESM (gated by the caller — `import.meta` is invalid in
//!     CJS output).
//!
//! HYBRID resolution, splitting on nub's existing resolver boundary:
//!   * nub-owned (relative / extensionless / tsconfig `paths` → `resolve_ts_core`
//!     returns `Some`): re-relativize the absolute target back to the compiling
//!     module and emit `new Worker(new URL("<relative>", import.meta.url))` — the
//!     bundler-analyzable, deploy-portable form.
//!   * bare package (`resolve_ts_core` returns `None` AND the specifier is bare):
//!     emit `new Worker(import.meta.resolve("<bare>"))` — Node's own resolver owns
//!     node_modules / `exports` / conditions at runtime (no forbidden Rust reimpl).
//!
//! SOUNDNESS: passthrough (absolute / `file:` / `data:` / `blob:` / any non-string
//! arg) is left byte-identical, and a nub-owned target that cannot be relativized
//! (cross-root) is LEFT UNTOUCHED — the rewrite NEVER bakes an absolute path.

use oxc::ast::ast::{Argument, Expression, NewExpression, Program};
use oxc::ast::{AstBuilder, NONE};
use oxc::ast_visit::VisitMut;
use oxc::semantic::Scoping;
use oxc::span::SPAN;

use crate::resolve::resolve_worker_target;

/// Rewrite eligible `new Worker("…")` specifiers in `program`. `file_path` is the
/// compiling module's absolute path (the resolver referrer); `file_dir` its
/// directory (the relativize base). No-op unless a `Worker` global call with a
/// string literal is present.
pub fn rewrite_worker_specifiers<'a>(
    ast: AstBuilder<'a>,
    program: &mut Program<'a>,
    scoping: &Scoping,
    file_path: &str,
    file_dir: &str,
) {
    let mut visitor = WorkerRewrite {
        ast,
        scoping,
        file_path,
        file_dir,
    };
    visitor.visit_program(program);
}

struct WorkerRewrite<'a, 'b> {
    ast: AstBuilder<'a>,
    scoping: &'b Scoping,
    file_path: &'b str,
    file_dir: &'b str,
}

impl<'a> VisitMut<'a> for WorkerRewrite<'a, '_> {
    fn visit_new_expression(&mut self, it: &mut NewExpression<'a>) {
        // Recurse first so nested `new Worker(...)` (e.g. inside an argument) is
        // also visited; the rewrite below only touches THIS node's first arg.
        oxc::ast_visit::walk_mut::walk_new_expression(self, it);

        if !self.is_free_global_worker(&it.callee) {
            return;
        }
        let Some(Argument::StringLiteral(lit)) = it.arguments.first() else {
            return;
        };
        let specifier = lit.value.to_string();

        if let Some(replacement) = self.resolve_replacement(&specifier) {
            it.arguments[0] = Argument::from(replacement);
        }
    }
}

impl<'a> WorkerRewrite<'a, '_> {
    /// The callee is the bare identifier `Worker` AND its reference is unresolved
    /// (free/global). A bound reference — `import { Worker } from
    /// "node:worker_threads"`, a `const Worker = …`, a param — has a `symbol_id`
    /// and is SKIPPED.
    fn is_free_global_worker(&self, callee: &Expression<'a>) -> bool {
        let Expression::Identifier(ident) = callee else {
            return false;
        };
        if ident.name.as_str() != "Worker" {
            return false;
        }
        match ident.reference_id.get() {
            Some(ref_id) => self.scoping.get_reference(ref_id).symbol_id().is_none(),
            // No reference id resolved ⇒ treat as not-eligible (conservative).
            None => false,
        }
    }

    /// Build the replacement first-argument expression for `specifier`, or `None`
    /// to leave the node untouched (passthrough / unresolvable / cross-root).
    fn resolve_replacement(&self, specifier: &str) -> Option<Expression<'a>> {
        // Passthrough: already cwd-independent or an inline source. The same
        // discrimination resolve_ts_core encodes — short-circuit here so a `file:`
        // absolute etc. is never rewritten.
        if is_passthrough(specifier) {
            return None;
        }

        match resolve_worker_target(specifier, self.file_path) {
            // nub-owned: re-relativize → `new URL("<rel>", import.meta.url)`.
            Some(abs) => {
                let rel = nub_worker_resolve::relativize_for_url(self.file_dir, &abs)?;
                Some(self.build_new_url(&rel))
            }
            // Node-owned bare package → `import.meta.resolve("<bare>")`. A non-bare
            // `None` (e.g. a relative path that simply doesn't exist on disk) is
            // left untouched.
            None if nub_worker_resolve::is_bare_specifier(specifier) => {
                Some(self.build_import_meta_resolve(specifier))
            }
            None => None,
        }
    }

    /// `new URL("<rel>", import.meta.url)`.
    fn build_new_url(&self, rel: &str) -> Expression<'a> {
        let callee = self.ast.expression_identifier(SPAN, "URL");
        let arg_str = self
            .ast
            .expression_string_literal(SPAN, self.ast.str(rel), None);
        let import_meta_url = self.import_meta_member("url");
        let args = self
            .ast
            .vec_from_array([Argument::from(arg_str), Argument::from(import_meta_url)]);
        self.ast.expression_new(SPAN, callee, NONE, args)
    }

    /// `import.meta.resolve("<bare>")`.
    fn build_import_meta_resolve(&self, bare: &str) -> Expression<'a> {
        let callee = self.import_meta_member("resolve");
        let arg = self
            .ast
            .expression_string_literal(SPAN, self.ast.str(bare), None);
        let args = self.ast.vec1(Argument::from(arg));
        self.ast.expression_call(SPAN, callee, NONE, args, false)
    }

    /// `import.meta.<prop>` (a static member access on the `import.meta` meta
    /// property).
    fn import_meta_member(&self, prop: &str) -> Expression<'a> {
        let meta = self.ast.identifier_name(SPAN, "import");
        let property = self.ast.identifier_name(SPAN, "meta");
        let import_meta = self.ast.expression_meta_property(SPAN, meta, property);
        let prop_name = self.ast.identifier_name(SPAN, self.ast.str(prop));
        Expression::from(
            self.ast
                .member_expression_static(SPAN, import_meta, prop_name, false),
        )
    }
}

/// A specifier that is already cwd-independent (absolute / `file:`) or an inline
/// source (`data:` / `blob:`), or a degenerate non-specifier — never rewritten.
fn is_passthrough(specifier: &str) -> bool {
    // Degenerate: empty, whitespace-only, or a bare `.`/`..` directory reference.
    // These are not real worker specifiers; leave them to Node rather than emit a
    // nonsensical `import.meta.resolve("")`.
    if specifier.trim().is_empty() || specifier == "." || specifier == ".." {
        return true;
    }
    specifier.starts_with('/')
        || std::path::Path::new(specifier).is_absolute()
        || specifier.starts_with("file:")
        || specifier.starts_with("data:")
        || specifier.starts_with("blob:")
}
