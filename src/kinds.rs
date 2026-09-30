//! The five knowledge kinds. Per-kind rules arrive with the librarian in M4.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Experience,
    Decision,
    Expertise,
    Graph,
    Preference,
}

/// Who a page can be for: the whole tenant, one group, or one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudienceLevel {
    Tenant,
    Group,
    User,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Experience,
        Kind::Decision,
        Kind::Expertise,
        Kind::Graph,
        Kind::Preference,
    ];

    /// The audiences a kind's pages may have. The wiki path layout enforces the same rule.
    pub fn audiences(self) -> &'static [AudienceLevel] {
        use AudienceLevel::*;
        match self {
            Kind::Experience | Kind::Expertise => &[Tenant, Group],
            Kind::Decision => &[Group, User],
            Kind::Preference => &[User],
            Kind::Graph => &[Tenant],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Experience => "experience",
            Kind::Decision => "decision",
            Kind::Expertise => "expertise",
            Kind::Graph => "graph",
            Kind::Preference => "preference",
        }
    }
}
