//! Page paths such as `expertise/refund-regulations`, `groups/g_…/decisions/bin-blocks` or
//! `users/u_7b21e0d4a1b2c3d4/preferences`.
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

/// Who a page is for: the whole tenant, one group, or one user. Groups and users are named by
/// their folder (`g_…`, `u_…`), never by raw id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience<'a> {
    Tenant,
    Group(&'a str),
    User(&'a str),
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
            [
                "groups",
                group,
                "experience" | "expertise" | "decisions",
                _name,
            ] => is_hashed_dir(group, "g_"),
            ["users", user, "preferences"] => is_hashed_dir(user, "u_"),
            ["users", user, "decisions", _topic] => is_hashed_dir(user, "u_"),
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

    pub fn audience(&self) -> Audience<'_> {
        let mut segs = self.segments();
        match (segs.next(), segs.next()) {
            (Some("groups"), Some(group)) => Audience::Group(group),
            (Some("users"), Some(user)) => Audience::User(user),
            _ => Audience::Tenant,
        }
    }

    /// The knowledge kind implied by where the page lives. `index` and `log` have none.
    pub fn kind(&self) -> Option<Kind> {
        let segs: Vec<&str> = self.segments().collect();
        match segs.as_slice() {
            ["experience", _] | ["groups", _, "experience", _] => Some(Kind::Experience),
            ["expertise", _] | ["groups", _, "expertise", _] => Some(Kind::Expertise),
            ["graph", _] => Some(Kind::Graph),
            ["users", _, "preferences"] => Some(Kind::Preference),
            ["users", _, "decisions", _] | ["groups", _, "decisions", _] => Some(Kind::Decision),
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

/// Folder names produced by `tenants::user_dir_name` and `tenants::group_dir_name`: a prefix
/// plus 16 lowercase hex digits.
fn is_hashed_dir(seg: &str, prefix: &str) -> bool {
    seg.strip_prefix(prefix).is_some_and(|h| {
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
    const G: &str = "g_fedcba9876543210";

    #[test]
    fn accepts_the_wiki_layout() {
        for p in [
            "index".to_string(),
            "log.md".to_string(),
            "experience/refund-handling".to_string(),
            "expertise/refund_regulations.md".to_string(),
            "graph/commerce".to_string(),
            "expertise/退款规则".to_string(),
            format!("groups/{G}/experience/card-testing"),
            format!("groups/{G}/expertise/internal-notes"),
            format!("groups/{G}/decisions/bin-blocks"),
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
            "groups/risk-team/decisions/x",
            "groups/g_fedcba9876543210/preferences",
            "groups/g_fedcba9876543210/graph/x",
            "groups/u_0123456789abcdef/decisions/x",
            "users/g_fedcba9876543210/decisions/x",
            "expertise/CON",
            "expertise/lpt1",
            "expertise/",
            "expertise//x",
        ] {
            assert!(PagePath::parse(p).is_err(), "{p:?} should be rejected");
        }
    }

    #[test]
    fn audience_and_kind() {
        let p = PagePath::parse(&format!("users/{U}/decisions/x")).unwrap();
        assert_eq!(p.audience(), Audience::User(U));
        assert_eq!(p.kind(), Some(Kind::Decision));
        let g = PagePath::parse(&format!("groups/{G}/decisions/x")).unwrap();
        assert_eq!(g.audience(), Audience::Group(G));
        assert_eq!(g.kind(), Some(Kind::Decision));
        let ge = PagePath::parse(&format!("groups/{G}/experience/x")).unwrap();
        assert_eq!(ge.kind(), Some(Kind::Experience));
        let t = PagePath::parse("expertise/x").unwrap();
        assert_eq!(t.audience(), Audience::Tenant);
        assert_eq!(t.kind(), Some(Kind::Expertise));
        assert_eq!(PagePath::parse("index").unwrap().kind(), None);
    }

    /// The path layout and `Kind::audiences` must agree: every page a path can name has an
    /// audience its kind allows, and every allowed pair has a path.
    #[test]
    fn layout_matches_kind_audiences() {
        use crate::kinds::AudienceLevel;
        let level = |a: Audience| match a {
            Audience::Tenant => AudienceLevel::Tenant,
            Audience::Group(_) => AudienceLevel::Group,
            Audience::User(_) => AudienceLevel::User,
        };
        let mut seen = std::collections::HashSet::new();
        for p in [
            "experience/x".to_string(),
            "expertise/x".to_string(),
            "graph/x".to_string(),
            format!("groups/{G}/experience/x"),
            format!("groups/{G}/expertise/x"),
            format!("groups/{G}/decisions/x"),
            format!("users/{U}/preferences"),
            format!("users/{U}/decisions/x"),
        ] {
            let path = PagePath::parse(&p).unwrap();
            let kind = path.kind().unwrap();
            let lvl = level(path.audience());
            assert!(
                kind.audiences().contains(&lvl),
                "{p}: {kind:?} can't be {lvl:?}"
            );
            seen.insert((kind, lvl as u8));
        }
        for kind in Kind::ALL {
            for lvl in kind.audiences() {
                assert!(
                    seen.contains(&(kind, *lvl as u8)),
                    "no path for {kind:?} {lvl:?}"
                );
            }
        }
    }
}
