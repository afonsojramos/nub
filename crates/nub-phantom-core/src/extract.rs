//! Extract import/require specifier OCCURRENCES from one source file, using the
//! same oxc parser nub transpiles with.
//!
//! Every static `import`, runtime re-export, dynamic `import()`, `require()`, and
//! `require.resolve()` with a string-literal argument is recorded, tagged `soft`
//! when it appears lexically inside a runtime GUARD — a `try`/`catch`/`finally`,
//! or a conditional branch (`if`/`else`, a ternary arm, the right of `&&`/`||`).
//! Both are the "loaded only when the environment/condition permits" pattern the
//! spec classifies soft (e.g. `if (typeof phantom !== 'undefined') require('system')`,
//! or `try { require('opt') } catch {}`). A top-level, unconditional require is
//! hard. Type-only imports/exports are dropped — they are erased before runtime
//! and would false-flag a devDep-typed package as a phantom. Dynamic specifiers
//! (template/computed) are not recorded: they cannot be attributed to a package
//! name, and guessing would risk a false positive.
//!
//! Guard-ness is LEXICAL, not control-flow: a require in a function DECLARED
//! inside an `if`/`try` is marked soft even if that function is later called
//! unconditionally. This only ever errs toward soft (a false negative on
//! hardness), never toward a false phantom — safe for the never-false-flag bar.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ConditionalExpression, Expression, IfStatement, ImportDeclarationSpecifier,
    LogicalExpression, Statement, TryStatement,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// How a specifier was referenced. Kept for reporting; the classifier treats all
/// as runtime edges (soft-ness, not kind, gates the phantom/soft split).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    StaticImport,
    ReExport,
    DynamicImport,
    Require,
    RequireResolve,
}

/// One reference to a specifier in a file.
#[derive(Debug, Clone)]
pub struct Occurrence {
    /// The raw specifier string, exactly as written (`./x`, `lodash/fp`, `node:fs`).
    pub spec: String,
    /// Inside a `try`/`catch`/`finally` — a guarded (soft) load.
    pub soft: bool,
    pub kind: RefKind,
}

/// Parse `source` (its `SourceType` inferred from `path`) and return every
/// specifier occurrence. A parse that panics yields an empty list (the file is
/// simply not analyzed rather than aborting the package).
///
/// This is the BASELINE (full guard-aware AST visit) path. [`extract_optimized`]
/// is the production entry — it applies a cost ladder that reaches this same
/// full walk only for files that can actually carry a guarded (CJS) load.
pub fn extract(path: &str, source: &str) -> Vec<Occurrence> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_else(|_| SourceType::mjs());
    let ret = Parser::new(&allocator, source, source_type).parse();
    if ret.panicked {
        return Vec::new();
    }
    let mut v = SpecVisitor {
        guard_depth: 0,
        out: Vec::new(),
    };
    v.visit_program(&ret.program);
    v.out
}

