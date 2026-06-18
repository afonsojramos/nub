//! Eager .env* loading with workspace walk-up and ${VAR} expansion.
//! The parser follows Node's `--env-file` grammar; expansion stays in Nub's
//! intentional post-parse layer.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Max size of an env file we read into memory (16 MiB). Real env files are
/// KB-sized; this caps an absurdly large regular file.
const ENV_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Read an env file's contents, refusing anything that is not a regular file or
/// that exceeds the size cap, then read it. This guards against `read_to_string`
/// hanging or OOMing on a character device (`--env-file=/dev/zero`), a FIFO, or
/// a pathological file: `/dev/zero` reports size 0 yet streams forever, so the
/// `is_file` check — not the size cap — is what stops it. `metadata` follows
/// symlinks, so a `.env` symlinked to a device is rejected by its target.
/// Returns `None` on any guard failure or read error (caller treats it as an
/// absent/unreadable file).
pub fn read_env_file(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > ENV_FILE_MAX_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// The `.env*` filenames Nub loads, in descending priority order (the file
/// listed first wins a key over later ones). Driven by `NODE_ENV`, matching
/// Node's own `.env` precedence. Shared by [`load_env_files`] (first-writer-wins
/// merge) and [`discover_env_files`] (the watch path's `--env-file` args).
fn env_file_names() -> Vec<String> {
    let node_env = std::env::var("NODE_ENV").unwrap_or_default();
    let is_test = node_env == "test";

    let mut files = Vec::new();
    if !node_env.is_empty() {
        files.push(format!(".env.{node_env}.local"));
    }
    if !is_test {
        files.push(".env.local".to_string());
    }
    if !node_env.is_empty() {
        files.push(format!(".env.{node_env}"));
    }
    files.push(".env".to_string());
    files
}

/// The existing `.env*` file paths under `project_root`, in descending priority
/// order (highest-priority first — same order as [`load_env_files`]'s merge).
/// Used by `nub watch` to hand `--env-file=<path>` args to the watched Node so
/// Node watches and re-reads them across restarts, rather than freezing their
/// values at parent-spawn time. Only paths that currently exist and read as
/// regular files are returned, so a caller passing them to Node's `--env-file`
/// (which errors on a missing file) won't hit a spurious not-found.
///
/// NOTE — precedence inversion: Node's `--env-file` is *last*-writer-wins, the
/// inverse of this list's *first*-writer-wins order, so the caller must pass
/// these to Node in reverse for the priorities to line up.
pub fn discover_env_files(project_root: &Path) -> Vec<std::path::PathBuf> {
    env_file_names()
        .iter()
        .map(|name| project_root.join(name))
        .filter(|path| read_env_file(path).is_some())
        .collect()
}

/// Expand `${VAR}` and `$VAR` references within all values of a map, in-place.
/// Multi-pass (up to 10 rounds) to resolve nested chains like `A=hello`,
/// `B=${A}_world`, `C=${B}_!`. Undefined references resolve to the empty string
/// (consistent with [`load_env_files`]). Mutates `map` in-place and returns it
/// for easy chaining.
pub fn expand_env_map(map: &mut HashMap<String, String>) -> &mut HashMap<String, String> {
    for _ in 0..10 {
        let snapshot = map.clone();
        let mut changed = false;
        for value in map.values_mut() {
            let expanded = expand_vars(value, &snapshot);
            if expanded != *value {
                *value = expanded;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    map
}

/// Load .env* files from the project root, returning the key-value
/// pairs to inject into the child process environment. Shell env
/// (from the parent process) always wins — values already set in
/// the process environment are not overridden.
pub fn load_env_files(project_root: &Path) -> HashMap<String, String> {
    let files = env_file_names();

    let mut result = HashMap::new();

    for filename in &files {
        let path = project_root.join(filename);
        if let Some(content) = read_env_file(&path) {
            for (key, value) in parse_env(&content) {
                // Shell env wins: don't override existing env vars.
                if std::env::var_os(&key).is_some() {
                    continue;
                }
                // First writer wins among .env files.
                result.entry(key).or_insert(value);
            }
        }
    }

    // Expand ${VAR} references within values. Multi-pass to handle
    // nested references like A=hello, B=${A}_world, C=${B}_!.
    expand_env_map(&mut result);

    result
}

/// Parse a .env file with Node-`--env-file`-compatible semantics.
///
/// Node only treats quotes as syntax when the trimmed value starts with `'`,
/// `"`, or `` ` ``. Regular unquoted values are otherwise copied up to the
/// newline or inline `#` comment and then trimmed, so JSON-looking values keep
/// their inner quotes and backslash escapes. Later keys override earlier ones
/// (Node's `insert_or_assign` / last-writer-wins), preserving first-seen order
/// for callers that fold these pairs into a `HashMap`.
pub fn parse_env(content: &str) -> Vec<(String, String)> {
    // Node removes carriage returns before scanning.
    let content = content.replace('\r', "");
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let mut rest = trim_env_spaces(content);

    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();

    while !rest.is_empty() {
        if rest.starts_with('\n') || rest.starts_with('#') {
            rest = trim_env_spaces(skip_line(rest));
            continue;
        }

        let Some(equal_or_newline) = rest.find(['=', '\n']) else {
            break;
        };
        if rest.as_bytes()[equal_or_newline] == b'\n' {
            rest = trim_env_spaces(&rest[equal_or_newline + 1..]);
            continue;
        }

        let mut key = trim_env_spaces(&rest[..equal_or_newline]);
        rest = &rest[equal_or_newline + 1..];
        if let Some(stripped) = key.strip_prefix("export ") {
            key = trim_env_spaces(stripped);
        }
        if key.is_empty() {
            rest = trim_env_spaces(skip_line(rest));
            continue;
        }

        if rest.is_empty() || rest.starts_with('\n') {
            upsert_env_pair(&mut pairs, &mut seen, key.to_string(), String::new());
            rest = match rest.find('\n') {
                Some(newline) => trim_env_spaces(&rest[newline + 1..]),
                None => "",
            };
            continue;
        }

        rest = trim_env_spaces(rest);
        if rest.is_empty() {
            upsert_env_pair(&mut pairs, &mut seen, key.to_string(), String::new());
            break;
        }

        if rest.starts_with('"') {
            if let Some(closing_quote) = closing_quote(rest, '"') {
                let value = rest[1..closing_quote].replace("\\n", "\n");
                upsert_env_pair(&mut pairs, &mut seen, key.to_string(), value);
                rest = trim_env_spaces(after_value_line(rest, closing_quote + 1));
                continue;
            }
        }

        if let Some(quote) = leading_quote(rest) {
            if let Some(closing_quote) = closing_quote(rest, quote) {
                let value = rest[1..closing_quote].to_string();
                upsert_env_pair(&mut pairs, &mut seen, key.to_string(), value);
                rest = trim_env_spaces(after_value_line(rest, closing_quote + 1));
                continue;
            }

            let (line, next) = split_line(rest);
            upsert_env_pair(&mut pairs, &mut seen, key.to_string(), line.to_string());
            rest = trim_env_spaces(next);
            continue;
        }

        let (line, next) = split_line(rest);
        let value = line.split_once('#').map(|(value, _)| value).unwrap_or(line);
        upsert_env_pair(
            &mut pairs,
            &mut seen,
            key.to_string(),
            trim_env_spaces(value).to_string(),
        );
        rest = trim_env_spaces(next);
    }

    pairs
}

fn trim_env_spaces(input: &str) -> &str {
    input.trim_matches(|c| matches!(c, ' ' | '\t' | '\n'))
}

fn skip_line(input: &str) -> &str {
    split_line(input).1
}

fn split_line(input: &str) -> (&str, &str) {
    match input.find('\n') {
        Some(newline) => (&input[..newline], &input[newline + 1..]),
        None => (input, ""),
    }
}

fn after_value_line(input: &str, from: usize) -> &str {
    input[from..]
        .find('\n')
        .map(|newline| &input[from + newline + 1..])
        .unwrap_or("")
}

fn leading_quote(input: &str) -> Option<char> {
    match input.as_bytes().first().copied() {
        Some(b'\'') => Some('\''),
        Some(b'"') => Some('"'),
        Some(b'`') => Some('`'),
        _ => None,
    }
}

fn closing_quote(input: &str, quote: char) -> Option<usize> {
    input[1..].find(quote).map(|idx| idx + 1)
}

fn upsert_env_pair(
    pairs: &mut Vec<(String, String)>,
    seen: &mut HashMap<String, usize>,
    key: String,
    value: String,
) {
    if let Some(&idx) = seen.get(&key) {
        pairs[idx].1 = value;
    } else {
        seen.insert(key.clone(), pairs.len());
        pairs.push((key, value));
    }
}

/// Expand `${VAR}` and `$VAR` references in a value.
fn expand_vars(value: &str, env: &HashMap<String, String>) -> String {
    let mut result = String::new();
    let chars: Vec<char> = value.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == '$' {
            result.push('$');
            i += 2;
            continue;
        }

        if chars[i] == '$' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                // ${VAR} form
                if let Some(close) = chars[i + 2..].iter().position(|&c| c == '}') {
                    let var_name: String = chars[i + 2..i + 2 + close].iter().collect();
                    let resolved = env
                        .get(&var_name)
                        .cloned()
                        .or_else(|| std::env::var(&var_name).ok())
                        .unwrap_or_default();
                    result.push_str(&resolved);
                    i += close + 3;
                    continue;
                }
            } else if i + 1 < chars.len() && chars[i + 1].is_ascii_alphabetic() {
                // $VAR form
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                let var_name: String = chars[start..end].iter().collect();
                let resolved = env
                    .get(&var_name)
                    .cloned()
                    .or_else(|| std::env::var(&var_name).ok())
                    .unwrap_or_default();
                result.push_str(&resolved);
                i = end;
                continue;
            }
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_env() {
        let pairs = parse_env("FOO=bar\nBAZ=qux\n");
        assert_eq!(
            pairs,
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("BAZ".to_string(), "qux".to_string()),
            ]
        );
    }

    #[test]
    fn parse_quoted_values() {
        let pairs = parse_env("A=\"hello world\"\nB='single'\n");
        assert_eq!(pairs[0].1, "hello world");
        assert_eq!(pairs[1].1, "single");
    }

    #[test]
    fn unquoted_json_value_is_verbatim() {
        let pairs = parse_env("FOO={\"field\":\"line1\\nline2\"}\n");
        let value = pairs
            .iter()
            .find(|(key, _)| key == "FOO")
            .map(|(_, value)| value.as_str());
        assert_eq!(value, Some("{\"field\":\"line1\\nline2\"}"));
    }

    /// Node's `--env-file` treats backticks as a third quote style alongside
    /// `'` and `"` (`src/node_dotenv.cc`): the surrounding backticks are
    /// stripped and the content is taken verbatim. dotenvy alone leaves the
    /// backticks in the value, so [`parse_env`] must close the gap. Covers all
    /// three quote styles plus the empty-backtick case the regression flagged
    /// (`parallel/test-dotenv.js` BACKTICKS / EMPTY_BACKTICKS). Reference
    /// values were captured from node-v25.8.1's `--env-file` on this fixture.
    #[test]
    fn strips_surrounding_quotes_for_single_double_and_backtick() {
        let pairs = parse_env(concat!(
            "SQ='hi'\n",
            "DQ=\"hi\"\n",
            "BT=`hi`\n",
            "EMPTY_BT=``\n",
            "SPACED_BT=`    pad    `\n",
        ));
        let get = |k: &str| pairs.iter().find(|(p, _)| p == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SQ"), Some("hi"));
        assert_eq!(get("DQ"), Some("hi"));
        assert_eq!(get("BT"), Some("hi"), "backtick value must be unwrapped");
        assert_eq!(
            get("EMPTY_BT"),
            Some(""),
            "empty backticks must yield an empty string, not ``"
        );
        assert_eq!(
            get("SPACED_BT"),
            Some("    pad    "),
            "interior whitespace inside backticks is preserved verbatim"
        );
    }

    /// Backtick content is verbatim the way Node's parser is: no `$`
    /// substitution, no `\n` unescaping, inner quotes retained, a trailing
    /// inline comment after the closing backtick stripped, and the value may
    /// span newlines until the closing backtick. These are the exact cases in
    /// `test/fixtures/dotenv/valid.env`; values match node-v25.8.1.
    #[test]
    fn backtick_values_are_verbatim_and_may_span_lines() {
        let pairs = parse_env(concat!(
            "INNER=`{\"foo\": \"bar's\"}`\n",
            "NOEXPAND=`he$X llo`\n",
            "NOESCAPE=`a\\nb`\n",
            "COMMENT=`outside #hash` # work\n",
            "MULTI=`THIS\nIS\n\"MULTI'S\"\nSTRING`\n",
            "AFTER=plain\n",
        ));
        let get = |k: &str| pairs.iter().find(|(p, _)| p == k).map(|(_, v)| v.as_str());
        assert_eq!(get("INNER"), Some("{\"foo\": \"bar's\"}"));
        assert_eq!(
            get("NOEXPAND"),
            Some("he$X llo"),
            "no $-substitution in backticks"
        );
        assert_eq!(
            get("NOESCAPE"),
            Some("a\\nb"),
            "no escape processing in backticks"
        );
        assert_eq!(get("COMMENT"), Some("outside #hash"));
        assert_eq!(get("MULTI"), Some("THIS\nIS\n\"MULTI'S\"\nSTRING"));
        // A line following a multi-line backtick value must still parse.
        assert_eq!(
            get("AFTER"),
            Some("plain"),
            "parsing resumes after the closing backtick line"
        );
    }

    #[test]
    fn parse_comments_and_blanks() {
        let pairs = parse_env("# comment\n\nFOO=bar\n");
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn parse_export_prefix() {
        let pairs = parse_env("export FOO=bar\n");
        assert_eq!(pairs, vec![("FOO".to_string(), "bar".to_string())]);
    }

    #[test]
    fn read_env_file_reads_a_regular_file() {
        let p = std::env::temp_dir().join(format!("nub-a41-{}.env", std::process::id()));
        std::fs::write(&p, "FOO=bar\n").unwrap();
        assert_eq!(read_env_file(&p).as_deref(), Some("FOO=bar\n"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_env_file_rejects_unbounded_and_missing_sources() {
        // The guard refuses anything that isn't a regular file, so a hostile
        // --env-file can't stream forever or OOM (A41).
        assert_eq!(
            read_env_file(&std::env::temp_dir()),
            None,
            "directory rejected"
        );
        assert_eq!(
            read_env_file(Path::new("/nonexistent-nub-a41")),
            None,
            "missing rejected"
        );
        #[cfg(unix)]
        assert_eq!(
            read_env_file(Path::new("/dev/zero")),
            None,
            "char device rejected — would otherwise read forever"
        );
    }

    #[test]
    fn parse_multiline_double_quoted() {
        let pairs = parse_env("KEY=\"line1\nline2\"\n");
        assert_eq!(pairs[0].1, "line1\nline2");
    }

    #[test]
    fn parse_escape_sequences() {
        let pairs = parse_env("KEY=\"hello\\nworld\"\n");
        assert_eq!(pairs[0].1, "hello\nworld");
    }

    #[test]
    fn parse_inline_comments() {
        let pairs = parse_env("FOO=bar # this is a comment\n");
        assert_eq!(pairs[0].1, "bar");
    }

    #[test]
    fn expand_dollar_brace() {
        let mut env = HashMap::new();
        env.insert("HOST".to_string(), "localhost".to_string());
        assert_eq!(
            expand_vars("http://${HOST}:3000", &env),
            "http://localhost:3000"
        );
    }

    #[test]
    fn expand_dollar_bare() {
        let mut env = HashMap::new();
        env.insert("PORT".to_string(), "8080".to_string());
        assert_eq!(expand_vars("port=$PORT", &env), "port=8080");
    }

    #[test]
    fn expand_escaped_dollar() {
        let env = HashMap::new();
        assert_eq!(expand_vars("price=\\$5", &env), "price=$5");
    }

    // `discover_env_files` underpins `nub watch`'s `--env-file` precedence: it
    // must return only files that exist, highest-priority first, so the watch
    // path can reverse them into Node's last-writer-wins order. Locking the
    // ordering + existence-filtering here guards that translation. (The reload
    // behavior itself — Node re-reading `--env-file` on `--watch` restart — is
    // timing-dependent and verified ad hoc, not unit-tested; see `run_watch`.)
    #[test]
    fn discover_env_files_returns_existing_files_highest_priority_first() {
        let dir = std::env::temp_dir().join(format!("nub-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Create `.env` and `.env.local` but deliberately omit `.env.production`,
        // so the absent priority slot must be skipped.
        std::fs::write(dir.join(".env"), "X=1\n").unwrap();
        std::fs::write(dir.join(".env.local"), "X=2\n").unwrap();

        let found = discover_env_files(&dir);

        assert!(
            found.iter().all(|p| p.is_file()),
            "every returned path must exist (no `.env.production` slot for an absent file): {found:?}"
        );
        // `.env` is the lowest-priority slot, so it is always last when present.
        assert_eq!(
            found.last(),
            Some(&dir.join(".env")),
            "`.env` must sort last (lowest priority): {found:?}"
        );
        // `.env.local` outranks `.env` (except under NODE_ENV=test, which omits
        // it); when both are returned, `.env.local` must precede `.env`.
        if found.contains(&dir.join(".env.local")) {
            let local = found
                .iter()
                .position(|p| p == &dir.join(".env.local"))
                .unwrap();
            let base = found.iter().position(|p| p == &dir.join(".env")).unwrap();
            assert!(local < base, "`.env.local` must precede `.env`: {found:?}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `load_env_files` must expand `${VAR}` cross-references, matching the
    /// behavior the direct `nub <file>` path delivers. This is the regression
    /// guard for the bug where `nub watch` / `--env-file` left `${VAR}` literal.
    #[test]
    fn load_env_files_expands_var_references() {
        let dir = std::env::temp_dir().join(format!("nub-expand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".env"),
            "DB_HOST=localhost\nDATABASE_URL=postgres://${DB_HOST}:5432/db\n",
        )
        .unwrap();

        let vars = load_env_files(&dir);

        assert_eq!(
            vars.get("DATABASE_URL").map(String::as_str),
            Some("postgres://localhost:5432/db"),
            "`${{DB_HOST}}` must be expanded to its value; got {vars:?}"
        );
        assert_eq!(vars.get("DB_HOST").map(String::as_str), Some("localhost"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `expand_env_map` (used by the `--env-file` flag path) must apply the same
    /// multi-pass expansion as `load_env_files`.
    #[test]
    fn expand_env_map_expands_var_references() {
        let mut map = HashMap::new();
        map.insert("DB_HOST".to_string(), "localhost".to_string());
        map.insert(
            "DATABASE_URL".to_string(),
            "postgres://${DB_HOST}:5432/db".to_string(),
        );

        expand_env_map(&mut map);

        assert_eq!(
            map.get("DATABASE_URL").map(String::as_str),
            Some("postgres://localhost:5432/db"),
            "`${{DB_HOST}}` must be expanded; got {map:?}"
        );
    }
}
