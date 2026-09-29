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

/// Whether a kind's pages belong to the whole tenant or to one user inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeLevel {
    Tenant,
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

    pub fn scope_level(self) -> ScopeLevel {
        match self {
            Kind::Experience | Kind::Expertise | Kind::Graph => ScopeLevel::Tenant,
            Kind::Decision | Kind::Preference => ScopeLevel::User,
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