/// The static-imports-only cost ladder — same outputs as [`extract`] on the
/// phantom-relevant dimension (every recorded specifier + its correct `soft`
/// flag) for all ordinary code, reached by the cheapest rung complete for the
/// file's shape. INVESTIGATED + MEASURED but NOT the production path — it
/// regresses the scan (parse+IO-dominated, CJS-heavy reachable graphs), so
/// production uses [`extract`] (see `nub-phantom-scan::graph::extract_file`).
///
/// Residual known gap (why not "identical for EVERY input"): rung routing keys on
/// a BYTE scan for a dynamic `import(` call ([`has_dynamic_import_call`]), which
/// recognizes ASCII whitespace between `import` and `(` but NOT a comment or an
/// exotic Unicode separator (`\u{A0}`, `\u{FEFF}`) there — `import/*c*/("x")`
/// would route to the static-ESM rung and be dropped. Such forms are
/// near-nonexistent in published code and the ladder is unused in production, so
/// this is documented rather than fully closed.
///
/// The insight (why this is sound, not a heuristic): a `soft` (guarded) load can
/// only occur inside a `try`/`catch` or a conditional branch, and the ONLY module
/// forms that can be nested inside those are runtime CALLS — `require(...)`,
/// `require.resolve(...)`, and dynamic `import(...)`. Static ESM `import ... from`
/// / `export ... from` declarations are hoisted and, by the language grammar,
/// exist ONLY at the top level of a module — they can never be guarded, so they
/// are always hard. So:
///
/// - **(a) byte pre-filter** — a source with none of the substrings `import`,
///   `require`, `from` has no module edge at all → return empty, no parse.
/// - **(b) static-ESM fast path** — a source with no `require` substring and no
///   dynamic-`import(` call has ONLY top-level static declarations. Parse once
///   (oxc, cheap) but skip the deep AST visit: scan `program.body` at the top
///   level. Complete by the grammar rule above, and every hit is hard.
/// - **(c) guard-aware full walk** — a source containing `require` or a dynamic
///   `import(` may carry a guarded load, so it needs the full guard-modeling
///   visit ([`extract`]). This is the ONLY rung whose precision (`soft` vs hard)
///   depends on control-flow nesting, and it is reserved for exactly the files
///   that can express it.
pub fn extract_optimized(path: &str, source: &str) -> Vec<Occurrence> {
    // (a) No module-edge substring anywhere → nothing to record. `from` catches
    // `export … from` / `import … from`; `import`/`require` catch the rest
    // (bare imports, dynamic imports, requires, require.resolve).
    if !contains_module_edge_bytes(source) {
        return Vec::new();
    }

    // (c) Anything that could carry a GUARDED (soft) load — a `require`/
    // `require.resolve` (CJS, the only form guardable in try/catch) or a dynamic
    // `import(` call — takes the full guard-aware visit. `has_dynamic_import_call`
    // distinguishes the dynamic `import(` call from a static `import … from`.
    if source.contains("require") || has_dynamic_import_call(source) {
        return extract(path, source);
    }

    // (b) Static-ESM only. Parse, then read top-level declarations without the
    // deep recursive walk — sound because static import/export-from are
    // grammatically top-level-only, and all are unguarded (hard).
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_else(|_| SourceType::mjs());
    let ret = Parser::new(&allocator, source, source_type).parse();
    if ret.panicked {
        return Vec::new();
    }
    let mut out = Vec::new();
    for stmt in &ret.program.body {
        record_toplevel_static(stmt, &mut out);
    }
    out
}

/// Byte pre-filter for rung (a): true if `source` contains any substring that a
/// static or runtime module edge must contain. Conservative — only a source with
/// NONE of these can be skipped without a parse.
fn contains_module_edge_bytes(source: &str) -> bool {
    source.contains("import") || source.contains("require") || source.contains("from")
}

/// True if `source` contains a dynamic `import(` CALL (as opposed to only static
/// `import … from` declarations). A dynamic import call is the token `import`
/// immediately followed (modulo insignificant whitespace) by `(`. This is a byte
/// scan, not a parse: it only decides whether the guard-aware rung (c) is needed;
/// a false positive merely routes the file to the (still-correct) full walk.
fn has_dynamic_import_call(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut i = 0;
    while let Some(pos) = source[i..].find("import") {
        let start = i + pos;
        let mut j = start + "import".len();
        // ASCII whitespace incl. CRLF (`\r`), vertical tab (`\x0b`), form feed
        // (`\x0c`) — the realistic separators. (Comment / Unicode-space
        // separators are the documented residual gap.)
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'(' {
            return true;
        }
        i = start + "import".len();
    }
    false
}

