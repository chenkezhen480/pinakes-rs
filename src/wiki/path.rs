//! Page paths such as `expertise/refund-regulations` or `users/u_7b21e0d4a1b2c3d4/preferences`.
//!
//! A path is validated once, when parsed, against the fixed wiki layout. Anything else — `..`,
//! absolute paths, backslashes, unknown folders — is rejected, so a `PagePath` can always be
//! joined onto a tenant's wiki folder safely.

use std::fmt;

use serde::{Serialize, Serializer};

use super::WikiError;
use crate::kinds::Kind;

const MAX_SEGMENT_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PagePath {
    /// Normalized form without the `.md` extension, segments joined by `/`.
    raw: String,
}

impl PagePath {
    /// Parses a page path. A trailing `.md` is accepted and dropped.
    pub fn parse(input: &str) -> Result<Self, WikiError> {
        let s = input.strip_suffix(".md").unwrap_or(input);
        let segs: Vec<&str> = s.split('/').collect();
        for seg in &segs {
            check_segment(seg)?;
        }
        let ok = match segs.as_slice() {
            ["index"] | ["log"] => true,
            ["experience" | "expertise" | "graph", _name] => true,
            ["users", user, "preferences"] => is_user_dir(user),
            ["users", user, "decisions", _topic] => is_user_dir(user),
            _ => false,
        };
        if !ok {
            return Err(invalid(format!("{input:?} is not a known wiki location")));
        }
        Ok(Self { raw: s.to_owned() })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.raw.split('/')
    }

    /// The user folder that owns this page (`u_…`), or `None` for tenant-level pages.
    pub fn owner_user_dir(&self) -> Option<&str> {
        let mut segs = self.segments();
        match (segs.next(), segs.next()) {
            (Some("users"), Some(user)) => Some(user),
            _ => None,
        }
    }

    /// The knowledge kind implied by where the page lives. `index` and `log` have none.
    pub fn kind(&self) -> Option<Kind> {
        let segs: Vec<&str> = self.segments().collect();
        match segs.as_slice() {
            ["experience", _] => Some(Kind::Experience),
            ["expertise", _] => Some(Kind::Expertise),
            ["graph", _] => Some(Kind::Graph),
            ["users", _, "preferences"] => Some(Kind::Preference),
            ["users", _, "decisions", _] => Some(Kind::Decision),
            _ => None,
        }
    }
}

impl fmt::Display for PagePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Serialize for PagePath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.raw)
    }
}

fn invalid(msg: String) -> WikiError {
    WikiError::InvalidPath(msg)
}

/// A segment is a page or folder name: letters (any script), digits, `-` and `_`. No dots, so
/// `.`/`..` and hidden files are impossible; no separators or drive letters.
fn check_segment(seg: &str) -> Result<(), WikiError> {
    if seg.is_empty() {
        return Err(invalid("empty path segment".into()));
    }
    if seg.len() > MAX_SEGMENT_LEN {
        return Err(invalid("path segment too long".into()));
    }
    if !seg
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(invalid(format!(
            "path segment {seg:?} has disallowed characters"
        )));
    }
    if is_windows_reserved(seg) {
        return Err(invalid(format!("path segment {seg:?} is a reserved name")));
    }
    Ok(())
}

/// Folder names produced by `tenants::user_dir_name`: `u_` plus 16 lowercase hex digits.
fn is_user_dir(seg: &str) -> bool {
    seg.strip_prefix("u_").is_some_and(|h| {
        h.len() == 16
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn is_windows_reserved(seg: &str) -> bool {
    let upper = seg.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: &str = "u_0123456789abcdef";

    #[test]
    fn accepts_the_wiki_layout() {
        for p in [
            "index".to_string(),
            "log.md".to_string(),
            "experience/refund-handling".to_string(),
            "expertise/refund_regulations.md".to_string(),
            "graph/commerce".to_string(),
            "expertise/退款规则".to_string(),
            format!("users/{U}/preferences"),
            format!("users/{U}/decisions/refund-policy"),
        ] {
            assert!(PagePath::parse(&p).is_ok(), "{p} should parse");
        }
    }

    #[test]
    fn rejects_escapes_and_unknown_locations() {
        for p in [
            "",
            "/etc/passwd",
            "../x",
            "expertise/../../x",
            "expertise/..",
            "expertise/a/b",
            "expertise\\..\\x",
            "expertise/.hidden",
            "expertise/a.b",
            "C:/x",
            "sources/raw",
            "users/alice/preferences",
            "users/u_0123/preferences",
            "users/u_0123456789ABCDEF/preferences",
            "expertise/CON",
            "expertise/lpt1",
            "expertise/",
            "expertise//x",
        ] {
            assert!(PagePath::parse(p).is_err(), "{p:?} should be rejected");
        }
    }

    #[test]
    fn owner_and_kind() {
        let p = PagePath::parse(&format!("users/{U}/decisions/x")).unwrap();
        assert_eq!(p.owner_user_dir(), Some(U));
        assert_eq!(p.kind(), Some(Kind::Decision));
        let t = PagePath::parse("expertise/x").unwrap();
        assert_eq!(t.owner_user_dir(), None);
        assert_eq!(t.kind(), Some(Kind::Expertise));
        assert_eq!(PagePath::parse("index").unwrap().kind(), None);
    }
}
