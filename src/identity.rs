//! Who is calling: the `Scope` (tenant, optional user, groups) every read and write path takes.
//!
//! In API key mode the tenant comes from the key, and the user and groups from headers set by the
//! calling backend. A `Scope` can only be made here, from credentials, never from a request body.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::config::Deployment;
use crate::error::ApiError;
use crate::tenants::{Tenant, group_dir_name, user_dir_name, validate_id};
use crate::wiki::{Audience, PagePath};

/// More groups than this in one request is treated as a malformed header.
const MAX_GROUPS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: String,
    /// Folder name inside the tenant, from `user_dir_name`.
    pub dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    /// Folder name inside the tenant, from `group_dir_name`.
    pub dir: String,
}

#[derive(Debug, Clone)]
pub struct Scope {
    tenant: Arc<Tenant>,
    user: Option<User>,
    groups: Vec<Group>,
}

impl Scope {
    pub fn tenant(&self) -> &Arc<Tenant> {
        &self.tenant
    }

    pub fn user(&self) -> Option<&User> {
        self.user.as_ref()
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// A scope for offline tools that already hold the data folder, such as `kb-replay`,
    /// rebuilt from the folder names a trace recorded. Request handlers never use this: over
    /// HTTP a Scope only comes from credentials.
    pub(crate) fn offline(
        tenant: Arc<Tenant>,
        user_dir: Option<String>,
        group_dirs: Vec<String>,
    ) -> Self {
        Scope {
            tenant,
            user: user_dir.map(|dir| User {
                id: dir.clone(),
                dir,
            }),
            groups: group_dirs
                .into_iter()
                .map(|dir| Group {
                    id: dir.clone(),
                    dir,
                })
                .collect(),
        }
    }

    /// A trace record for this caller: tenant, user and group folders, never raw ids.
    pub fn trace_record(&self, endpoint: crate::trace::Endpoint) -> crate::trace::TraceRecord {
        let mut r = crate::trace::TraceRecord::new(endpoint, &self.tenant.dir_name);
        r.user = self.user.as_ref().map(|u| u.dir.clone());
        r.groups = self.groups.iter().map(|g| g.dir.clone()).collect();
        r
    }

    /// What this caller's agent session has read so far; nothing without a session.
    pub fn read_state(&self, session: Option<&str>) -> crate::search::tracker::ReadState {
        match session {
            Some(s) => self
                .tenant
                .reads
                .state(self.user.as_ref().map(|u| u.dir.as_str()), s),
            None => Default::default(),
        }
    }

    /// The page audiences this caller may see, as the search index names them: `tenant`, the
    /// caller's group folders and the caller's user folder. Agrees with [`Scope::can_see`].
    pub fn audience_keys(&self) -> Vec<String> {
        let mut out = vec!["tenant".to_owned()];
        out.extend(self.groups.iter().map(|g| g.dir.clone()));
        out.extend(self.user.iter().map(|u| u.dir.clone()));
        out
    }

    /// Whether this caller may see a page: tenant-level pages always, group pages when the caller
    /// is in the group, user pages only when they belong to the caller's own user.
    pub fn can_see(&self, path: &PagePath) -> bool {
        match path.audience() {
            Audience::Tenant => true,
            Audience::Group(dir) => self.groups.iter().any(|g| g.dir == dir),
            Audience::User(dir) => self.user.as_ref().is_some_and(|u| u.dir == dir),
        }
    }
}

/// Group ids from a comma-separated header. Ids are opaque, so they can't contain commas.
fn parse_groups(value: Option<&HeaderValue>) -> Result<Vec<Group>, ApiError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let text = value
        .to_str()
        .map_err(|_| ApiError::BadRequest("group header is not valid text".into()))?;
    let mut groups: Vec<Group> = Vec::new();
    for id in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        validate_id("group", id).map_err(ApiError::BadRequest)?;
        if !groups.iter().any(|g| g.id == id) {
            groups.push(Group {
                id: id.to_owned(),
                dir: group_dir_name(id),
            });
        }
    }
    if groups.len() > MAX_GROUPS {
        return Err(ApiError::BadRequest(format!(
            "more than {MAX_GROUPS} groups in one request"
        )));
    }
    Ok(groups)
}

/// The API key from the configured header. For `Authorization`, the `Bearer` scheme is required
/// (case-insensitive, as RFC 7235 says) and removed.
fn api_key<'a>(headers: &'a HeaderMap, header: &str) -> Option<&'a str> {
    let value = headers.get(header)?.to_str().ok()?.trim();
    let key = if header.eq_ignore_ascii_case("authorization") {
        let (scheme, rest) = value.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return None;
        }
        rest.trim()
    } else {
        value
    };
    (!key.is_empty()).then_some(key)
}