/// Record a top-level static `import`/`export … from` declaration (rung (b)).
/// Mirrors the import/re-export arms of [`SpecVisitor::visit_statement`] but does
/// NOT recurse — the caller iterates `program.body` directly. Every occurrence is
/// unguarded (`soft: false`) because a static declaration cannot be nested in a
/// guard.
fn record_toplevel_static(stmt: &Statement<'_>, out: &mut Vec<Occurrence>) {
    match stmt {
        Statement::ImportDeclaration(decl)
            if !decl.import_kind.is_type() && import_has_value(decl) =>
        {
            out.push(Occurrence {
                spec: decl.source.value.to_string(),
                soft: false,
                kind: RefKind::StaticImport,
            });
        }
        Statement::ExportNamedDeclaration(decl) => {
            if let Some(src) = &decl.source
                && !decl.export_kind.is_type()
                && export_named_has_value(decl)
            {
                out.push(Occurrence {
                    spec: src.value.to_string(),
                    soft: false,
                    kind: RefKind::ReExport,
                });
            }
        }
        Statement::ExportAllDeclaration(decl) if !decl.export_kind.is_type() => {
            out.push(Occurrence {
                spec: decl.source.value.to_string(),
                soft: false,
                kind: RefKind::ReExport,
            });
        }
        _ => {}
    }
}

struct SpecVisitor {
    /// Lexical nesting inside a try/catch/finally OR a conditional branch. A
    /// nonzero depth means the current position is only reached under a runtime
    /// guard → any load here is soft.
    guard_depth: u32,
    out: Vec<Occurrence>,
}

impl SpecVisitor {
    fn record(&mut self, spec: &str, kind: RefKind) {
        self.out.push(Occurrence {
            spec: spec.to_string(),
            soft: self.guard_depth > 0,
            kind,
        });
    }
}

impl<'a> Visit<'a> for SpecVisitor {
    fn visit_try_statement(&mut self, it: &TryStatement<'a>) {
        // Everything lexically within a try — the try block, the catch handler,
        // and the finalizer — is a guarded region: the canonical optional-load
        // pattern is `try { x = require('opt') } catch { x = fallback }`.
        self.guard_depth += 1;
        walk::walk_try_statement(self, it);
        self.guard_depth -= 1;
    }

    fn visit_if_statement(&mut self, it: &IfStatement<'a>) {
        // The TEST runs unconditionally; the branches do not. Guard only the
        // branch bodies so `if (typeof x !== 'undefined') require('x')` is soft
        // while a require in the test itself stays hard.
        self.visit_expression(&it.test);
        self.guard_depth += 1;
        self.visit_statement(&it.consequent);
        if let Some(alt) = &it.alternate {
            self.visit_statement(alt);
        }
        self.guard_depth -= 1;
    }

    fn visit_conditional_expression(&mut self, it: &ConditionalExpression<'a>) {
        // Ternary: test unconditional, arms guarded.
        self.visit_expression(&it.test);
        self.guard_depth += 1;
        self.visit_expression(&it.consequent);
        self.visit_expression(&it.alternate);
        self.guard_depth -= 1;
    }

    fn visit_logical_expression(&mut self, it: &LogicalExpression<'a>) {
        // `a && require('x')` / `a || require('x')`: the left runs
        // unconditionally, the right is short-circuit-guarded.
        self.visit_expression(&it.left);
        self.guard_depth += 1;
        self.visit_expression(&it.right);
        self.guard_depth -= 1;
    }

    fn visit_statement(&mut self, it: &Statement<'a>) {
        match it {
            // Static ESM import. `import type …` (declaration-level) is erased;
            // an inline all-`type` named import is likewise erased. A bare
            // `import "x"` (side effect) and any value specifier are runtime.
            Statement::ImportDeclaration(decl)
                if !decl.import_kind.is_type() && import_has_value(decl) =>
            {
                self.record(&decl.source.value, RefKind::StaticImport);
            }
            // Runtime re-exports. `export { x } from 'y'` (value) and
            // `export * from 'y'` load the module; `export type { … }` does not.
            Statement::ExportNamedDeclaration(decl) => {
                if let Some(src) = &decl.source
                    && !decl.export_kind.is_type()
                    && export_named_has_value(decl)
                {
                    self.record(&src.value, RefKind::ReExport);
                }
            }
            Statement::ExportAllDeclaration(decl) if !decl.export_kind.is_type() => {
                self.record(&decl.source.value, RefKind::ReExport);
            }
            _ => {}
        }
        walk::walk_statement(self, it);
    }

