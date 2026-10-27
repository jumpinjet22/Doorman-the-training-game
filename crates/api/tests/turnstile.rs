//! The real Cloudflare verifier, against a fake siteverify endpoint: what it
//! sends, and how it reads every kind of answer. Anything but a clear pass
//! must fail.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Form, Router};
use dorman_api::turnstile::CloudflareVerifier;
use dorman_core::{TurnstileVerifier, Verdict};

type Seen = Arc<Mutex<Vec<HashMap<String, String>>>>;

/// Serves `status` and `body` to every POST, recording the forms it got.
async fn fake_cloudflare(status: StatusCode, body: &'static str) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let record = seen.clone();
    let app = Router::new().route(
        "/siteverify",
        post(move |Form(form): Form<HashMap<String, String>>| {
            record.lock().unwrap().push(form);
            async move { (status, body).into_response() }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/siteverify", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

async fn verdict(status: StatusCode, body: &'static str) -> Verdict {
    let (url, _) = fake_cloudflare(status, body).await;
    CloudflareVerifier::with_url("secret".into(), url)
        .verify("token", None)
        .await
}

#[tokio::test]
async fn a_pass_sends_the_secret_token_and_client_address() {
    let (url, seen) = fake_cloudflare(
        StatusCode::OK,
        r#"{"success": true, "error-codes": [], "hostname": "example.com"}"#,
    )
    .await;
    let verifier = CloudflareVerifier::with_url("the-secret".into(), url);
    let ip: IpAddr = "203.0.113.7".parse().unwrap();
    assert_eq!(
        verifier.verify("the-token", Some(ip)).await,
        Verdict::Passed
    );
    assert_eq!(verifier.verify("other", None).await, Verdict::Passed);

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["secret"], "the-secret");
    assert_eq!(seen[0]["response"], "the-token");
    assert_eq!(seen[0]["remoteip"], "203.0.113.7");
    assert!(!seen[1].contains_key("remoteip"));
}

#[tokio::test]
async fn forged_expired_and_reused_tokens_are_rejected() {
    for codes in [
        r#"{"success": false, "error-codes": ["invalid-input-response"]}"#,
        r#"{"success": false, "error-codes": ["timeout-or-duplicate"]}"#,
        r#"{"success": false, "error-codes": ["missing-input-response"]}"#,
        r#"{"success": false}"#,
    ] {
        assert!(
            matches!(verdict(StatusCode::OK, codes).await, Verdict::Rejected(_)),
            "{codes}"
        );
    }
    assert_eq!(
        verdict(
            StatusCode::OK,
            r#"{"success": false, "error-codes": ["timeout-or-duplicate"]}"#
        )
        .await,
        Verdict::Rejected("timeout-or-duplicate".into())
    );
}

#[tokio::test]
async fn trouble_on_cloudflares_side_or_ours_is_unavailable_never_a_pass() {
    for (status, body) in [
        (
            StatusCode::OK,
            r#"{"success": false, "error-codes": ["internal-error"]}"#,
        ),
        (
            StatusCode::OK,
            r#"{"success": false, "error-codes": ["invalid-input-secret"]}"#,
        ),
        (StatusCode::INTERNAL_SERVER_ERROR, r#"{"success": true}"#),
        (StatusCode::BAD_GATEWAY, "upstream down"),
        (StatusCode::OK, "<html>maintenance</html>"),
        (StatusCode::OK, r#"{"success": "yes"}"#),
        (StatusCode::OK, r#"{}"#),
    ] {
        let got = verdict(status, body).await;
        assert!(
            matches!(got, Verdict::Unavailable(_)),
            "{status} {body}: {got:?}"
        );
    }
}

#[tokio::test]
async fn an_unreachable_cloudflare_is_unavailable() {
    // Bind a port, then free it, so nothing is listening there.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/siteverify", listener.local_addr().unwrap());
    drop(listener);
    let got = CloudflareVerifier::with_url("secret".into(), url)
        .verify("token", None)
        .await;
    assert!(matches!(got, Verdict::Unavailable(_)), "{got:?}");
}

/// What a real pass for our vote form looks like.
const PASS_FOR_US: &str =
    r#"{"success": true, "error-codes": [], "hostname": "dorman.test", "action": "vote"}"#;

async fn verdict_for_us(body: &'static str) -> Verdict {
    let (url, _) = fake_cloudflare(StatusCode::OK, body).await;
    CloudflareVerifier::with_url("secret".into(), url)
        .expecting("dorman.test")
        .verify("token", None)
        .await
}

#[tokio::test]
async fn a_pass_must_name_our_host_and_the_vote_action() {
    assert_eq!(verdict_for_us(PASS_FOR_US).await, Verdict::Passed);
    for body in [
        // Solved on another site that shares the site key.
        r#"{"success": true, "hostname": "evil.example", "action": "vote"}"#,
        r#"{"success": true, "hostname": "dorman.test.evil.example", "action": "vote"}"#,
        // Solved for another action, or none.
        r#"{"success": true, "hostname": "dorman.test", "action": "login"}"#,
        r#"{"success": true, "hostname": "dorman.test", "action": ""}"#,
        r#"{"success": true, "hostname": "dorman.test"}"#,
        r#"{"success": true, "action": "vote"}"#,
        r#"{"success": true}"#,
    ] {
        let got = verdict_for_us(body).await;
        assert!(matches!(got, Verdict::Rejected(_)), "{body}: {got:?}");
    }
    // A failure is still a failure, whatever it names.
    assert_eq!(
        verdict_for_us(
            r#"{"success": false, "error-codes": ["timeout-or-duplicate"], "hostname": "dorman.test", "action": "vote"}"#
        )
        .await,
        Verdict::Rejected("timeout-or-duplicate".into())
    );
}
