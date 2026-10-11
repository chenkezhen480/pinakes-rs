//! Signing in to the console (design, "M10": signing in): the admin's user name and password
//! give a session token that is the admin and looks at a tenant as a user of it would; the
//! default password works from anywhere until it is changed on the console.

use crate::common::app::{Caller, TestApp};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};
use std::net::SocketAddr;

const HERE: &str = "127.0.0.1:50000";
const PERSONAL: &str = "[identity]\ndeployment = \"personal\"\n";
const ELSEWHERE: &str = "192.168.1.9:50000";

/// A request from `peer`, with `token` in the key header and the extra headers given.
async fn send(
    app: &TestApp,
    method: &str,
    uri: &str,
    peer: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:8080");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let peer: SocketAddr = peer.parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(peer));
    let (status, _, text) = app.send_raw(req).await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn sign_in(app: &TestApp, peer: &str, user: &str, password: &str) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        "/console/sign-in",
        peer,
        None,
        &[],
        Some(json!({ "user": user, "password": password })),
    )
    .await
}

fn seed(app: &TestApp, tenant: &str, title: &str) {
    let wiki = WikiFs::new(app.wiki_dir(tenant));
    let mut page = Page::new(PagePath::parse("expertise/returns").unwrap());
    page.front.title = Some(title.into());
    page.body = format!("# {title}\n\nSeven days.\n");
    wiki.write(&page).unwrap();
}