    fn visit_expression(&mut self, it: &Expression<'a>) {
        match it {
            // Dynamic `import("x")` — only a string-literal argument is
            // attributable to a package; template/computed sources are skipped.
            Expression::ImportExpression(imp) => {
                if let Expression::StringLiteral(s) = &imp.source {
                    self.record(&s.value, RefKind::DynamicImport);
                }
            }
            // `require("x")` / `require.resolve("x")`.
            Expression::CallExpression(call) => {
                if let Some((spec, kind)) = require_call(call) {
                    self.record(spec, kind);
                }
            }
            _ => {}
        }
        walk::walk_expression(self, it);
    }
}

/// A named import declaration is a runtime (value) import unless every specifier
/// is inline-`type`. A bare import (no specifiers) is always runtime.
fn import_has_value(decl: &oxc_ast::ast::ImportDeclaration<'_>) -> bool {
    match &decl.specifiers {
        None => true,
        Some(specs) if specs.is_empty() => true,
        Some(specs) => specs.iter().any(|s| match s {
            ImportDeclarationSpecifier::ImportSpecifier(named) => !named.import_kind.is_type(),
            // default / namespace imports are always value bindings
            _ => true,
        }),
    }
}

/// `export { a, type B } from 'y'` is a runtime re-export unless every specifier
/// is inline-`type`.
fn export_named_has_value(decl: &oxc_ast::ast::ExportNamedDeclaration<'_>) -> bool {
    decl.specifiers.is_empty() || decl.specifiers.iter().any(|s| !s.export_kind.is_type())
}

