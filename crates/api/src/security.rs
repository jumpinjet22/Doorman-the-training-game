//! Request hardening that applies to every route: security headers, the
//! `Origin` check on anything that changes state, and working out which
//! network a request came from.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;

const PERMISSIONS_POLICY_HEADER: HeaderName = HeaderName::from_static("permissions-policy");
const CROSS_ORIGIN_OPENER_POLICY_HEADER: HeaderName =
    HeaderName::from_static("cross-origin-opener-policy");
const CROSS_ORIGIN_RESOURCE_POLICY_HEADER: HeaderName =
    HeaderName::from_static("cross-origin-resource-policy");

/// Scripts come only from this site and Cloudflare's Turnstile widget; no
/// inline script or style anywhere.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; \
    script-src 'self' https://challenges.cloudflare.com; \
    frame-src https://challenges.cloudflare.com; \
    style-src 'self'; img-src 'self'; font-src 'self'; connect-src 'self'; \
    base-uri 'none'; form-action 'self'; frame-ancestors 'none'";
/// Two years. No `preload`: that's a commitment to make once the domain is settled.
pub const STRICT_TRANSPORT_SECURITY: &str = "max-age=63072000; includeSubDomains";
/// Turnstile needs to see which site it's on, so send the origin and nothing more.
pub const REFERRER_POLICY: &str = "strict-origin";
/// The game uses no device or payment APIs, so neither the page nor the
/// Turnstile iframe gets them. (Clipboard stays: the share card copies.)
pub const PERMISSIONS_POLICY: &str = "accelerometer=(), bluetooth=(), camera=(), \
    display-capture=(), geolocation=(), gyroscope=(), hid=(), magnetometer=(), \
    microphone=(), midi=(), payment=(), serial=(), usb=()";
/// Keeps other sites' windows out of our browsing context group. It only
/// governs top-level windows (popups and openers), not iframes, so the
/// Turnstile iframe is unaffected; there is no COEP, which is what would
/// constrain it.
pub const CROSS_ORIGIN_OPENER_POLICY: &str = "same-origin";
/// Nothing we serve is meant to be loaded by another site.
pub const CROSS_ORIGIN_RESOURCE_POLICY: &str = "same-origin";

/// Adds the security headers to every response, and `no-store` to any
/// response that didn't choose its own caching: pages and API answers are
/// personal to the player. HSTS goes out only when the site is served over
/// https, so a plain-http deploy (or a proxy that forwards http) never
/// sends it.
pub async fn headers(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, REFERRER_POLICY),
        (header::X_FRAME_OPTIONS, "DENY"),
        (PERMISSIONS_POLICY_HEADER, PERMISSIONS_POLICY),
        (
            CROSS_ORIGIN_OPENER_POLICY_HEADER,
            CROSS_ORIGIN_OPENER_POLICY,
        ),
        (
            CROSS_ORIGIN_RESOURCE_POLICY_HEADER,
            CROSS_ORIGIN_RESOURCE_POLICY,
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    if state.public_origin.starts_with("https://") {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(STRICT_TRANSPORT_SECURITY),
        );
    }
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

/// Refuses any request that could change something unless the browser says
/// it came from our own page. Browsers always send `Origin` on a POST, so a
/// missing one is refused too.
pub async fn same_origin(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if !safe && !from_origin(request.headers(), &state.public_origin) {
        return (StatusCode::FORBIDDEN, "Cross-origin request refused.").into_response();
    }
    next.run(request).await
}

fn from_origin(headers: &HeaderMap, origin: &str) -> bool {
    let sent: Vec<_> = headers.get_all(header::ORIGIN).iter().collect();
    matches!(sent.as_slice(), [only] if only.as_bytes() == origin.as_bytes())
}

/// The client's address: from the header a trusted proxy sets, when one is
/// configured, otherwise the TCP peer. None when neither is available.
pub struct ClientIp(pub Option<IpAddr>);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let ip = match &state.client_ip_header {
            // A proxy appends the address it saw, so with a list the last
            // entry is the one to trust; earlier ones the client could forge.
            Some(name) => parts
                .headers
                .get_all(name)
                .iter()
                .next_back()
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.rsplit(',').next())
                .and_then(|value| value.trim().parse().ok()),
            None => parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| addr.ip()),
        };
        Ok(ClientIp(ip))
    }
}