#[tokio::test]
async fn the_default_password_signs_in_the_first_time_from_anywhere() {
    // A multi-tenant deployment takes it too, and from another address (in Docker the
    // browser is never on the service's machine): the first sign-in is never blocked.
    let platform = TestApp::new();
    let (_, info) = send(
        &platform,
        "GET",
        "/console/session",
        ELSEWHERE,
        None,
        &[],
        None,
    )
    .await;
    assert_eq!(info["default_password"], json!(true), "{info}");
    let (status, body) = sign_in(&platform, ELSEWHERE, "root", "1234").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["default_password"], json!(true));

    let app = TestApp::with_config(PERSONAL);
    let (status, _) = sign_in(&app, HERE, "root", "wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = sign_in(&app, HERE, "root", "1234").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().unwrap().to_owned();
    assert_eq!(body["account"]["user"], json!("root"));

    // The session is the admin.
    let (status, _) = send(
        &app,
        "GET",
        "/admin/tenants",
        ELSEWHERE,
        Some(&token),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, audit) = send(&app, "GET", "/admin/audit", HERE, Some(&token), &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(audit.to_string().contains("console.sign_in"), "{audit}");

    // Signed out, it is nothing.
    let (status, _) = send(
        &app,
        "POST",
        "/console/sign-out",
        HERE,
        Some(&token),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, "GET", "/admin/tenants", HERE, Some(&token), &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_session_looks_at_the_tenant_it_names() {
    let app = TestApp::new();
    // Changed from the default (as PINAKES_ADMIN_PASSWORD would too).
    app.state.login.change("1234", "root", "s3cret").unwrap();
    let acme = app.create_tenant("acme").await;
    app.create_tenant("globex").await;
    seed(&app, "acme", "Acme returns");
    seed(&app, "globex", "Globex returns");
    let (_, body) = sign_in(&app, HERE, "root", "s3cret").await;
    let token = body["token"].as_str().unwrap().to_owned();

    // A multi-tenant deployment needs the tenant named; one that doesn't exist is not found.
    let (status, body) = send(&app, "GET", "/pages", HERE, Some(&token), &[], None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = send(
        &app,
        "GET",
        "/pages",
        HERE,
        Some(&token),
        &[("X-Pinakes-Tenant", "nobody")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, pages) = send(
        &app,
        "GET",
        "/pages/expertise/returns",
        HERE,
        Some(&token),
        &[("X-Pinakes-Tenant", "globex")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pages}");
    assert!(pages.to_string().contains("Globex returns"), "{pages}");

    // Any tenant id fits the header, percent-encoded.
    app.create_tenant("客户 A").await;
    seed(&app, "客户 A", "客户 A 的退货");
    let (status, page) = send(
        &app,
        "GET",
        "/pages/expertise/returns",
        HERE,
        Some(&token),
        &[("X-Pinakes-Tenant", "%E5%AE%A2%E6%88%B7%20A")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.to_string().contains("客户 A 的退货"), "{page}");

    // The admin's overview says how the service runs; the tenant's own key isn't told.
    let (_, s) = send(
        &app,
        "GET",
        "/status",
        HERE,
        Some(&token),
        &[("X-Pinakes-Tenant", "acme")],
        None,
    )
    .await;
    assert_eq!(s["local_only"], true, "{s}");
    let (_, s) = app.get(Caller::new(&acme), "/status").await;
    assert_eq!(s["local_only"], false, "{s}");

    // A tenant's API key names its own tenant; the header means nothing to it.
    let (_, own) = app
        .get(Caller::new(&acme), "/pages/expertise/returns")
        .await;
    assert!(own.to_string().contains("Acme returns"), "{own}");
    let (_, still) = send(
        &app,
        "GET",
        "/pages/expertise/returns",
        HERE,
        Some(&acme),
        &[("X-Pinakes-Tenant", "globex")],
        None,
    )
    .await;
    assert!(still.to_string().contains("Acme returns"), "{still}");

    // A forged session token is nothing.
    let (status, _) = send(
        &app,
        "GET",
        "/pages",
        HERE,
        Some("pnx_session_0123"),
        &[("X-Pinakes-Tenant", "acme")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_personal_session_looks_at_its_own_tenant() {
    let app = TestApp::with_config("[identity]\ndeployment = \"personal\"\n");
    app.state.prepare().await.unwrap();
    seed(&app, "000000", "My returns");
    let (_, body) = sign_in(&app, HERE, "root", "1234").await;
    let token = body["token"].as_str().unwrap().to_owned();
    let (status, page) = send(
        &app,
        "GET",
        "/pages/expertise/returns",
        HERE,
        Some(&token),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.to_string().contains("My returns"), "{page}");
}

#[tokio::test]
async fn the_account_is_changed_on_the_console() {
    let app = TestApp::with_config(PERSONAL);
    let (_, body) = sign_in(&app, HERE, "root", "1234").await;
    let token = body["token"].as_str().unwrap().to_owned();
    let (_, account) = send(
        &app,
        "GET",
        "/console/account",
        HERE,
        Some(&token),
        &[],
        None,
    )
    .await;
    assert_eq!(
        account,
        json!({ "user": "root", "default_password": true, "source": "default" })
    );

    let change = |current: &str| json!({ "current_password": current, "user": "ken", "password": "long enough" });
    let (status, _) = send(
        &app,
        "PUT",
        "/console/account",
        HERE,
        Some(&token),
        &[],
        Some(change("wrong")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send(
        &app,
        "PUT",
        "/console/account",
        HERE,
        Some(&token),
        &[],
        Some(change("1234")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let kept = body["token"].as_str().unwrap().to_owned();
    assert_eq!(body["account"]["default_password"], json!(false));

    // The old session ended; the new password works from anywhere, the old one nowhere.
    let (status, _) = send(&app, "GET", "/admin/tenants", HERE, Some(&token), &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&app, "GET", "/admin/tenants", HERE, Some(&kept), &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = sign_in(&app, ELSEWHERE, "ken", "long enough").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = sign_in(&app, HERE, "root", "1234").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, info) = send(&app, "GET", "/console/session", HERE, None, &[], None).await;
    assert_eq!(info["default_password"], json!(false));

    // Without a session, the account is not shown.
    let (status, _) = send(&app, "GET", "/console/account", HERE, None, &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_session_follows_every_job_of_the_tenant() {
    let app = TestApp::new();
    let acme = app.create_tenant("acme").await;
    let dir = pinakes::tenants::tenant_dir_name("acme");
    // An approval runs without a user; another user's job has its owner.
    let approval = app
        .state
        .jobs
        .run(
            &dir,
            None,
            "approve",
            Box::pin(async { Ok(Default::default()) }),
        )
        .await;
    let jins = app
        .state
        .jobs
        .run(
            &dir,
            Some(&pinakes::tenants::user_dir_name("jin")),
            "memory",
            Box::pin(async { Ok(Default::default()) }),
        )
        .await;
    let ids = |list: &Value| -> Vec<String> {
        list["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["job"].as_str().unwrap().to_owned())
            .collect()
    };

    // A user of the tenant sees neither in the list.
    let (status, list) = send(
        &app,
        "GET",
        "/jobs",
        HERE,
        Some(&acme),
        &[("X-User-Id", "ann")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(ids(&list).is_empty(), "{list}");
    let (status, _) = send(
        &app,
        "GET",
        &format!("/jobs/{}", jins.job),
        HERE,
        Some(&acme),
        &[("X-User-Id", "ann")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The console's admin sees both, and opens either.
    let (_, body) = sign_in(&app, HERE, "root", "1234").await;
    let token = body["token"].as_str().unwrap().to_owned();
    let tenant = [("X-Pinakes-Tenant", "acme")];
    let (status, list) = send(&app, "GET", "/jobs", HERE, Some(&token), &tenant, None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let seen = ids(&list);
    assert!(
        seen.contains(&approval.job) && seen.contains(&jins.job),
        "{list}"
    );
    let (status, job) = send(
        &app,
        "GET",
        &format!("/jobs/{}", jins.job),
        HERE,
        Some(&token),
        &tenant,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["status"], "done");
}
