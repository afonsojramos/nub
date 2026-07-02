//! Floor-only import-attribute keyword rewrite: `with { … }` → `assert { … }`.
//!
//! Node 18.19.0 / 18.19.1's V8 cannot parse the `with` import-attribute keyword —
//! the grammar landed in V8 12.3 (Node 22) and was backported to 18.20 / 20.10, but
//! NOT to 18.19.x. Those two patch releases sit inside nub's documented 18.19 support
//! floor, so the JS preload rewrites the keyword to the older `assert` spelling —
//! which that V8 parses, and which surfaces the IDENTICAL `importAttributes` to the
//! load hook — before Node's parser ever sees the source. `assert` is REMOVED on
//! Node 22+, so the JS side gates this to 18.19.x only.
//!
//! Every `with`-keyword clause is rewritten regardless of the attribute (`text`,
//! `json`, …): the SYNTAX itself is what 18.19.x cannot parse. The rewrite is an
//! oxc-parse-driven MINIMAL byte splice of just the 4-byte keyword token — a `with`
//! inside a string, comment, or regexp is never touched, and everything but the
//! keyword stays byte-for-byte. `import`, `export … from`, and `export * from` all
//! carry a `WithClause`, so one AST visitor covers them uniformly.

use napi_derive::napi;

use oxc::{
    allocator::Allocator,
    ast::ast::{WithClause, WithClauseKeyword},
    ast_visit::Visit,
    parser::Parser,
};
use oxc_napi::get_source_type;

/// Result of a keyword rewrite. `changed` is false when the source carried no
/// `with`-keyword clause (or could not be parsed), so the caller keeps the original.
#[napi(object)]
pub struct RewriteResult {
    pub code: String,
    pub changed: bool,
}

/// Collects the byte offset of the `{` that opens each `with`-keyword clause.
/// `WithClause.span` covers only the `{ … }` entries block — NOT the keyword — so the
/// keyword is located by scanning back from this offset (see `keyword_start_before_brace`).
/// `assert` clauses already parse on the floor and are left untouched.
struct WithKeywordFinder {
    brace_starts: Vec<u32>,
}

impl<'a> Visit<'a> for WithKeywordFinder {
    fn visit_with_clause(&mut self, it: &WithClause<'a>) {
        if matches!(it.keyword, WithClauseKeyword::With) {
            self.brace_starts.push(it.span.start);
        }
    }
}

