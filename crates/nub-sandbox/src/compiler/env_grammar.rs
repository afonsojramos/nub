//! The closed env value-type grammar (Rust-parsed, NOT ArkType). Per
//! .fray/sandbox-config-spec.md:
//!
//!   value  := "string" | FORMAT | /regex/ | 'a' | 'b' | …   (literal union)
//!   FORMAT := integer | number | port                        (trimmed 2026-07-08)
//!
//! No comparison/intersection operators. String formats (email/url/…) deliberately
//! do NOT ship — `/regex/` covers them. Unrecognized → hard error naming the set
//! (closed vocab, so re-adding later is non-breaking).

use crate::policy::EnvFormat;

/// A parsed env value type from the string grammar.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvType {
    /// The `"string"` catch-all — any value validates.
    AnyString,
    /// One of the closed FORMAT keywords.
    Format(EnvFormat),
    /// A `/regex/` pattern (compiled at validate time).
    Regex(String),
    /// A literal union — the value must be one of these exact strings.
    Union(Vec<String>),
}

/// Parse a type string from the env grammar. Errors name the supported set.
pub fn parse_env_type(spec: &str) -> Result<EnvType, String> {
    let s = spec.trim();
    // `/regex/` — a leading and trailing slash.
    if let Some(inner) = s.strip_prefix('/').and_then(|r| r.strip_suffix('/')) {
        if inner.is_empty() {
            return Err("empty /regex/ in env type".to_string());
        }
        return Ok(EnvType::Regex(inner.to_string()));
    }
    // Literal union: `'a' | 'b' | …` (single-quoted members joined by `|`).
    if s.contains('\'') {
        let members = parse_literal_union(s)?;
        return Ok(EnvType::Union(members));
    }
    match s {
        "string" => Ok(EnvType::AnyString),
        "integer" => Ok(EnvType::Format(EnvFormat::Integer)),
        "number" => Ok(EnvType::Format(EnvFormat::Number)),
        "port" => Ok(EnvType::Format(EnvFormat::Port)),
        other => Err(format!(
            "unknown env type `{other}` — supported: string, integer, number, port, /regex/, or a 'a'|'b' literal union"
        )),
    }
}

/// Parse a `'a' | 'b' | 'c'` literal union into its member strings.
fn parse_literal_union(s: &str) -> Result<Vec<String>, String> {
    let mut members = Vec::new();
    for part in s.split('|') {
        let p = part.trim();
        let inner = p
            .strip_prefix('\'')
            .and_then(|r| r.strip_suffix('\''))
            .ok_or_else(|| format!("malformed literal-union member `{p}` (expected 'quoted')"))?;
        members.push(inner.to_string());
    }
    if members.is_empty() {
        return Err("empty literal union in env type".to_string());
    }
    Ok(members)
}

impl EnvType {
    /// Return the [`EnvFormat`] this type carries, for the IR's `schema`. Regex /
    /// union / any-string have no closed format.
    pub fn format(&self) -> Option<EnvFormat> {
        match self {
            EnvType::Format(f) => Some(*f),
            _ => None,
        }
    }

    /// Validate a concrete value against this type. Errs with a human-readable
    /// message on mismatch.
    pub fn validate(&self, value: &str) -> Result<(), String> {
        match self {
            EnvType::AnyString => Ok(()),
            EnvType::Format(EnvFormat::Integer) => value
                .parse::<i64>()
                .map(|_| ())
                .map_err(|_| format!("`{value}` is not an integer")),
            EnvType::Format(EnvFormat::Number) => value
                .parse::<f64>()
                .map(|_| ())
                .map_err(|_| format!("`{value}` is not a number")),
            EnvType::Format(EnvFormat::Port) => match value.parse::<u32>() {
                Ok(n) if (1..=65535).contains(&n) => Ok(()),
                _ => Err(format!("`{value}` is not a valid port (1–65535)")),
            },
            EnvType::Regex(pat) => {
                let re =
                    regex::Regex::new(pat).map_err(|e| format!("invalid regex `{pat}`: {e}"))?;
                if re.is_match(value) {
                    Ok(())
                } else {
                    Err(format!("`{value}` does not match /{pat}/"))
                }
            }
            EnvType::Union(members) => {
                if members.iter().any(|m| m == value) {
                    Ok(())
                } else {
                    Err(format!("`{value}` is not one of {members:?}"))
                }
            }
        }
    }
}
