use std::sync::{Arc, Mutex};

use super::common::*;
use crate::config::{Limits, RealtimeConfig};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, header},
    routing::post,
};
use serde_json::{Value, json};
use sqlx::PgPool;

// ---------- login page, CSRF, parameter validation ----------

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn login_page_has_security_headers_and_csrf_cookie(pool: PgPool) {
    let app = App::new(pool);
    let r = app.get(&App::authorize_uri(STATE, &App::challenge())).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.header("content-security-policy"),
        "default-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'"
    );
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert_eq!(r.header("referrer-policy"), "no-referrer");
    assert_eq!(r.header("cache-control"), "no-store");
    let cookie = r.header("set-cookie");
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"),
        "{cookie}"
    );
    assert!(r.body.contains("name=\"csrf\""));
    assert!(!r.body.contains("<script"));

    let css = app.get("/static/auth.css").await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(css.header("content-type").starts_with("text/css"));
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn no_cors_headers_even_for_cross_origin_requests(pool: PgPool) {
    let app = App::new(pool);
    let req = Request::get("/healthz")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .unwrap();
    let r = app.send(req).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers
            .keys()
            .all(|k| !k.as_str().starts_with("access-control-"))
    );
    let preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/me")
        .header(header::ORIGIN, "https://evil.example")
        .header("access-control-request-method", "GET")
        .body(Body::empty())
        .unwrap();
    let r = app.send(preflight).await;
    assert!(
        r.headers
            .keys()
            .all(|k| !k.as_str().starts_with("access-control-"))
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn authorize_rejects_bad_parameters_without_redirecting(pool: PgPool) {
    let app = App::new(pool);
    let ch = App::challenge();
    let good = App::authorize_uri(STATE, &ch);
    let cases = [
        good.replace(
            "redirect_uri=sotto%3A%2F%2Fauth%2Fcallback",
            "redirect_uri=sotto%3A%2F%2Fauth%2Fcallback%2Fx",
        ),
        good.replace(
            "redirect_uri=sotto%3A%2F%2Fauth%2Fcallback",
            "redirect_uri=https%3A%2F%2Fevil.example",
        ),
        good.replace("code_challenge_method=S256", "code_challenge_method=plain"),
        good.replace("response_type=code", "response_type=token"),
        good.replace("client_id=sotto-desktop", "client_id=other"),
        App::authorize_uri("", &ch),
        App::authorize_uri(&"a".repeat(129), &ch),
        App::authorize_uri("bad%20state", &ch),
        App::authorize_uri(STATE, "short"),
        App::authorize_uri(STATE, &"!".repeat(43)),
        "/authorize".to_owned(),
    ];
    for uri in cases {
        let r = app.get(&uri).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(r.headers.get(header::LOCATION).is_none(), "{uri}");
        assert!(r.headers.get(header::SET_COOKIE).is_none(), "{uri}");
        assert!(!r.body.contains("sotto://"), "{uri}");
    }
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn form_posts_without_a_matching_csrf_token_are_rejected(pool: PgPool) {
    let app = App::new(pool.clone());
    app.sign_up("ada@example.com", PASSWORD).await;
    let csrf = app.open_login().await;
    let owned = App::fields(
        &csrf,
        &[("email", "ada@example.com"), ("password", PASSWORD)],
    );
    let fields: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();

    for path in ["/authorize/signin", "/authorize/signup", "/authorize/magic"] {
        let no_cookie = app.post_form(path, &fields, None).await;
        assert_eq!(no_cookie.status, StatusCode::FORBIDDEN, "{path}");
        let other = "x".repeat(43);
        let wrong_cookie = app.post_form(path, &fields, Some(&other)).await;
        assert_eq!(wrong_cookie.status, StatusCode::FORBIDDEN, "{path}");
        let mut empty = fields.clone();
        empty.retain(|(k, _)| *k != "csrf");
        empty.push(("csrf", ""));
        let empty_both = app.post_form(path, &empty, Some("")).await;
        assert_eq!(empty_both.status, StatusCode::FORBIDDEN, "{path}");
    }
    let (codes,): (i64,) = sqlx::query_as("SELECT count(*) FROM auth_codes WHERE used_at IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(codes, 1, "only the sign-up code from setup (unused) exists");
    assert!(app.take_mail().is_empty());
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn form_posts_revalidate_hidden_parameters(pool: PgPool) {
    let app = App::new(pool);
    let csrf = app.open_login().await;
    let owned = App::fields(&csrf, &[("email", "a@example.com"), ("password", PASSWORD)]);
    let tampered: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| {
            if *k == "redirect_uri" {
                (*k, "https://evil.example/cb")
            } else {
                (*k, v.as_str())
            }
        })
        .collect();
    let r = app
        .post_form("/authorize/signin", &tampered, Some(&csrf))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.callback_url().is_none());
}

// ---------- sign-up, sign-in, magic link ----------

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn sign_up_by_mailed_link_then_password_sign_in(pool: PgPool) {
    let app = App::new(pool.clone());
    let code = app.sign_up("  Ada@Example.COM ", PASSWORD).await;
    assert_eq!(code.len(), 43);

    let (email, hash): (String, Option<String>) =
        sqlx::query_as("SELECT email, password_hash FROM accounts")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(email, "ada@example.com");
    assert!(hash.unwrap().starts_with("$argon2id$"));

    let r = app.password_login_code("ADA@example.com", PASSWORD).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let url = r.callback_url().expect("handoff link");
    assert!(url.starts_with("sotto://auth/callback?code="));
    assert_eq!(query_param(&url, "state").as_deref(), Some(STATE));
    assert!(r.body.contains("Return to Sotto"));
    assert!(r.body.contains("http-equiv=\"refresh\""));
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn wrong_password_and_unknown_email_look_the_same(pool: PgPool) {
    let app = App::new(pool);
    app.sign_up("ada@example.com", PASSWORD).await;
    let wrong = app
        .password_login_code("ada@example.com", "not the password")
        .await;
    let unknown = app
        .password_login_code("nobody@example.com", "not the password")
        .await;
    let malformed = app
        .password_login_code("not an email", "not the password")
        .await;
    for r in [&wrong, &unknown, &malformed] {
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert!(r.callback_url().is_none());
        assert!(r.body.contains("The email or password is wrong."));
    }
    assert_eq!(
        wrong.body.replace(&csrf_of(&wrong), ""),
        unknown.body.replace(&csrf_of(&unknown), "")
    );
}

fn csrf_of(r: &Resp) -> String {
    let start = r.body.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    r.body[start..start + 43].to_owned()
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn sign_up_response_does_not_reveal_existing_accounts(pool: PgPool) {
    let app = App::new(pool.clone());
    app.sign_up("ada@example.com", PASSWORD).await;

    let csrf = app.open_login().await;
    let new = app
        .login_post(
            "/authorize/signup",
            &csrf,
            &[
                ("email", "new@example.com"),
                ("password", "another password 1"),
            ],
        )
        .await;
    let existing = app
        .login_post(
            "/authorize/signup",
            &csrf,
            &[
                ("email", "ada@example.com"),
                ("password", "another password 2"),
            ],
        )
        .await;
    assert_eq!(new.status, existing.status);
    assert_eq!(new.body, existing.body);
    let mails = app.take_mail();
    assert_eq!(mails.len(), 2, "both addresses get a mail");
    assert_eq!(mails[0].subject, mails[1].subject);

    // The link for the existing account must not replace its password.
    app.follow_magic(&mails[1]).await;
    assert_eq!(
        app.password_login_code("ada@example.com", PASSWORD)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        app.password_login_code("ada@example.com", "another password 2")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        n, 1,
        "the new address has no account until its link is opened"
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn sign_up_validates_email_and_password(pool: PgPool) {
    let app = App::new(pool);
    let csrf = app.open_login().await;
    let short = app
        .login_post(
            "/authorize/signup",
            &csrf,
            &[("email", "a@example.com"), ("password", "short")],
        )
        .await;
    assert_eq!(short.status, StatusCode::BAD_REQUEST);
    let bad = app
        .login_post(
            "/authorize/signup",
            &csrf,
            &[("email", "nope"), ("password", PASSWORD)],
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert!(app.take_mail().is_empty());
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn magic_link_creates_account_for_unknown_email_and_is_single_use(pool: PgPool) {
    let app = App::new(pool.clone());
    let csrf = app.open_login().await;
    let first = app
        .login_post("/authorize/magic", &csrf, &[("email", "New@Example.com")])
        .await;
    assert_eq!(first.status, StatusCode::OK);
    let mail = app.take_mail().pop().unwrap();
    assert_eq!(mail.to, "new@example.com");
    assert!(mail.body.contains("http://sotto.test/magic/"));

    let path = App::magic_path(&mail);
    let ok = app.get(&path).await;
    assert_eq!(ok.status, StatusCode::OK);
    let url = ok.callback_url().unwrap();
    assert_eq!(
        query_param(&url, "state").as_deref(),
        Some(STATE),
        "finishes the pending request"
    );
    let tokens = app.exchange(&code_from(&url)).await;
    assert_eq!(tokens.status, StatusCode::OK);

    let again = app.get(&path).await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert!(again.callback_url().is_none());

    // Same page for a known address.
    let second = app
        .login_post("/authorize/magic", &csrf, &[("email", "new@example.com")])
        .await;
    assert_eq!(first.body, second.body);
    let (hash,): (Option<String>,) = sqlx::query_as("SELECT password_hash FROM accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(hash.is_none());
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn magic_link_expires_after_15_minutes_and_unknown_tokens_fail(pool: PgPool) {
    let app = App::new(pool.clone());
    let csrf = app.open_login().await;
    app.login_post("/authorize/magic", &csrf, &[("email", "a@example.com")])
        .await;
    let mail = app.take_mail().pop().unwrap();
    let (ttl_min,): (f64,) = sqlx::query_as(
        "SELECT (extract(epoch FROM expires_at - created_at) / 60)::float8 FROM magic_links",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((14.9..=15.1).contains(&ttl_min), "{ttl_min}");

    sqlx::query("UPDATE magic_links SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        app.get(&App::magic_path(&mail)).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.get(&format!("/magic/{}", "A".repeat(43))).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.get("/magic/short").await.status,
        StatusCode::BAD_REQUEST
    );
    let (accounts,): (i64,) = sqlx::query_as("SELECT count(*) FROM accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(accounts, 0);
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn tokens_and_codes_are_stored_hashed(pool: PgPool) {
    let app = App::new(pool.clone());
    let (access, refresh) = app.tokens_for("ada@example.com").await;
    for (table, column, secret) in [
        ("access_tokens", "token_hash", &access),
        ("refresh_tokens", "token_hash", &refresh),
    ] {
        let (n,): (i64,) = sqlx::query_as(&format!(
            "SELECT count(*) FROM {table} WHERE encode({column}, 'escape') = $1"
        ))
        .bind(secret)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 0);
    }
    let (len,): (i32,) = sqlx::query_as("SELECT length(token_hash) FROM access_tokens")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(len, 32);
}

// ---------- authorization code + PKCE ----------

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn pkce_success_issues_tokens_and_me_works(pool: PgPool) {
    let app = App::new(pool);
    let code = app.sign_up("ada@example.com", PASSWORD).await;
    let r = app.exchange(&code).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.header("cache-control"), "no-store");
    let v: Value = r.json();
    assert_eq!(v["token_type"], "Bearer");
    assert_eq!(v["expires_in"], 900);
    let me = app
        .get_auth("/v1/me", v["access_token"].as_str().unwrap())
        .await;
    assert_eq!(me.status, StatusCode::OK);
    let me: Value = me.json();
    assert_eq!(me["email"], "ada@example.com");
    assert!(
        me["id"].as_str().unwrap().len() == 36 && me["created_at"].as_str().unwrap().ends_with('Z')
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn pkce_verifier_mismatch_is_rejected_and_burns_the_code(pool: PgPool) {
    let app = App::new(pool);
    let code = app.sign_up("ada@example.com", PASSWORD).await;
    let wrong = "A".repeat(43);
    let r = app
        .json(Method::POST, "/v1/oauth/token", None, &json!({"grant_type":"authorization_code","code":code,"code_verifier":wrong,"redirect_uri":REDIRECT,"client_id":"sotto-desktop"}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json::<Value>()["error"], "invalid_grant");
    assert_eq!(
        app.exchange(&code).await.status,
        StatusCode::BAD_REQUEST,
        "the right verifier no longer helps"
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn code_reuse_fails_and_revokes_what_the_first_exchange_issued(pool: PgPool) {
    let app = App::new(pool);
    let code = app.sign_up("ada@example.com", PASSWORD).await;
    let first: Value = app.exchange(&code).await.json();
    let access = first["access_token"].as_str().unwrap();
    assert_eq!(app.get_auth("/v1/me", access).await.status, StatusCode::OK);

    assert_eq!(app.exchange(&code).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        app.get_auth("/v1/me", access).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.refresh(first["refresh_token"].as_str().unwrap())
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn expired_code_is_rejected(pool: PgPool) {
    let app = App::new(pool.clone());
    let code = app.sign_up("ada@example.com", PASSWORD).await;
    let (ttl,): (f64,) = sqlx::query_as(
        "SELECT extract(epoch FROM expires_at - created_at)::float8 FROM auth_codes",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((59.0..=61.0).contains(&ttl), "{ttl}");
    sqlx::query("UPDATE auth_codes SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(app.exchange(&code).await.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn token_endpoint_checks_redirect_uri_client_and_shapes(pool: PgPool) {
    let app = App::new(pool);
    let code = app.sign_up("ada@example.com", PASSWORD).await;
    let base = |patch: Value| {
        let mut b = json!({"grant_type":"authorization_code","code":code,"code_verifier":VERIFIER,"redirect_uri":REDIRECT,"client_id":"sotto-desktop"});
        for (k, v) in patch.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    for patch in [
        json!({"redirect_uri":"sotto://auth/other"}),
        json!({"client_id":"other"}),
        json!({"code_verifier":"short"}),
        json!({"code":"nope"}),
    ] {
        let r = app
            .json(Method::POST, "/v1/oauth/token", None, &base(patch))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
    // None of those attempts spent the code.
    assert_eq!(app.exchange(&code).await.status, StatusCode::OK);

    let extra = app
        .json(
            Method::POST,
            "/v1/oauth/token",
            None,
            &json!({"grant_type":"password","username":"a","password":"b"}),
        )
        .await;
    assert_eq!(extra.status, StatusCode::BAD_REQUEST);
    assert_eq!(extra.json::<Value>()["error"], "invalid_request");
    assert!(
        !extra.body.contains("password"),
        "serde errors must not be echoed"
    );

    let not_json = app
        .send(
            Request::post("/v1/oauth/token")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from("x"))
                .unwrap(),
        )
        .await;
    assert_eq!(not_json.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn code_is_bound_to_the_challenge_of_its_authorize_request(pool: PgPool) {
    let app = App::new(pool);
    // A challenge for a different verifier.
    let other_verifier = "B".repeat(43);
    let challenge = crate::crypto::pkce_s256(&other_verifier);
    let r = app.get(&App::authorize_uri(STATE, &challenge)).await;
    let csrf = r
        .header("set-cookie")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("sotto_csrf=")
        .unwrap()
        .to_owned();
    let fields = [
        ("csrf", csrf.as_str()),
        ("response_type", "code"),
        ("client_id", "sotto-desktop"),
        ("redirect_uri", REDIRECT),
        ("state", STATE),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("email", "ada@example.com"),
        ("password", PASSWORD),
    ];
    app.post_form("/authorize/signup", &fields, Some(&csrf))
        .await;
    let mail = app.take_mail().pop().unwrap();
    let code = app.follow_magic(&mail).await;
    assert_eq!(
        app.exchange(&code).await.status,
        StatusCode::BAD_REQUEST,
        "verifier of another request"
    );
}

// ---------- refresh rotation, logout ----------

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn refresh_rotates_and_reuse_revokes_the_family(pool: PgPool) {
    let app = App::new(pool);
    let (access0, refresh0) = app.tokens_for("ada@example.com").await;

    let r1 = app.refresh(&refresh0).await;
    assert_eq!(r1.status, StatusCode::OK, "{}", r1.body);
    let v1: Value = r1.json();
    let (access1, refresh1) = (
        v1["access_token"].as_str().unwrap(),
        v1["refresh_token"].as_str().unwrap(),
    );
    assert_ne!(refresh1, refresh0);
    assert_eq!(app.get_auth("/v1/me", access1).await.status, StatusCode::OK);

    let r2: Value = app.refresh(refresh1).await.json();
    let (access2, refresh2) = (
        r2["access_token"].as_str().unwrap(),
        r2["refresh_token"].as_str().unwrap(),
    );

    // Replaying the first, already rotated token is theft: the whole family dies.
    assert_eq!(app.refresh(&refresh0).await.status, StatusCode::BAD_REQUEST);
    for token in [access0.as_str(), access1, access2] {
        assert_eq!(
            app.get_auth("/v1/me", token).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(app.refresh(refresh2).await.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn refresh_rejects_unknown_expired_and_wrong_client(pool: PgPool) {
    let app = App::new(pool.clone());
    let (_, refresh) = app.tokens_for("ada@example.com").await;
    assert_eq!(
        app.refresh(&"z".repeat(43)).await.status,
        StatusCode::BAD_REQUEST
    );
    let wrong_client = app
        .json(
            Method::POST,
            "/v1/oauth/token",
            None,
            &json!({"grant_type":"refresh_token","refresh_token":refresh,"client_id":"other"}),
        )
        .await;
    assert_eq!(wrong_client.status, StatusCode::BAD_REQUEST);
    sqlx::query("UPDATE refresh_tokens SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(app.refresh(&refresh).await.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn refresh_token_lasts_30_days_and_access_token_15_minutes(pool: PgPool) {
    let app = App::new(pool.clone());
    app.tokens_for("ada@example.com").await;
    let (a,): (f64,) =
        sqlx::query_as("SELECT extract(epoch FROM expires_at - now())::float8 FROM access_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (r,): (f64,) =
        sqlx::query_as("SELECT extract(epoch FROM expires_at - now())::float8 FROM refresh_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!((890.0..=900.0).contains(&a), "{a}");
    assert!(
        ((30.0 * 86400.0 - 10.0)..=30.0 * 86400.0).contains(&r),
        "{r}"
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn logout_revokes_the_current_family_only(pool: PgPool) {
    let app = App::new(pool);
    let (access_a, refresh_a) = app.tokens_for("ada@example.com").await;
    // A second device for the same account.
    let code = app
        .password_login_code("ada@example.com", PASSWORD)
        .await
        .callback_url()
        .map(|u| code_from(&u))
        .unwrap();
    let b: Value = app.exchange(&code).await.json();

    let r = app
        .json(Method::POST, "/v1/logout", Some(&access_a), &json!({}))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get_auth("/v1/me", &access_a).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.refresh(&refresh_a).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.get_auth("/v1/me", b["access_token"].as_str().unwrap())
            .await
            .status,
        StatusCode::OK
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn protected_endpoints_need_a_valid_unexpired_bearer(pool: PgPool) {
    let app = App::new(pool.clone());
    let (access, _) = app.tokens_for("ada@example.com").await;
    for (method, uri) in [
        (Method::GET, "/v1/me"),
        (Method::POST, "/v1/logout"),
        (Method::GET, "/v1/preferences"),
        (Method::POST, "/v1/realtime-tokens"),
    ] {
        let r = app
            .send(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(r.header("www-authenticate"), "Bearer");
    }
    assert_eq!(
        app.get_auth("/v1/me", "garbage").await.status,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE access_tokens SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        app.get_auth("/v1/me", &access).await.status,
        StatusCode::UNAUTHORIZED
    );
}

// ---------- preferences ----------

fn prefs_body(base: u64, locale: &str) -> Value {
    json!({
        "base_version": base,
        "preferences": {
            "settings": {
                "auto_answer_model": "claude-haiku-5-5", "manual_model": "claude-sonnet-5-5",
                "spend_cap_micro_usd": 500000, "locale": locale, "auto_answer": true,
                "invisible": false, "engine_preference": "auto"
            },
            "shortcuts": {"toggle_overlay": "Cmd+Shift+Space"},
            "modes": [{"id": "m1", "name": "Interview", "system_prompt": "Be brief.", "updated_at": "2026-11-06T10:00:00Z"}]
        }
    })
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn preferences_round_trip_with_versions_and_conflict(pool: PgPool) {
    let app = App::new(pool);
    let (token, _) = app.tokens_for("ada@example.com").await;

    let empty: Value = app.get_auth("/v1/preferences", &token).await.json();
    assert_eq!(empty["version"], 0);
    assert!(empty["preferences"].is_null());

    let put1 = app
        .json(
            Method::PUT,
            "/v1/preferences",
            Some(&token),
            &prefs_body(0, "pt-BR"),
        )
        .await;
    assert_eq!(put1.status, StatusCode::OK, "{}", put1.body);
    assert_eq!(put1.json::<Value>()["version"], 1);

    let put2 = app
        .json(
            Method::PUT,
            "/v1/preferences",
            Some(&token),
            &prefs_body(1, "en-US"),
        )
        .await;
    assert_eq!(put2.json::<Value>()["version"], 2);

    let stale = app
        .json(
            Method::PUT,
            "/v1/preferences",
            Some(&token),
            &prefs_body(1, "fr-FR"),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    let body: Value = stale.json();
    assert_eq!(body["error"], "conflict");
    assert_eq!(body["current"]["version"], 2);
    assert_eq!(
        body["current"]["preferences"]["settings"]["locale"],
        "en-US"
    );

    let stale_zero = app
        .json(
            Method::PUT,
            "/v1/preferences",
            Some(&token),
            &prefs_body(0, "fr-FR"),
        )
        .await;
    assert_eq!(stale_zero.status, StatusCode::CONFLICT);

    let got: Value = app.get_auth("/v1/preferences", &token).await.json();
    assert_eq!(got["version"], 2);
    assert_eq!(got["preferences"]["modes"][0]["name"], "Interview");
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn preferences_are_per_account(pool: PgPool) {
    let app = App::new(pool);
    let (a, _) = app.tokens_for("a@example.com").await;
    let (b, _) = app.tokens_for("b@example.com").await;
    app.json(
        Method::PUT,
        "/v1/preferences",
        Some(&a),
        &prefs_body(0, "pt-BR"),
    )
    .await;
    let theirs: Value = app.get_auth("/v1/preferences", &b).await.json();
    assert_eq!(theirs["version"], 0);
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn a_deleted_mode_syncs_as_a_tombstone_without_its_prompt(pool: PgPool) {
    let app = App::new(pool);
    let (token, _) = app.tokens_for("ada@example.com").await;

    let mut kept = prefs_body(0, "en-US");
    kept["preferences"]["modes"][0]["deleted"] = json!(true);
    let r = app
        .json(Method::PUT, "/v1/preferences", Some(&token), &kept)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "a tombstone with a prompt");

    let mut gone = prefs_body(0, "en-US");
    gone["preferences"]["modes"][0]["deleted"] = json!(true);
    gone["preferences"]["modes"][0]["system_prompt"] = json!("");
    let r = app
        .json(Method::PUT, "/v1/preferences", Some(&token), &gone)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let got: Value = app.get_auth("/v1/preferences", &token).await.json();
    assert_eq!(got["preferences"]["modes"][0]["deleted"], true);
    assert_eq!(got["preferences"]["modes"][0]["system_prompt"], "");
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn preferences_reject_unknown_fields_bad_values_and_big_bodies(pool: PgPool) {
    let app = App::new(pool);
    let (token, _) = app.tokens_for("ada@example.com").await;
    let put = |body: Value| {
        let app = &app;
        let token = token.clone();
        async move {
            app.json(Method::PUT, "/v1/preferences", Some(&token), &body)
                .await
        }
    };

    let mut extra = prefs_body(0, "en-US");
    extra["preferences"]["modes"][0]["files"] = json!(["a.pdf"]);
    assert_eq!(put(extra).await.status, StatusCode::BAD_REQUEST);

    let mut arbitrary = prefs_body(0, "en-US");
    arbitrary["preferences"]["transcript"] = json!("hello");
    assert_eq!(put(arbitrary).await.status, StatusCode::BAD_REQUEST);

    let mut engine = prefs_body(0, "en-US");
    engine["preferences"]["settings"]["engine_preference"] = json!("telepathy");
    assert_eq!(put(engine).await.status, StatusCode::BAD_REQUEST);

    let mut long_prompt = prefs_body(0, "en-US");
    long_prompt["preferences"]["modes"][0]["system_prompt"] = json!("x".repeat(8001));
    assert_eq!(put(long_prompt).await.status, StatusCode::BAD_REQUEST);

    let mut dup = prefs_body(0, "en-US");
    let m = dup["preferences"]["modes"][0].clone();
    dup["preferences"]["modes"].as_array_mut().unwrap().push(m);
    assert_eq!(put(dup).await.status, StatusCode::BAD_REQUEST);

    let mut many = prefs_body(0, "en-US");
    let m = many["preferences"]["modes"][0].clone();
    many["preferences"]["modes"] = json!(
        (0..101)
            .map(|i| {
                let mut m = m.clone();
                m["id"] = json!(format!("m{i}"));
                m
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(put(many).await.status, StatusCode::BAD_REQUEST);

    let mut huge = prefs_body(0, "en-US");
    huge["preferences"]["shortcuts"] = json!({"k": "v".repeat(600 * 1024)});
    let r = put(huge).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(r.json::<Value>()["error"], "payload_too_large");

    // Just under the limit still gets as far as validation.
    let mut near = prefs_body(0, "en-US");
    near["preferences"]["shortcuts"] = json!({"k": "v".repeat(60 * 1024)});
    assert_eq!(put(near).await.status, StatusCode::BAD_REQUEST);

    let got: Value = app.get_auth("/v1/preferences", &token).await.json();
    assert_eq!(got["version"], 0, "nothing was stored");
}

// ---------- realtime tokens ----------

fn realtime_body() -> Value {
    json!({"provider": "openai", "locale": "pt-BR"})
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn fake_provider_mints_a_token_and_audits_without_it(pool: PgPool) {
    let app = App::new(pool.clone());
    let (access, _) = app.tokens_for("ada@example.com").await;
    let r = app
        .json(
            Method::POST,
            "/v1/realtime-tokens",
            Some(&access),
            &realtime_body(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let v: Value = r.json();
    let token = v["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("fake_") && v["fake"] == true && v["provider"] == "openai");
    assert!(v["expires_at"].as_str().unwrap().ends_with('Z'));

    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT action, provider, account_id::text FROM realtime_token_audit")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1.as_str()),
        ("mint_realtime_token", "openai")
    );
    // Nothing in the audit row (or anywhere in the database) holds the token or the access token.
    let dump: Vec<(String,)> =
        sqlx::query_as("SELECT row_to_json(t)::text FROM realtime_token_audit t")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        dump.iter()
            .all(|(j,)| !j.contains(&token) && !j.contains(&access))
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn realtime_tokens_are_limited_per_account_per_hour(pool: PgPool) {
    let limits = Limits {
        realtime_per_account: 3,
        ..high_limits()
    };
    let app = App::with(pool.clone(), RealtimeConfig::Fake, limits);
    let (a, _) = app.tokens_for("a@example.com").await;
    let (b, _) = app.tokens_for("b@example.com").await;
    for _ in 0..3 {
        assert_eq!(
            app.json(
                Method::POST,
                "/v1/realtime-tokens",
                Some(&a),
                &realtime_body()
            )
            .await
            .status,
            StatusCode::OK
        );
    }
    let blocked = app
        .json(
            Method::POST,
            "/v1/realtime-tokens",
            Some(&a),
            &realtime_body(),
        )
        .await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = blocked.header("retry-after").parse().unwrap();
    assert!((3590..=3600).contains(&retry), "{retry}");
    assert_eq!(blocked.json::<Value>()["error"], "rate_limited");
    assert_eq!(
        app.json(
            Method::POST,
            "/v1/realtime-tokens",
            Some(&b),
            &realtime_body()
        )
        .await
        .status,
        StatusCode::OK
    );

    // Old audit rows leave the window.
    sqlx::query("UPDATE realtime_token_audit SET created_at = now() - interval '61 minutes'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        app.json(
            Method::POST,
            "/v1/realtime-tokens",
            Some(&a),
            &realtime_body()
        )
        .await
        .status,
        StatusCode::OK
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn realtime_request_validation(pool: PgPool) {
    let app = App::new(pool);
    let (a, _) = app.tokens_for("a@example.com").await;
    for body in [
        json!({"provider":"deepgram"}),
        json!({"provider":"openai","extra":1}),
        json!({"provider":"openai","locale":"a b"}),
        json!({}),
    ] {
        let r = app
            .json(Method::POST, "/v1/realtime-tokens", Some(&a), &body)
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}");
    }
}

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

async fn fake_openai(status: StatusCode) -> (String, Seen) {
    let seen: Seen = Arc::default();
    async fn handler(
        State((seen, status)): State<(Seen, StatusCode)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        seen.lock().unwrap().push((auth, body));
        if status.is_success() {
            let exp = chrono_now() + 600;
            (
                status,
                Json(
                    json!({"value":"ek_upstream_secret","expires_at":exp,"session":{"id":"sess_1"}}),
                ),
            )
        } else {
            (status, Json(json!({"error":{"message":"nope"}})))
        }
    }
    let app = Router::new()
        .route("/v1/realtime/client_secrets", post(handler))
        .with_state((seen.clone(), status));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

fn chrono_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

fn openai(base_url: String) -> RealtimeConfig {
    RealtimeConfig::Openai {
        api_key: "sk-server-key".into(),
        base_url,
        transcribe_model: "gpt-live-transcribe".into(),
    }
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn openai_provider_calls_the_client_secrets_endpoint(pool: PgPool) {
    let (url, seen) = fake_openai(StatusCode::OK).await;
    let app = App::with(pool.clone(), openai(format!("{url}/")), high_limits());
    let (a, _) = app.tokens_for("a@example.com").await;
    let r = app
        .json(
            Method::POST,
            "/v1/realtime-tokens",
            Some(&a),
            &realtime_body(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let v: Value = r.json();
    assert_eq!(v["token"], "ek_upstream_secret");
    assert_eq!(v["fake"], false);

    let calls = seen.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "Bearer sk-server-key");
    let body = &calls[0].1;
    assert_eq!(body["session"]["type"], "transcription");
    assert_eq!(
        body["session"]["audio"]["input"]["transcription"]["model"],
        "gpt-live-transcribe"
    );
    let input = &body["session"]["audio"]["input"];
    assert_eq!(input["transcription"]["languages"], json!(["pt"]));
    assert!(input["transcription"].get("language").is_none());
    assert!(input["turn_detection"].is_null() && input.get("turn_detection").is_some());
    assert_eq!(input["format"], json!({"type": "audio/pcm", "rate": 24000}));
    assert_eq!(body["expires_after"]["seconds"], 600);

    let dump: Vec<(String,)> =
        sqlx::query_as("SELECT row_to_json(t)::text FROM realtime_token_audit t")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(dump.iter().all(|(j,)| !j.contains("ek_upstream_secret")));
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn openai_failure_is_a_502_without_leaking_upstream_details(pool: PgPool) {
    let (url, _) = fake_openai(StatusCode::UNAUTHORIZED).await;
    let app = App::with(pool, openai(url), high_limits());
    let (a, _) = app.tokens_for("a@example.com").await;
    let r = app
        .json(
            Method::POST,
            "/v1/realtime-tokens",
            Some(&a),
            &realtime_body(),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert!(!r.body.contains("nope") && !r.body.contains("sk-server-key"));

    let dead = App::with(
        app.pool.clone(),
        openai("http://127.0.0.1:1".into()),
        high_limits(),
    );
    let r = dead
        .json(
            Method::POST,
            "/v1/realtime-tokens",
            Some(&a),
            &realtime_body(),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
}

// ---------- rate limits ----------

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn sign_in_is_limited_per_email_and_per_ip(pool: PgPool) {
    let limits = Limits {
        signin_per_email: 3,
        signin_per_ip: 6,
        ..high_limits()
    };
    let app = App::with(pool, RealtimeConfig::Fake, limits);
    let csrf = app.open_login().await;
    for _ in 0..3 {
        let r = app
            .login_post(
                "/authorize/signin",
                &csrf,
                &[("email", "ada@example.com"), ("password", "wrong password")],
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = app
        .login_post(
            "/authorize/signin",
            &csrf,
            &[("email", "ADA@example.com"), ("password", "wrong password")],
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.header("retry-after").parse::<u64>().unwrap() > 0);

    // Other emails from the same address hit the per-IP budget (4 used, 2 more allowed).
    for i in 0..2 {
        let email = format!("user{i}@example.com");
        let r = app
            .login_post(
                "/authorize/signin",
                &csrf,
                &[("email", email.as_str()), ("password", "wrong password")],
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = app
        .login_post(
            "/authorize/signin",
            &csrf,
            &[("email", "last@example.com"), ("password", "x")],
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn magic_link_sends_are_limited_per_email(pool: PgPool) {
    let limits = Limits {
        mail_per_email: 2,
        ..high_limits()
    };
    let app = App::with(pool, RealtimeConfig::Fake, limits);
    let csrf = app.open_login().await;
    for _ in 0..2 {
        assert_eq!(
            app.login_post("/authorize/magic", &csrf, &[("email", "a@example.com")])
                .await
                .status,
            StatusCode::OK
        );
    }
    let r = app
        .login_post("/authorize/magic", &csrf, &[("email", "a@example.com")])
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers.contains_key("retry-after"));
    assert_eq!(app.take_mail().len(), 2);
    assert_eq!(
        app.login_post("/authorize/magic", &csrf, &[("email", "b@example.com")])
            .await
            .status,
        StatusCode::OK
    );
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn token_endpoint_is_limited_per_ip(pool: PgPool) {
    let limits = Limits {
        token_per_ip: 3,
        ..high_limits()
    };
    let app = App::with(pool, RealtimeConfig::Fake, limits);
    let body =
        json!({"grant_type":"refresh_token","refresh_token":"x","client_id":"sotto-desktop"});
    for _ in 0..3 {
        assert_eq!(
            app.json(Method::POST, "/v1/oauth/token", None, &body)
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    let r = app.json(Method::POST, "/v1/oauth/token", None, &body).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.header("retry-after").parse::<u64>().unwrap() > 0);
    let other = app
        .send_from(
            Request::post("/v1/oauth/token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            "10.9.9.9:1",
        )
        .await;
    assert_eq!(other.status, StatusCode::BAD_REQUEST);
}

// ---------- misc ----------

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn healthz_and_body_limit(pool: PgPool) {
    let app = App::new(pool);
    let r = app.get("/healthz").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json::<Value>()["status"], "ok");

    let big = "a".repeat(20 * 1024);
    let r = app
        .json(
            Method::POST,
            "/v1/oauth/token",
            None,
            &json!({"grant_type":"refresh_token","refresh_token":big,"client_id":"sotto-desktop"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
}