const WITH: &[u8] = b"with";
const ASSERT: &str = "assert";

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The `with` keyword sits immediately before the clause's `{`, separated only by
/// whitespace in oxc's emit and in all normal source (`with {`, `with{`, `with  {`).
/// Scan back over ASCII whitespace and return the keyword's start byte iff the four
/// bytes are a standalone `with`. (A comment between the keyword and the `{` is not
/// handled — it does not occur in oxc output and is not seen in real code; such a
/// clause is left as-is and V8 surfaces its own error.)
fn keyword_start_before_brace(src: &[u8], brace: usize) -> Option<usize> {
    let mut i = brace;
    while i > 0 && src[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i >= WITH.len() && &src[i - WITH.len()..i] == WITH {
        let start = i - WITH.len();
        // Reject an identifier that merely ends in `with` (e.g. `foowith {`).
        if start == 0 || !is_ident_byte(src[start - 1]) {
            return Some(start);
        }
    }
    None
}

/// Rewrite `with { … }` import-attribute clauses to `assert { … }`. `lang` selects
/// the parser dialect (`"ts"` parses the JS/TS superset); the source is always parsed
/// as a module (import attributes are ESM-only). A parse panic returns the source
/// unchanged so V8 surfaces the real error at the real spot.
#[allow(clippy::needless_pass_by_value, clippy::allow_attributes)]
#[napi]
pub fn rewrite_import_attributes_keyword(
    filename: String,
    source_text: String,
    lang: Option<String>,
) -> RewriteResult {
    let source_type = get_source_type(&filename, lang.as_deref(), Some("module"));

    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, &source_text, source_type).parse();
    if ret.panicked {
        return RewriteResult {
            code: source_text,
            changed: false,
        };
    }

    let mut finder = WithKeywordFinder {
        brace_starts: Vec::new(),
    };
    finder.visit_program(&ret.program);
    if finder.brace_starts.is_empty() {
        return RewriteResult {
            code: source_text,
            changed: false,
        };
    }

    // Resolve each `{` back to its `with` keyword, then splice from the highest offset
    // down so earlier offsets stay valid. `with` is ASCII, so byte offsets are char
    // boundaries.
    let mut keyword_starts: Vec<usize> = finder
        .brace_starts
        .iter()
        .filter_map(|&b| keyword_start_before_brace(source_text.as_bytes(), b as usize))
        .collect();
    keyword_starts.sort_unstable();
    keyword_starts.dedup();
    if keyword_starts.is_empty() {
        return RewriteResult {
            code: source_text,
            changed: false,
        };
    }
    let mut out = source_text;
    for &start in keyword_starts.iter().rev() {
        out.replace_range(start..start + WITH.len(), ASSERT);
    }
    RewriteResult {
        code: out,
        changed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(src: &str) -> (String, bool) {
        let r = rewrite_import_attributes_keyword("f.mjs".into(), src.into(), Some("ts".into()));
        (r.code, r.changed)
    }

    #[test]
    fn rewrites_default_text_import() {
        let (code, changed) = rewrite("import x from \"./f.txt\" with { type: \"text\" };\n");
        assert!(changed);
        assert_eq!(
            code,
            "import x from \"./f.txt\" assert { type: \"text\" };\n"
        );
    }

    #[test]
    fn rewrites_any_attribute_not_just_text() {
        let (code, changed) = rewrite("import j from \"./d.json\" with { type: \"json\" };");
        assert!(changed);
        assert_eq!(
            code,
            "import j from \"./d.json\" assert { type: \"json\" };"
        );
    }

    #[test]
    fn rewrites_export_from_and_export_all() {
        let (code, changed) = rewrite(
            "export { a } from \"./a.json\" with { type: \"json\" };\n\
             export * from \"./b.json\" with { type: \"json\" };\n",
        );
        assert!(changed);
        assert_eq!(
            code,
            "export { a } from \"./a.json\" assert { type: \"json\" };\n\
             export * from \"./b.json\" assert { type: \"json\" };\n",
        );
    }

    #[test]
    fn rewrites_multiple_clauses_keeping_offsets() {
        let (code, changed) = rewrite(
            "import a from \"./a.txt\" with { type: \"text\" };\n\
             import b from \"./b.txt\" with { type: \"text\" };\n",
        );
        assert!(changed);
        assert_eq!(
            code,
            "import a from \"./a.txt\" assert { type: \"text\" };\n\
             import b from \"./b.txt\" assert { type: \"text\" };\n",
        );
    }

    #[test]
    fn leaves_string_and_comment_with_untouched() {
        // `with` inside a string literal / comment / identifier must survive verbatim;
        // only the real clause keyword is rewritten.
        let src = "const s = \"import x from 'y' with { type }\"; // trailing with {\n\
                   const width = 1;\n\
                   import t from \"./t.txt\" with { type: \"text\" };\n";
        let (code, changed) = rewrite(src);
        assert!(changed);
        assert!(code.contains("const s = \"import x from 'y' with { type }\""));
        assert!(code.contains("// trailing with {"));
        assert!(code.contains("const width = 1;"));
        assert!(code.contains("import t from \"./t.txt\" assert { type: \"text\" };"));
    }

    #[test]
    fn handles_no_space_and_extra_space_before_brace() {
        let (a, ca) = rewrite("import x from \"./f.txt\"with{type:\"text\"};");
        assert!(ca);
        assert_eq!(a, "import x from \"./f.txt\"assert{type:\"text\"};");
        let (b, cb) = rewrite("import y from \"./f.txt\"  with  { type: \"text\" };");
        assert!(cb);
        assert_eq!(b, "import y from \"./f.txt\"  assert  { type: \"text\" };");
    }

    #[test]
    fn no_clause_is_noop() {
        let (code, changed) = rewrite("import x from \"./x.js\";\nconst w = { with: 1 };\n");
        assert!(!changed);
        assert_eq!(code, "import x from \"./x.js\";\nconst w = { with: 1 };\n");
    }

    #[test]
    fn already_assert_is_noop() {
        let (code, changed) = rewrite("import x from \"./f.txt\" assert { type: \"text\" };");
        assert!(!changed);
        assert_eq!(code, "import x from \"./f.txt\" assert { type: \"text\" };");
    }
}
