//! Who is calling: the `Scope` (tenant + optional user) every read and write path takes.
//!
//! In API key mode the tenant comes from the key and the user from a header set by the calling
//! backend. A `Scope` can only be made here, from credentials, never from a request body.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::ApiError;
use crate::tenants::{Tenant, user_dir_name, validate_id};
use crate::wiki::PagePath;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: String,
    /// Folder name inside the tenant, from `user_dir_name`.
    pub dir: String,
}

#[derive(Debug, Clone)]
pub struct Scope {
    tenant: Arc<Tenant>,
    user: Option<User>,
}

impl Scope {
    pub fn tenant(&self) -> &Arc<Tenant> {
        &self.tenant
    }

    pub fn user(&self) -> Option<&User> {
        self.user.as_ref()
    }

    /// Whether this caller may see a page: tenant-level pages always, user pages only when they
    /// belong to the caller's own user.
    pub fn can_see(&self, path: &PagePath) -> bool {
        match path.owner_user_dir() {
            None => true,
            Some(owner) => self.user.as_ref().is_some_and(|u| u.dir == owner),
        }
    }
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
        tracing::debug!(tenant = %tenant.dir_name, user = user.as_ref().map(|u| u.dir.as_str()), "identity resolved");
        Ok(Scope { tenant, user })
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
    fn admin_key_matching() {
        assert!(AdminKey::new("  ").is_none());
        let k = AdminKey::new("secret").unwrap();
        assert!(k.matches("secret"));
        assert!(!k.matches("secret2"));
    }
}
