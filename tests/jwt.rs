//! JWT mode: tenant, user and groups come from the claims of a token the host signed with its
//! private key; the service checks it with the public key in its config. Tokens that are
//! expired, signed by another key, for another audience or issuer, or that name a tenant the
//! service doesn't have are all a plain 401.

mod common;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::testset;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use pinakes::tenants::user_dir_name;
use serde_json::{Value, json};

const RSA_PRIVATE: &[u8] = include_bytes!("fixtures/jwt/rsa-private.pem");
const ED_PRIVATE: &[u8] = include_bytes!("fixtures/jwt/ed25519-private.pem");

fn config(algorithm: &str, public: &str, extra: &str) -> String {
    format!(
        "[identity]\nmode = \"jwt\"\nalgorithm = \"{algorithm}\"\n\
         public_key_file = \"tests/fixtures/jwt/{public}\"\n{extra}"
    )
}

fn in_an_hour() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

fn sign(claims: Value) -> String {
    jsonwebtoken::encode(
        &Header::new(Algorithm::RS256),
        &claims,
        &EncodingKey::from_rsa_pem(RSA_PRIVATE).unwrap(),
    )
    .unwrap()
}

fn token(tenant: &str, user: Option<&str>, groups: &[&str]) -> String {
    let mut claims = json!({ "tenant_id": tenant, "groups": groups, "exp": in_an_hour() });
    if let Some(u) = user {
        claims["sub"] = json!(u);
    }
    sign(claims)
}

async fn shopline(extra: &str) -> (TestApp, String) {
    let mut app = TestApp::with_config(&config("RS256", "rsa-public.pem", extra));
    let key = app.create_tenant("shopline").await;
    testset::install_wiki("shopline", &app.wiki_dir("shopline"));
    app.restart();
    (app, key)
}

async fn read(app: &TestApp, token: &str, page: &str) -> (StatusCode, Value) {
    app.post(Caller::new(token), "/read", json!({ "ids": [page] }))
        .await
}

#[tokio::test]
async fn the_token_names_tenant_user_and_groups() {
    let (app, _) = shopline("").await;
    let wang = format!("users/{}/preferences", user_dir_name("wang"));

    let (status, body) = read(&app, &token("shopline", Some("wang"), &[]), &wang).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["pages"][0]["text"].is_string(), "{body}");

    // Another user's page is not found; so is a user page without a user.
    let (_, body) = read(&app, &token("shopline", Some("lily"), &[]), &wang).await;
    assert_eq!(body["pages"][0]["error"], "not_found", "{body}");
    let (status, body) = read(&app, &token("shopline", None, &[]), &wang).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pages"][0]["error"], "not_found", "{body}");

    // Search runs as the token's caller.
    let (status, body) = app
        .post(
            Caller::new(&token("shopline", Some("wang"), &["ops"])),
            "/search",
            json!({ "query": "七天无理由退货", "keywords": ["七天无理由"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["knowledge"]["hits"][0]["page"],
        "expertise/refund-regulations"
    );
}

#[tokio::test]
async fn bad_tokens_are_refused_alike() {
    let (app, api_key) = shopline("").await;
    let page = "expertise/refund-regulations";
    let expired = sign(json!({ "tenant_id": "shopline", "exp": 1_000_000_000 }));
    let no_exp = sign(json!({ "tenant_id": "shopline" }));
    let other_key = jsonwebtoken::encode(
        &Header::new(Algorithm::EdDSA),
        &json!({ "tenant_id": "shopline", "exp": in_an_hour() }),
        &EncodingKey::from_ed_pem(ED_PRIVATE).unwrap(),
    )
    .unwrap();
    let with_aud = sign(json!({ "tenant_id": "shopline", "aud": "billing", "exp": in_an_hour() }));
    let not_yet =
        sign(json!({ "tenant_id": "shopline", "nbf": in_an_hour(), "exp": in_an_hour() + 60 }));
    for (what, credential) in [
        ("expired", expired),
        ("without exp", no_exp),
        ("another key", other_key),
        ("an unknown tenant", token("nobody", Some("wang"), &[])),
        ("an audience while none is configured", with_aud),
        ("not valid yet", not_yet),
        ("the API key", api_key),
        ("garbage", "not.a.token".to_owned()),
    ] {
        let (status, body) = read(&app, &credential, page).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{what}: {body}");
    }
    // A malformed claim is the caller's mistake.
    let bad_groups = sign(json!({ "tenant_id": "shopline", "groups": "ops", "exp": in_an_hour() }));
    assert_eq!(
        read(&app, &bad_groups, page).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn issuer_and_audience_are_checked_when_configured() {
    let (app, _) = shopline("issuer = \"https://auth.example\"\naudience = \"pinakes\"\n").await;
    let page = "expertise/refund-regulations";
    let claims = |iss: &str, aud: &str| {
        sign(json!({ "tenant_id": "shopline", "iss": iss, "aud": aud, "exp": in_an_hour() }))
    };
    assert_eq!(
        read(&app, &claims("https://auth.example", "pinakes"), page)
            .await
            .0,
        StatusCode::OK
    );
    for t in [
        claims("https://evil.example", "pinakes"),
        claims("https://auth.example", "billing"),
        token("shopline", None, &[]),
    ] {
        assert_eq!(read(&app, &t, page).await.0, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn ed25519_keys_work_and_wrong_keys_are_config_errors() {
    let mut app = TestApp::with_config(&config("EdDSA", "ed25519-public.pem", ""));
    app.create_tenant("shopline").await;
    testset::install_wiki("shopline", &app.wiki_dir("shopline"));
    app.restart();
    let t = jsonwebtoken::encode(
        &Header::new(Algorithm::EdDSA),
        &json!({ "tenant_id": "shopline", "exp": in_an_hour() }),
        &EncodingKey::from_ed_pem(ED_PRIVATE).unwrap(),
    )
    .unwrap();
    assert_eq!(
        read(&app, &t, "expertise/refund-regulations").await.0,
        StatusCode::OK
    );

    for (algorithm, file) in [
        ("RS256", "ed25519-public.pem"),
        ("HS256", "rsa-public.pem"),
        ("RS256", "missing.pem"),
    ] {
        let mut cfg = pinakes::config::Config::from_toml_str(&config(algorithm, file, "")).unwrap();
        assert!(
            cfg.load_jwt_key(std::path::Path::new(".")).is_err(),
            "{algorithm} with {file}"
        );
    }
    assert!(pinakes::config::Config::from_toml_str("[identity]\nmode = \"jwt\"\n").is_err());
}

/// A token deletes its own user's data only; another user's is not found, and stays.
#[tokio::test]
async fn a_token_deletes_only_its_own_user() {
    let (app, _) = shopline("").await;
    let wang = format!("users/{}/preferences", user_dir_name("wang"));
    let as_wang = token("shopline", Some("wang"), &[]);

    let lily = token("shopline", Some("lily"), &[]);
    let (status, _) = app.delete(Caller::new(&lily), "/users/wang").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, body) = read(&app, &as_wang, &wang).await;
    assert!(body["pages"][0]["text"].is_string(), "{body}");

    let (status, body) = app.delete(Caller::new(&as_wang), "/users/wang").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["pages_deleted"].as_u64().unwrap() >= 1, "{body}");
    let (_, body) = read(&app, &as_wang, &wang).await;
    assert_eq!(body["pages"][0]["error"], "not_found", "{body}");
}
