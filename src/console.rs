//! The web console (design, "M10": a web console for everything): its built files (`ui/dist/`,
//! embedded by `build.rs`) served at `/ui`, `/` sending there, under a strict
//! Content-Security-Policy; `GET /console/session`, which tells the sign-in page about the
//! service; and signing in (`login`): the admin's user name and password give a session token,
//! the console's one credential for the API, so it can never do what the API wouldn't let the
//! admin do.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::config::{Deployment, IdentityMode};
use crate::error::ApiError;
use crate::identity::Admin;
use crate::login::{Account, Caller, SignInError};

mod files {
    include!(concat!(env!("OUT_DIR"), "/console.rs"));
}

/// Scripts, styles, images and calls only from the service itself; nothing inline, no eval,
/// no frames.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn file(path: &str) -> Option<&'static [u8]> {
    files::FILES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, bytes)| *bytes)
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, e)| e) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json" | "webmanifest") => "application/json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn serve(path: &str) -> Response {
    let Some(bytes) = file(path) else {
        if files::FILES.is_empty() {
            return (
                StatusCode::NOT_FOUND,
                "The console isn't built into this binary: run `npm ci && npm run build` in ui/ and build the service again.",
            )
                .into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // Built assets carry a hash in their names; the page itself is always asked for again.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    resp
}

pub async fn root() -> Redirect {
    Redirect::to("/ui/")
}

pub async fn index() -> Response {
    serve("index.html")
}

pub async fn asset(Path(path): Path<String>) -> Response {
    if path.is_empty() {
        return serve("index.html");
    }
    serve(&path)
}

/// What the sign-in page and the console need to know about the service.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ConsoleSession {
    pub deployment: Deployment,
    pub identity: IdentityMode,
    /// The headers carrying the credential, the user, the groups, an insert's audience and
    /// the tenant looked at.
    pub key_header: String,
    pub user_header: String,
    pub group_header: String,
    pub audience_header: String,
    pub tenant_header: String,
    /// The password is still the default one: the sign-in page says what it is.
    pub default_password: bool,
}

/// Who a sign-in comes from, for counting its failures: the connection's address, IPv6 by its
/// /64 (one host commonly holds a whole /64, and could otherwise take a fresh address for every
/// guess).
fn caller_address(peer: Option<SocketAddr>) -> String {
    match peer.map(|p| p.ip().to_canonical()) {
        None => "unknown".to_owned(),
        Some(std::net::IpAddr::V6(v6)) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        Some(v4) => v4.to_string(),
    }
}

pub async fn session(State(app): State<AppState>) -> Json<ConsoleSession> {
    let id = &app.config.identity;
    Json(ConsoleSession {
        deployment: id.deployment,
        identity: id.mode,
        key_header: id.key_header.clone(),
        user_header: id.user_header.clone(),
        group_header: id.group_header.clone(),
        audience_header: id.audience_header.clone(),
        tenant_header: crate::identity::TENANT_HEADER.into(),
        default_password: app.login.default_password(),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SignIn {
    pub user: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SignedIn {
    /// The session token: the console's credential, sent in the key header.
    pub token: String,
    pub account: Account,
}

fn sign_in_error(e: SignInError) -> ApiError {
    match e {
        SignInError::Wrong => ApiError::Unauthorized,
        SignInError::TooMany => ApiError::TooManyRequests(e.to_string()),
        SignInError::Invalid(m) => ApiError::BadRequest(m),
        SignInError::Internal(e) => ApiError::Internal(e),
    }
}

/// Enough passwords are being checked already: try again (nothing queues).
fn busy() -> ApiError {
    ApiError::TooManyRequests("other sign-ins are being checked; try again in a moment".into())
}

/// `POST /console/sign-in`: the admin's user name and password for a session token.
pub async fn sign_in(
    State(app): State<AppState>,
    peer: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    Json(req): Json<SignIn>,
) -> Result<Json<SignedIn>, ApiError> {
    let caller = Caller {
        peer: caller_address(peer.map(|c| c.0.0)),
    };
    let caller_peer = caller.peer.clone();
    let turn = app.login.turn().ok_or_else(busy)?;
    let state = app.clone();
    let token = tokio::task::spawn_blocking(move || {
        let _turn = turn;
        state.login.sign_in(req.user.trim(), &req.password, &caller)
    })
    .await
    .map_err(|e| anyhow::anyhow!("signing in: {e}"))?
    .map_err(|e| {
        if matches!(e, SignInError::Wrong | SignInError::TooMany) {
            tracing::warn!(caller = %caller_peer, "a console sign-in failed");
        }
        sign_in_error(e)
    })?;
    app.audit.record("console", "console.sign_in", json!({}));
    Ok(Json(SignedIn {
        token,
        account: app.login.account(),
    }))
}

/// `POST /console/sign-out`: ends the session whose token is sent.
pub async fn sign_out(
    State(app): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    if let Some(token) = crate::identity::api_key(&headers, &app.config.identity.key_header)
        && token.starts_with(crate::login::TOKEN_PREFIX)
    {
        let token = token.to_owned();
        let state = app.clone();
        tokio::task::spawn_blocking(move || state.login.sign_out(&token))
            .await
            .map_err(|e| anyhow::anyhow!("signing out: {e}"))??;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /console/account`: the admin's user name and whether the password is the default.
pub async fn account(_: Admin, State(app): State<AppState>) -> Json<Account> {
    Json(app.login.account())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct AccountChange {
    pub current_password: String,
    pub user: String,
    pub password: String,
}

/// `PUT /console/account`: a new user name and password, given the current password. Every
/// session ends; the answer carries a new one for the caller.
pub async fn change_account(
    _: Admin,
    State(app): State<AppState>,
    Json(req): Json<AccountChange>,
) -> Result<Json<SignedIn>, ApiError> {
    let turn = app.login.turn().ok_or_else(busy)?;
    let state = app.clone();
    let user = req.user.trim().to_owned();
    let token = tokio::task::spawn_blocking(move || {
        let _turn = turn;
        state
            .login
            .change(&req.current_password, &req.user, &req.password)
    })
    .await
    .map_err(|e| anyhow::anyhow!("changing the account: {e}"))?
    .map_err(|e| match e {
        // Not a 401: the session itself is fine.
        SignInError::Wrong => ApiError::Forbidden("the current password is wrong".into()),
        e => sign_in_error(e),
    })?;
    app.audit
        .record("console", "console.account", json!({ "user": user }));
    Ok(Json(SignedIn {
        token,
        account: app.login.account(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callers_are_counted_by_address() {
        let local: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        assert_eq!(caller_address(Some(local)), "127.0.0.1");
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:50000".parse().unwrap();
        assert_eq!(caller_address(Some(mapped)), "127.0.0.1");
        // IPv6 callers are counted by their /64.
        let a: SocketAddr = "[2001:db8:1:2:aaaa::1]:443".parse().unwrap();
        let b: SocketAddr = "[2001:db8:1:2:bbbb::9]:443".parse().unwrap();
        assert_eq!(caller_address(Some(a)), "2001:db8:1:2::/64");
        assert_eq!(caller_address(Some(a)), caller_address(Some(b)));
    }
}