/// If `call` is `require("lit")` or `require.resolve("lit")`, return the literal
/// specifier and which. Any non-string-literal argument yields `None`.
fn require_call<'a>(call: &'a oxc_ast::ast::CallExpression<'a>) -> Option<(&'a str, RefKind)> {
    let kind = match &call.callee {
        Expression::Identifier(id) if id.name == "require" => RefKind::Require,
        Expression::StaticMemberExpression(m) => match &m.object {
            Expression::Identifier(id) if id.name == "require" && m.property.name == "resolve" => {
                RefKind::RequireResolve
            }
            _ => return None,
        },
        _ => return None,
    };
    match call.arguments.first() {
        Some(Argument::StringLiteral(s)) => Some((&s.value, kind)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{RefKind, extract};

    fn specs(src: &str) -> Vec<(String, bool, RefKind)> {
        extract("mod.mjs", src)
            .into_iter()
            .map(|o| (o.spec, o.soft, o.kind))
            .collect()
    }

    #[test]
    fn captures_static_import_require_dynamic_and_reexport() {
        let got = specs(
            r#"
            import a from "aa";
            import "side-effect";
            export { z } from "zz";
            export * from "ss";
            const b = require("bb");
            const c = await import("cc");
            require.resolve("dd");
            "#,
        );
        let names: Vec<_> = got.iter().map(|(s, _, _)| s.as_str()).collect();
        assert!(names.contains(&"aa"));
        assert!(names.contains(&"side-effect"));
        assert!(names.contains(&"zz"));
        assert!(names.contains(&"ss"));
        assert!(names.contains(&"bb"));
        assert!(names.contains(&"cc"));
        assert!(names.contains(&"dd"));
    }

    #[test]
    fn try_catch_require_is_soft_toplevel_is_hard() {
        let got = specs(
            r#"
            const hard = require("hard-dep");
            let opt;
            try { opt = require("soft-dep"); } catch {}
            "#,
        );
        let hard = got.iter().find(|(s, _, _)| s == "hard-dep").unwrap();
        let soft = got.iter().find(|(s, _, _)| s == "soft-dep").unwrap();
        assert!(!hard.1, "top-level require must be hard");
        assert!(soft.1, "require inside try must be soft");
    }

    #[test]
    fn conditionally_guarded_requires_are_soft() {
        // The esprima→'system' shape: a require inside a `typeof` env-guard `if`
        // branch, plus `&&` and ternary guards. All soft; the top-level one hard.
        let got = specs(
            r#"
            const hard = require("always");
            if (typeof phantom !== "undefined") { var s = require("system"); }
            const c = cond && require("andguard");
            const t = flag ? require("ternguard") : null;
            "#,
        );
        let soft = |n: &str| got.iter().find(|(s, _, _)| s == n).unwrap().1;
        assert!(!soft("always"), "top-level require is hard");
        assert!(soft("system"), "require in if-branch is soft");
        assert!(soft("andguard"), "require in && right is soft");
        assert!(soft("ternguard"), "require in ternary arm is soft");
    }

    #[test]
    fn type_only_imports_are_dropped() {
        // Inline `type` modifiers are TS syntax — parse as .mts.
        let got: Vec<_> = extract(
            "mod.mts",
            r#"
            import type { T } from "type-pkg";
            import { type Only } from "inline-type-pkg";
            import { value, type Also } from "mixed-pkg";
            "#,
        )
        .into_iter()
        .map(|o| (o.spec, o.soft, o.kind))
        .collect();
        let names: Vec<_> = got.iter().map(|(s, _, _)| s.as_str()).collect();
        assert!(!names.contains(&"type-pkg"), "import type erased");
        assert!(
            !names.contains(&"inline-type-pkg"),
            "all-inline-type erased"
        );
        assert!(names.contains(&"mixed-pkg"), "mixed import is runtime");
    }

    #[test]
    fn computed_specifiers_are_not_recorded() {
        let got = specs(
            r#"
            const x = require("pre" + "fix");
            const y = await import(`tpl${a}`);
            require(dynamicVar);
            "#,
        );
        assert!(got.is_empty(), "no attributable string literal → skip");
    }

    /// The optimization invariant: `extract_optimized` must record the SAME
    /// (spec, soft, kind) multiset as the baseline `extract` for every input,
    /// regardless of which rung it takes. This is what lets the fast rungs stand
    /// in for the full walk without changing a single phantom verdict.
    #[test]
    fn optimized_matches_baseline_across_all_shapes() {
        use super::extract_optimized;
        // One representative per rung + the tricky guard/type/dynamic cases.
        let cases: &[(&str, &str)] = &[
            // rung (a): no module edge at all.
            ("a.mjs", "const x = 1 + 2; export const y = fromNowhere(x);"),
            // rung (b): static ESM only (import/export-from, bare, type-mixed).
            (
                "b.mts",
                r#"import a from "aa";
                   import "side-effect";
                   export { z } from "zz";
                   export * from "ss";
                   import type { T } from "type-pkg";
                   import { type Only } from "inline-type-pkg";
                   import { value, type Also } from "mixed-pkg";"#,
            ),
            // rung (c): require + require.resolve + guards + dynamic import.
            (
                "c.js",
                r#"const hard = require("hard-dep");
                   let opt; try { opt = require("soft-dep"); } catch {}
                   if (typeof p !== "undefined") { require("system"); }
                   const t = flag ? require("tern") : null;
                   const a = cond && require("andg");
                   require.resolve("rr");
                   const d = await import("dyn-lit");
                   require("pre" + "fix");
                   import(`tpl${x}`);"#,
            ),
            // rung (c) via a lone dynamic import call, no require present.
            (
                "d.mjs",
                r#"import base from "base-pkg";
                   const m = await import("dyn-only");
                   if (cond) { const g = await import("guarded-dyn"); }"#,
            ),
            // CRLF + form-feed between `import` and `(` must still route to
            // rung (c) (regression guard for the has_dynamic_import_call ws set).
            ("e.mjs", "const x = import\r\n(\"dyn-crlf\");"),
            ("f.mjs", "const y = import\u{0c}(\"dyn-ff\");"),
        ];
        let sorted = |mut v: Vec<(String, bool, RefKind)>| {
            v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
            v
        };
        for (path, src) in cases {
            let base = sorted(
                extract(path, src)
                    .into_iter()
                    .map(|o| (o.spec, o.soft, o.kind))
                    .collect(),
            );
            let opt = sorted(
                extract_optimized(path, src)
                    .into_iter()
                    .map(|o| (o.spec, o.soft, o.kind))
                    .collect(),
            );
            assert_eq!(base, opt, "rung parity mismatch for {path}");
        }
    }
}
