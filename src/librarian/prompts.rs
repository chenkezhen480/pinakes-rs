//! The texts the librarian works from: the default rules per kind (`rules/`) and its prompts
//! (`prompts/`), built into the binary so every deployment has them. A tenant can override a
//! kind's rules with `<tenant>/rules/<kind>.md`.

use std::path::Path;

use crate::kinds::Kind;

pub const INGEST: &str = include_str!("../../prompts/ingest.md");
pub const VERIFY: &str = include_str!("../../prompts/verify.md");
pub const DESCRIBE: &str = include_str!("../../prompts/describe.md");
pub const VERIFY_DESCRIBE: &str = include_str!("../../prompts/verify-describe.md");
pub const MATCH: &str = include_str!("../../prompts/match.md");
pub const READ: &str = include_str!("../../prompts/read.md");
pub const LINT: &str = include_str!("../../prompts/lint.md");
pub const ACQUIRE_RANK: &str = include_str!("../../prompts/acquire-rank.md");
pub const ACQUIRE_SCREEN: &str = include_str!("../../prompts/acquire-screen.md");

/// The rules shipped with the service for a kind.
pub fn default_rules(kind: Kind) -> &'static str {
    match kind {
        Kind::Experience => include_str!("../../rules/experience.md"),
        Kind::Decision => include_str!("../../rules/decision.md"),
        Kind::Expertise => include_str!("../../rules/expertise.md"),
        Kind::Graph => include_str!("../../rules/graph.md"),
        Kind::Preference => include_str!("../../rules/preference.md"),
    }
}

/// A tenant's rules for a kind: its own override when it has one, else the default.
pub fn rules(tenant_dir: &Path, kind: Kind) -> String {
    let file = tenant_dir
        .join("rules")
        .join(format!("{}.md", kind.as_str()));
    match std::fs::read_to_string(&file) {
        Ok(text) if !text.trim().is_empty() => text,
        _ => default_rules(kind).to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_rules_override_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert!(rules(dir.path(), Kind::Experience).contains("One playbook per task type"));
        std::fs::create_dir_all(dir.path().join("rules")).unwrap();
        std::fs::write(dir.path().join("rules/experience.md"), "# Ours\n").unwrap();
        assert_eq!(rules(dir.path(), Kind::Experience), "# Ours\n");
        for kind in Kind::ALL {
            assert!(default_rules(kind).starts_with("# Rules"), "{kind:?}");
        }
    }
}