impl FromRequestParts<AppState> for Scope {
    type Rejection = ApiError;

    #[tracing::instrument(name = "resolve_identity", skip_all)]
    async fn from_request_parts(parts: &mut Parts, app: &AppState) -> Result<Self, ApiError> {
        let cfg = &app.config.identity;
        let key = api_key(&parts.headers, &cfg.key_header).ok_or(ApiError::Unauthorized)?;
        let tenant = app
            .tenants
            .authenticate(key)
            .await?
            .ok_or(ApiError::Unauthorized)?;

        let user = match parts.headers.get(cfg.user_header.as_str()) {
            // A personal deployment has one person, so no header means that person.
            None if cfg.deployment == Deployment::Personal => Some(User {
                id: cfg.default_user.clone(),
                dir: user_dir_name(&cfg.default_user),
            }),
            None => None,
            Some(v) => {
                let id = v
                    .to_str()
                    .map_err(|_| ApiError::BadRequest("user id header is not valid text".into()))?
                    .trim();
                validate_id("user", id).map_err(ApiError::BadRequest)?;
                Some(User {
                    id: id.to_owned(),
                    dir: user_dir_name(id),
                })
            }
        };
        let groups = parse_groups(parts.headers.get(cfg.group_header.as_str()))?;
        tracing::debug!(
            tenant = %tenant.dir_name,
            user = user.as_ref().map(|u| u.dir.as_str()),
            groups = groups.len(),
            "identity resolved"
        );
        Ok(Scope {
            tenant,
            user,
            groups,
        })
    }
}

/// Proof that the caller holds the service admin key. Admin routes take this instead of a Scope.
#[derive(Debug, Clone, Copy)]
pub struct Admin;

/// The admin key, kept only as a hash so comparisons are fixed-length.
#[derive(Clone)]
pub struct AdminKey([u8; 32]);

impl AdminKey {
    /// `None` for an empty key: admin routes stay disabled.
    pub fn new(key: &str) -> Option<Self> {
        let key = key.trim();
        (!key.is_empty()).then(|| Self(Sha256::digest(key.as_bytes()).into()))
    }

    fn matches(&self, candidate: &str) -> bool {
        let other: [u8; 32] = Sha256::digest(candidate.as_bytes()).into();
        // Constant-time comparison.
        self.0
            .iter()
            .zip(other)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &AppState) -> Result<Self, ApiError> {
        let admin = app.admin_key.as_ref().ok_or(ApiError::Unauthorized)?;
        let key = api_key(&parts.headers, &app.config.identity.key_header)
            .ok_or(ApiError::Unauthorized)?;
        if admin.matches(key) {
            Ok(Admin)
        } else {
            Err(ApiError::Unauthorized)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn bearer_prefix_is_required_for_authorization() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer abc"));
        assert_eq!(api_key(&h, "Authorization"), Some("abc"));
        h.insert("authorization", HeaderValue::from_static("bearer  abc"));
        assert_eq!(api_key(&h, "Authorization"), Some("abc"));
        h.insert("authorization", HeaderValue::from_static("Basic abc"));
        assert_eq!(api_key(&h, "Authorization"), None);
        h.insert("authorization", HeaderValue::from_static("abc"));
        assert_eq!(api_key(&h, "Authorization"), None);
        h.insert("x-api-key", HeaderValue::from_static(" abc "));
        assert_eq!(api_key(&h, "X-Api-Key"), Some("abc"));
    }

    #[test]
    fn groups_header_parsing() {
        assert!(parse_groups(None).unwrap().is_empty());
        let v = HeaderValue::from_static(" risk-team, ops ,,risk-team ");
        let ids: Vec<_> = parse_groups(Some(&v))
            .unwrap()
            .into_iter()
            .map(|g| g.id)
            .collect();
        assert_eq!(ids, vec!["risk-team", "ops"]);
        let many = HeaderValue::from_str(
            &(0..65)
                .map(|i| format!("g{i}"))
                .collect::<Vec<_>>()
                .join(","),
        )
        .unwrap();
        assert!(parse_groups(Some(&many)).is_err());
    }

    #[test]
    fn admin_key_matching() {
        assert!(AdminKey::new("  ").is_none());
        let k = AdminKey::new("secret").unwrap();
        assert!(k.matches("secret"));
        assert!(!k.matches("secret2"));
    }
}
