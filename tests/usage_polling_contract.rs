//! Contract tests for the `/api/oauth/usage` polling path (`src/poller.rs`).
//!
//! These pin the client side of the contract documented in
//! `docs/research/usage-tracking.md`:
//!
//! - `GET https://api.anthropic.com/api/oauth/usage` carrying the mandatory
//!   `Authorization: Bearer`, `anthropic-beta: oauth-2025-04-20` and
//!   `claude-code/*` User-Agent headers (the endpoint 401s without the beta
//!   header even with a valid OAuth token).
//! - Response windows with a `utilization` float (0-100) and an ISO 8601
//!   `resets_at` timestamp carrying a timezone offset; fields arrive as
//!   `null` when not applicable for the current plan, which the poller must
//!   treat as non-binding rather than failing the whole poll.
//! - Token refresh against `POST https://platform.claude.com/v1/oauth/token`
//!   once `expiresAt` falls within the refresh threshold, with the rotated
//!   credentials persisted back to the credentials file.
//! - Malformed bodies, 401/429 and network failures surface as poll errors;
//!   the usage endpoint is never retried client-side (it self-rate-limits),
//!   while a failed token refresh retries exactly once before surfacing, and
//!   sustained refresh failures escalate to an alert.
//!
//! Failure-mode and boundary coverage pinned on top of that baseline
//! (claudego-06974a01):
//!
//! - Credentials-file failure modes: a missing file and malformed JSON fail
//!   with their own `PollerError`s, the corrupted-credential guards (empty
//!   tokens, zero expiry) reject the file before any request is attempted,
//!   and a file that vanishes after a good reading degrades to the stale
//!   fallback like any other auth failure.
//! - Fully expired (past-dated) tokens refresh exactly like near-expiry ones.
//! - The 5-minute refresh threshold itself (usage-tracking.md §5: refresh
//!   when `now + 300_000 >= expiresAt`): 305s out no refresh fires, 295s out
//!   exactly one does.
//! - A refresh endpoint answering 200 with a malformed body is a refresh
//!   failure, not a pass: retried once, failure counter incremented.
//! - Window boundaries: 0% / 100% / over-saturation utilization pass through
//!   unclamped; `resets_at` exactly now computes ~0h and both documented ISO
//!   shapes (`...Z` and `...+00:00`) compute identically; an empty response
//!   object leaves every window non-binding; the legacy top-level
//!   `weekly_scoped` field is ignored in favour of the authoritative
//!   `limits[]` entry; and the `limits[]` additive-tolerance contract has a
//!   precise boundary — absent and null fields are tolerated, a wrong-typed
//!   present value is fatal.
//!
//! Contract-completion coverage (claudego-48fd3fa5), closing the two gaps the
//! baseline left in the now-documented contract (usage-tracking.md §10):
//!
//! - A **partially populated** response — some windows present, others absent
//!   outright — parses the present windows and defaults the absent ones: the
//!   middle case between the fully documented shape and the empty object.
//! - The **safe-fallback boundary**: a failure from the usage endpoint itself
//!   (here the documented 429 self-rate-limit response) propagates to the
//!   caller even when a cached reading exists — only the auth path degrades
//!   to stale data.
//!
//! Timeout coverage (claudego-0840eab5), the one failure class transport
//! errors cannot represent — the endpoint that accepts the connection and
//! never answers. ureq does not time out response reads on its own, so before
//! the request timeout these scenarios blocked `poll()` — and with it the
//! governor's whole observe cycle — forever:
//!
//! - A hung usage endpoint surfaces as `ApiRequestFailed` within the
//!   configured request timeout instead of blocking.
//! - A hung token-refresh endpoint burns both retry attempts within the
//!   timeout and then degrades to the cached reading (stale fallback), like
//!   any other refresh failure.
//! - A response that is merely slow but inside the timeout still succeeds,
//!   so the bound cannot silently tighten below real-world latency.
//!
//! A local mockito server stands in for the two endpoints;
//! [`Poller::with_endpoints`] / [`Poller::with_refresh_retry_delay`] point the
//! poller at it. All tests serialize on one lock because the refresh-failure
//! counter in `poller.rs` is process-global state.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use claude_governor::poller::{Poller, PollerError};

/// The refresh-failure counter in poller.rs is `static mut` shared by every
/// Poller in the process, and cargo runs a test binary's tests on parallel
/// threads — so every test that drives a refresh path holds this lock. A test
/// panicking while holding it poisons the mutex; later tests unlock the
/// poisoned guard so one failure doesn't cascade into all the others.
static REFRESH_COUNTER_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    REFRESH_COUNTER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Credentials file in the documented shape (usage-tracking.md §5).
fn write_credentials(dir: &Path, access: &str, refresh: &str, expires_at_ms: i64) -> String {
    let path = dir.join(".credentials.json");
    let body = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": access,
            "refreshToken": refresh,
            "expiresAt": expires_at_ms,
            "scopes": ["user:inference"],
        }
    });
    std::fs::write(&path, body.to_string()).expect("write credentials file");
    path.to_string_lossy().into_owned()
}

/// Credentials file with an arbitrary body, for malformed/corrupted-file
/// fixtures that must exercise the poller's own validation rather than
/// serde's.
fn write_raw_credentials(dir: &Path, body: &str) -> String {
    let path = dir.join(".credentials.json");
    std::fs::write(&path, body).expect("write raw credentials file");
    path.to_string_lossy().into_owned()
}

/// 1h out: comfortably outside the 5-minute refresh threshold.
fn far_future_expiry_ms() -> i64 {
    Utc::now().timestamp_millis() + 3_600_000
}

/// 1 minute out: inside the 5-minute refresh threshold.
fn expiring_soon_expiry_ms() -> i64 {
    Utc::now().timestamp_millis() + 60_000
}

/// A poller wired to a local mock server standing in for both endpoints.
fn contract_poller(creds_path: &str, server_url: &str) -> Poller {
    Poller::with_credentials_path(Some(creds_path.to_string()))
        .expect("a valid credentials path should build a poller")
        .with_endpoints(
            server_url.to_string(),
            format!("{server_url}/v1/oauth/token"),
        )
        // The production 5s retry delay would sleep real wall-clock time in
        // every retry test.
        .with_refresh_retry_delay(Duration::ZERO)
}

/// The documented §2 response shape, extended with the generic `limits[]`
/// array the poller parses additively. Includes the fields the poller has no
/// mapping for (`seven_day_sonnet`, `extra_usage`, ...) and null windows —
/// both must be tolerated.
fn documented_usage_body() -> String {
    r#"{
        "five_hour": {"utilization": 14.0, "resets_at": "2026-03-18T13:59:59.918852+00:00"},
        "seven_day": {"utilization": 82.0, "resets_at": "2026-03-20T03:00:00.918880+00:00"},
        "seven_day_oauth_apps": null,
        "seven_day_opus": null,
        "seven_day_sonnet": {"utilization": 72.0, "resets_at": "2026-03-20T03:59:59.918891+00:00"},
        "seven_day_cowork": null,
        "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null, "utilization": null},
        "limits": [
            {"kind": "session", "percent": 14, "resets_at": "2026-03-18T13:59:59Z",
             "scope": null, "is_active": true},
            {"kind": "weekly_scoped", "percent": 79, "resets_at": "2026-03-20T03:59:59Z",
             "scope": {"model": {"id": "claude-fable-5", "display_name": "Fable"}},
             "is_active": true}
        ]
    }"#
    .to_string()
}

/// A successful token-refresh response carrying rotated credentials.
fn refresh_response_body() -> String {
    format!(
        r#"{{"accessToken":"fresh-access-token","refreshToken":"rotated-refresh-token","expiresAt":{}}}"#,
        far_future_expiry_ms()
    )
}

/// A localhost URL whose listener is guaranteed closed: the port is bound and
/// immediately released, so connecting to it is refused deterministically.
fn dead_endpoint_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// A localhost URL whose listener is bound and held but never answered: the
/// kernel completes the TCP handshake into the accept backlog, the client
/// writes its request, and no response ever comes — a deterministic black
/// hole. Distinct from [`dead_endpoint_url`] (connection refused): this is
/// the hang the request timeout exists to bound, and without it a poll
/// against this URL blocks forever. The listener is returned so the caller
/// keeps the port open for the test's duration; dropping it would turn the
/// hang back into a refusal.
fn hung_endpoint() -> (String, std::net::TcpListener) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    (format!("http://127.0.0.1:{port}"), listener)
}

/// A one-shot HTTP endpoint that answers with a valid 200 carrying `body`,
/// but only after `delay` (mockito 1.x cannot delay responses). Complement
/// to [`hung_endpoint`]: pins that a merely slow response inside the request
/// timeout is still a normal success. The serving thread owns the listener,
/// so the URL is all the caller needs.
fn slow_endpoint(delay: Duration, body: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        use std::io::Read;
        let (mut stream, _) = listener.accept().expect("accept the one connection");
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf); // the request head; contents irrelevant
        std::thread::sleep(delay);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        std::io::Write::write_all(&mut stream, response.as_bytes()).ok();
    });
    format!("http://127.0.0.1:{port}")
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

#[test]
fn usage_request_carries_documented_auth_headers() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // Every header the documented contract requires. A request missing any of
    // them does not match this mock, so the poll fails and the test fails.
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .match_header("authorization", "Bearer access-token-1")
        .match_header("anthropic-beta", "oauth-2025-04-20")
        .match_header(
            "user-agent",
            mockito::Matcher::Regex(r"^claude-code/".to_string()),
        )
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("poll with documented headers");

    usage.assert(); // exactly one hit, all matched headers present
    assert!(!data.stale);
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

#[test]
fn documented_response_shape_parses_end_to_end() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("documented response must parse");

    usage.assert();
    assert!(!data.stale);

    // Legacy top-level windows.
    assert_eq!(data.five_hour_utilization, 14.0);
    assert_eq!(data.five_hour_resets_at, "2026-03-18T13:59:59.918852+00:00");
    assert_eq!(data.seven_day_utilization, 82.0);

    // The generic limits[] array parsed additively, and weekly_scoped was
    // resolved from it (model-agnostic source of truth).
    assert_eq!(data.limits.len(), 2);
    assert_eq!(data.weekly_scoped_utilization, 79.0);
    assert_eq!(data.weekly_scoped_model.as_deref(), Some("Fable"));
    assert_eq!(data.weekly_scoped_resets_at, "2026-03-20T03:59:59Z");
}

#[test]
fn null_windows_are_treated_as_non_binding() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // Fields are null when not applicable for the current plan (§2: "Fields
    // are null when not applicable"). This must not fail the poll.
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(r#"{"five_hour": null, "seven_day": null, "weekly_scoped": null}"#)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("null windows must not fail the poll");

    usage.assert();
    assert_eq!(data.five_hour_utilization, 0.0);
    assert_eq!(data.five_hour_resets_at, "");
    assert_eq!(data.five_hour_hours_remaining, 168.0);
    assert_eq!(data.seven_day_utilization, 0.0);
    assert_eq!(data.seven_day_hours_remaining, 168.0);
    assert_eq!(data.weekly_scoped_utilization, 0.0);
    assert_eq!(data.weekly_scoped_hours_remaining, 168.0);
    assert!(data.weekly_scoped_model.is_none());
    assert!(data.limits.is_empty());
}

// ---------------------------------------------------------------------------
// Reset timestamps
// ---------------------------------------------------------------------------

#[test]
fn resets_at_offset_form_yields_hours_remaining() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // The documented resets_at shape: microseconds + explicit +00:00 offset.
    let resets_at = (Utc::now() + chrono::Duration::seconds(7200))
        .to_rfc3339_opts(SecondsFormat::Micros, false);
    let body = format!(r#"{{"five_hour": {{"utilization": 50.0, "resets_at": "{resets_at}"}}}}"#);
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(body)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("offset timestamp must parse");

    usage.assert();
    let hours = data.five_hour_hours_remaining;
    assert!(
        (1.9..=2.1).contains(&hours),
        "expected ~2h remaining, got {hours}"
    );
}

#[test]
fn resets_at_in_the_past_yields_negative_hours() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    let resets_at = (Utc::now() - chrono::Duration::seconds(3600))
        .to_rfc3339_opts(SecondsFormat::Micros, false);
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(format!(
            r#"{{"five_hour": {{"utilization": 50.0, "resets_at": "{resets_at}"}}}}"#
        ))
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("past timestamp must parse");

    usage.assert();
    let hours = data.five_hour_hours_remaining;
    assert!(
        (-1.1..=-0.9).contains(&hours),
        "expected ~-1h remaining, got {hours}"
    );
}

#[test]
fn unparseable_resets_at_is_tolerated_as_zero_hours() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // A window whose resets_at cannot be parsed degrades to 0h remaining
    // rather than failing the poll; utilization survives.
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(r#"{"five_hour": {"utilization": 42.0, "resets_at": "not-a-timestamp"}}"#)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller
        .poll()
        .expect("unparseable resets_at must be tolerated");

    usage.assert();
    assert_eq!(data.five_hour_utilization, 42.0);
    assert_eq!(data.five_hour_hours_remaining, 0.0);
}

// ---------------------------------------------------------------------------
// Malformed responses
// ---------------------------------------------------------------------------

#[test]
fn malformed_json_fails_the_poll_with_parse_error() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    let _usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body("{not json")
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let err = poller
        .poll()
        .expect_err("truncated JSON must fail the poll");
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::ParseError(_))
        ),
        "expected ParseError, got: {err}"
    );
}

#[test]
fn wrong_typed_window_field_fails_the_poll() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // Contrast with null tolerance: a window that is present but garbage-typed
    // is a hard parse failure, not a silent zero.
    let _usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(r#"{"five_hour": {"utilization": "high", "resets_at": "2026-03-18T13:59:59Z"}}"#)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let err = poller
        .poll()
        .expect_err("a string utilization must fail the poll");
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::ParseError(_))
        ),
        "expected ParseError, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// 401 / 429 responses
// ---------------------------------------------------------------------------

#[test]
fn unauthorized_401_surfaces_error_without_retry() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(401)
        .with_body(r#"{"error":{"type":"authentication_error","message":"invalid token"}}"#)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let err = poller.poll().expect_err("401 must fail the poll");
    let msg = err.to_string();
    assert!(msg.contains("401"), "error must surface the status: {msg}");

    // The usage endpoint self-rate-limits and is never retried client-side:
    // exactly one request was made.
    usage.assert();
}

#[test]
fn rate_limited_429_surfaces_error_without_retry() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // The documented self-rate-limit response (§2).
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(429)
        .with_body(
            r#"{"error":{"type":"rate_limit_error","message":"Rate limited. Please try again later."}}"#,
        )
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let err = poller.poll().expect_err("429 must fail the poll");
    let msg = err.to_string();
    assert!(msg.contains("429"), "error must surface the status: {msg}");

    // Surfaced to the governor cycle, not retried in-process.
    usage.assert();
}

// ---------------------------------------------------------------------------
// Network failures
// ---------------------------------------------------------------------------

#[test]
fn unreachable_usage_endpoint_surfaces_request_error() {
    let _guard = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    let mut poller = Poller::with_credentials_path(Some(creds))
        .expect("a valid credentials path should build a poller")
        .with_endpoints(dead_endpoint_url(), dead_endpoint_url())
        .with_refresh_retry_delay(Duration::ZERO);

    let err = poller
        .poll()
        .expect_err("a refused connection must fail the poll");
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::ApiRequestFailed(_))
        ),
        "expected ApiRequestFailed, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Token refresh
// ---------------------------------------------------------------------------

#[test]
fn expiring_token_triggers_refresh_and_new_token_is_used() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "stale-access-token",
        "stored-refresh-token",
        expiring_soon_expiry_ms(), // inside the refresh threshold
    );

    // The refresh POST must carry the documented grant type and the stored
    // refresh token, and must return rotated credentials.
    let refresh = server
        .mock("POST", "/v1/oauth/token")
        .match_header("content-type", "application/json")
        .match_body(mockito::Matcher::JsonString(
            r#"{"grantType":"refresh_token","refreshToken":"stored-refresh-token"}"#.to_string(),
        ))
        .with_status(200)
        .with_body(refresh_response_body())
        .create();

    // The usage call must use the refreshed access token, not the stale one.
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .match_header("authorization", "Bearer fresh-access-token")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("refresh + usage must succeed");
    refresh.assert();
    usage.assert();
    assert!(!data.stale);

    // Rotated credentials are persisted back to the credentials file.
    let persisted = std::fs::read_to_string(&creds).expect("reread credentials file");
    assert!(persisted.contains("claudeAiOauth"));
    assert!(persisted.contains("fresh-access-token"));
    assert!(persisted.contains("rotated-refresh-token"));
    assert!(!persisted.contains("stale-access-token"));
    assert!(!persisted.contains("stored-refresh-token"));
}

#[test]
fn fresh_token_polls_without_refreshing() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(), // outside the refresh threshold
    );

    let refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(200)
        .with_body(refresh_response_body())
        .create();
    let usage = server
        .mock("GET", "/api/oauth/usage")
        // If a refresh had happened, the bearer would be "fresh-access-token"
        // and this match would fail.
        .match_header("authorization", "Bearer access-token-1")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("poll with a fresh token");

    usage.assert();
    assert!(!data.stale);

    // Credentials on disk are untouched when no refresh was needed.
    let persisted = std::fs::read_to_string(&creds).expect("reread credentials file");
    assert!(persisted.contains("access-token-1"));
    assert!(persisted.contains("refresh-token-1"));
    let _ = refresh; // registered only so an accidental refresh cannot 501 the poll
}

// ---------------------------------------------------------------------------
// Retry behavior
// ---------------------------------------------------------------------------

#[test]
fn failed_refresh_retries_once_then_surfaces_error() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");

    // Phase A: normalize the process-global failure counter to zero with one
    // successful refresh, so this test's assertions don't depend on which
    // tests ran before it.
    let creds = write_credentials(
        dir.path(),
        "warmup-access-token",
        "warmup-refresh-token",
        expiring_soon_expiry_ms(),
    );
    let warmup_refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(200)
        .with_body(refresh_response_body())
        .create();
    let warmup_usage = server
        .mock("GET", "/api/oauth/usage")
        .match_header("authorization", "Bearer fresh-access-token")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();
    let mut poller = contract_poller(&creds, &server.url());
    poller.poll().expect("warmup refresh must succeed");
    warmup_refresh.assert();
    warmup_usage.assert();

    // Phase B: a failing token endpoint. The refresh must be attempted
    // exactly twice (initial + one retry) before surfacing. A fresh poller
    // instance so no cached reading exists to fall back to — the failure
    // counter itself is process-global and survives the new instance.
    let creds = write_credentials(
        dir.path(),
        "doomed-access-token",
        "doomed-refresh-token",
        expiring_soon_expiry_ms(),
    );
    let mut poller = contract_poller(&creds, &server.url());
    let failing_refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(500)
        .with_body("boom")
        .expect(2)
        .create();

    let err = poller
        .poll()
        .expect_err("failed refresh with no prior data must fail the poll");
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::TokenRefreshFailed(_))
        ),
        "expected TokenRefreshFailed, got: {err}"
    );
    failing_refresh.assert(); // exactly two attempts

    // One failed refresh cycle increments the failure counter by one.
    assert_eq!(Poller::refresh_failure_count(), 1);
}

#[test]
fn poll_recovers_after_transient_refresh_outage() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        expiring_soon_expiry_ms(),
    );

    // First poll: the token endpoint 500s on both attempts.
    let failing_refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(500)
        .with_body("boom")
        .expect(2)
        .create();
    let mut poller = contract_poller(&creds, &server.url());
    assert!(
        poller.poll().is_err(),
        "poll during a refresh outage must fail"
    );
    failing_refresh.assert();

    // Second poll: the outage is over (mocks are matched newest-first, so
    // this new pair wins over the saturated failing mock).
    let working_refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(200)
        .with_body(refresh_response_body())
        .create();
    let working_usage = server
        .mock("GET", "/api/oauth/usage")
        .match_header("authorization", "Bearer fresh-access-token")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();
    let data = poller.poll().expect("recovery poll must succeed");

    working_refresh.assert();
    working_usage.assert();
    assert!(!data.stale);
    // A successful refresh clears the failure counter.
    assert_eq!(Poller::refresh_failure_count(), 0);
}

#[test]
fn refresh_failure_falls_back_to_stale_data() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    // Poll #1: success — this reading is what the stale fallback will serve.
    let mut poller = contract_poller(&creds, &server.url());
    let fresh = poller.poll().expect("first poll must succeed");
    usage.assert();
    assert!(!fresh.stale);

    // Now the token expires on disk and the refresh endpoint starts failing.
    write_credentials(
        dir.path(),
        "expiring-access-token",
        "expiring-refresh-token",
        expiring_soon_expiry_ms(),
    );
    let failing_refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(500)
        .with_body("boom")
        .expect(2)
        .create();

    // Poll #2: refresh fails, but cached data is served with stale=true
    // rather than failing the cycle.
    let stale = poller
        .poll()
        .expect("poll must fall back to cached data instead of failing");
    failing_refresh.assert();
    assert!(stale.stale);
    assert_eq!(stale.five_hour_utilization, fresh.five_hour_utilization);
    assert_eq!(stale.seven_day_utilization, fresh.seven_day_utilization);
    // The reading's timestamp is preserved, so consumers can see its age.
    assert_eq!(stale.timestamp, fresh.timestamp);
}

#[test]
fn sustained_refresh_failures_escalate_to_alert() {
    let _guard = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        expiring_soon_expiry_ms(),
    );

    let dead = dead_endpoint_url();
    let mut poller = Poller::with_credentials_path(Some(creds))
        .expect("a valid credentials path should build a poller")
        .with_endpoints(dead.clone(), dead)
        .with_refresh_retry_delay(Duration::ZERO);

    // Poll until the shared failure counter crosses the escalation threshold
    // (3 consecutive failed refresh cycles). Wherever the counter starts, the
    // poll that crosses the threshold must surface MaxRefreshFailures.
    const ESCALATION_THRESHOLD: u32 = 3;
    let mut polls = 0;
    loop {
        polls += 1;
        assert!(
            polls <= ESCALATION_THRESHOLD + 2,
            "refresh failure counter never reached the escalation threshold"
        );
        let result = poller.poll();
        if Poller::refresh_failure_count() >= ESCALATION_THRESHOLD {
            let err = result.expect_err("the escalation poll must fail");
            assert!(
                matches!(
                    err.downcast_ref::<PollerError>(),
                    Some(PollerError::MaxRefreshFailures)
                ),
                "expected MaxRefreshFailures at the threshold, got: {err}"
            );
            break;
        }
        assert!(
            result.is_err(),
            "a poll before the threshold must fail, not silently succeed"
        );
    }

    // Crossing the threshold arms the HUMAN alert.
    assert!(poller.should_alert());
}

// ---------------------------------------------------------------------------
// Credentials-file failure modes
// ---------------------------------------------------------------------------

#[test]
fn missing_credentials_file_fails_with_credentials_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = dir.path().join("does-not-exist").join(".credentials.json");

    // Dead endpoints: if the poller somehow reached the network anyway, the
    // error would be ApiRequestFailed, not the variant asserted below.
    let dead = dead_endpoint_url();
    let mut poller = Poller::with_credentials_path(Some(creds.to_string_lossy().into_owned()))
        .expect("a valid credentials path should build a poller")
        .with_endpoints(dead.clone(), dead)
        .with_refresh_retry_delay(Duration::ZERO);

    let err = poller
        .poll()
        .expect_err("a missing credentials file must fail the poll");
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::CredentialsNotFound(_))
        ),
        "expected CredentialsNotFound, got: {err}"
    );
}

#[test]
fn malformed_credentials_json_fails_with_invalid_credentials() {
    let _guard = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_raw_credentials(dir.path(), "{not json");

    let dead = dead_endpoint_url();
    let mut poller = Poller::with_credentials_path(Some(creds))
        .expect("a valid credentials path should build a poller")
        .with_endpoints(dead.clone(), dead)
        .with_refresh_retry_delay(Duration::ZERO);

    let err = poller
        .poll()
        .expect_err("unparseable credentials JSON must fail the poll");
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::InvalidCredentials(_))
        ),
        "expected InvalidCredentials, got: {err}"
    );
}

#[test]
fn corrupted_credentials_are_rejected_before_any_network_call() {
    let dir = tempfile::tempdir().expect("tempdir");

    // The three guards from read_credentials: empty access token, empty
    // refresh token, zero expiry. Each is valid JSON, so serde alone would
    // accept it — the poller's own validation must catch it before any
    // request is attempted. Dead endpoints prove that: had the poller gone
    // to the network, the error would be a connection failure, not the
    // corruption message asserted below.
    let corrupted_bodies = [
        (
            "empty access token",
            r#"{"claudeAiOauth": {"accessToken": "", "refreshToken": "r", "expiresAt": 123}}"#,
        ),
        (
            "empty refresh token",
            r#"{"claudeAiOauth": {"accessToken": "a", "refreshToken": "", "expiresAt": 123}}"#,
        ),
        (
            "zero expiry",
            r#"{"claudeAiOauth": {"accessToken": "a", "refreshToken": "r", "expiresAt": 0}}"#,
        ),
    ];

    for (case, body) in corrupted_bodies {
        let creds = write_raw_credentials(dir.path(), body);
        let dead = dead_endpoint_url();
        let mut poller = Poller::with_credentials_path(Some(creds))
            .expect("a valid credentials path should build a poller")
            .with_endpoints(dead.clone(), dead)
            .with_refresh_retry_delay(Duration::ZERO);

        let err = match poller.poll() {
            Ok(data) => panic!("{case}: poll must fail, but succeeded with {data:?}"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Credentials corrupted"),
            "{case}: expected the corruption guard, got: {msg}"
        );
    }
}

#[test]
fn missing_credentials_file_serves_stale_data_when_cached_reading_exists() {
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    // Poll #1: a good reading to seed the cache.
    let mut poller = contract_poller(&creds, &server.url());
    let fresh = poller.poll().expect("first poll must succeed");
    usage.assert();
    assert!(!fresh.stale);

    // The credentials file then vanishes (e.g. wiped by a concurrent login).
    // A credential read failure is still a PollerError, so the poll must
    // degrade to the stale fallback exactly like a refresh failure — not
    // fail the cycle and not hit the network again.
    std::fs::remove_file(&creds).expect("remove credentials file");
    let stale = poller
        .poll()
        .expect("credential loss must fall back to cached data");
    assert!(stale.stale);
    assert_eq!(stale.five_hour_utilization, fresh.five_hour_utilization);
    assert_eq!(stale.timestamp, fresh.timestamp);
}

// ---------------------------------------------------------------------------
// Expired tokens (past-dated, not merely near expiry)
// ---------------------------------------------------------------------------

#[test]
fn expired_token_triggers_refresh_end_to_end() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    // Expired 10 minutes ago: a stronger trigger than the 60s-out case in
    // expiring_token_triggers_refresh_and_new_token_is_used. The dead bearer
    // must never reach the usage endpoint.
    let creds = write_credentials(
        dir.path(),
        "expired-access-token",
        "stored-refresh-token",
        Utc::now().timestamp_millis() - 600_000,
    );

    let refresh = server
        .mock("POST", "/v1/oauth/token")
        .match_header("content-type", "application/json")
        .match_body(mockito::Matcher::JsonString(
            r#"{"grantType":"refresh_token","refreshToken":"stored-refresh-token"}"#.to_string(),
        ))
        .with_status(200)
        .with_body(refresh_response_body())
        .expect(1)
        .create();
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .match_header("authorization", "Bearer fresh-access-token")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("expired token must refresh and poll");

    refresh.assert();
    usage.assert();
    assert!(!data.stale);

    // The rotated credentials replaced the expired ones on disk.
    let persisted = std::fs::read_to_string(&creds).expect("reread credentials file");
    assert!(persisted.contains("fresh-access-token"));
    assert!(!persisted.contains("expired-access-token"));
}

// ---------------------------------------------------------------------------
// Refresh threshold boundary (usage-tracking.md §5)
// ---------------------------------------------------------------------------

#[test]
fn refresh_threshold_boundary_end_to_end() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");

    // Phase A: 305s out — outside the documented 300s threshold. No refresh
    // may fire: the refresh mock expects zero hits and the usage call is
    // matched against the un-refreshed bearer only.
    let creds = write_credentials(
        dir.path(),
        "outside-threshold-token",
        "outside-refresh-token",
        Utc::now().timestamp_millis() + 305_000,
    );
    let no_refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(200)
        .with_body(refresh_response_body())
        .expect(0)
        .create();
    let usage_a = server
        .mock("GET", "/api/oauth/usage")
        .match_header("authorization", "Bearer outside-threshold-token")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    poller.poll().expect("poll 305s out must not refresh");

    no_refresh.assert(); // zero hits
    usage_a.assert();

    // Phase B: 295s out — inside the threshold. Exactly one refresh fires
    // and the rotated bearer is the one that reaches the usage endpoint.
    let creds = write_credentials(
        dir.path(),
        "inside-threshold-token",
        "inside-refresh-token",
        Utc::now().timestamp_millis() + 295_000,
    );
    let one_refresh = server
        .mock("POST", "/v1/oauth/token")
        .match_body(mockito::Matcher::JsonString(
            r#"{"grantType":"refresh_token","refreshToken":"inside-refresh-token"}"#.to_string(),
        ))
        .with_status(200)
        .with_body(refresh_response_body())
        .expect(1)
        .create();
    let usage_b = server
        .mock("GET", "/api/oauth/usage")
        .match_header("authorization", "Bearer fresh-access-token")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    poller.poll().expect("poll 295s out must refresh");

    one_refresh.assert(); // exactly one hit
    usage_b.assert();
}

// ---------------------------------------------------------------------------
// Malformed refresh responses
// ---------------------------------------------------------------------------

#[test]
fn refresh_endpoint_200_with_malformed_body_is_a_refresh_failure() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");

    // Phase A: normalize the process-global failure counter to zero.
    let creds = write_credentials(
        dir.path(),
        "warmup-access-token",
        "warmup-refresh-token",
        expiring_soon_expiry_ms(),
    );
    let warmup_refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(200)
        .with_body(refresh_response_body())
        .create();
    let warmup_usage = server
        .mock("GET", "/api/oauth/usage")
        .match_header("authorization", "Bearer fresh-access-token")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();
    let mut poller = contract_poller(&creds, &server.url());
    poller.poll().expect("warmup refresh must succeed");
    warmup_refresh.assert();
    warmup_usage.assert();

    // Phase B: the refresh endpoint answers 200 but with a body that cannot
    // parse as rotated credentials. That is a refresh failure, not a pass:
    // both attempts hit the endpoint (expect 2), the failure counter
    // increments, and the poll surfaces TokenRefreshFailed.
    let creds = write_credentials(
        dir.path(),
        "doomed-access-token",
        "doomed-refresh-token",
        expiring_soon_expiry_ms(),
    );
    let malformed_refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(200)
        .with_body("{not json")
        .expect(2)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let err = poller
        .poll()
        .expect_err("a malformed refresh body with no cached data must fail the poll");
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::TokenRefreshFailed(_))
        ),
        "expected TokenRefreshFailed, got: {err}"
    );
    malformed_refresh.assert(); // initial attempt + one retry
    assert_eq!(Poller::refresh_failure_count(), 1);
}

// ---------------------------------------------------------------------------
// Window boundaries
// ---------------------------------------------------------------------------

#[test]
fn utilization_boundaries_pass_through_verbatim() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // 0% (window never used), 100% (window exhausted) and 137.5% (the API
    // reporting past its cap). The governor's cutoff logic consumes these
    // raw, so any clamping or normalization at the poller would hide real
    // boundary conditions — all three must survive verbatim.
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(
            r#"{
                "five_hour": {"utilization": 0.0, "resets_at": "2026-03-18T13:59:59Z"},
                "seven_day": {"utilization": 100.0, "resets_at": "2026-03-20T03:00:00Z"},
                "limits": [
                    {"kind": "weekly_scoped", "percent": 137.5,
                     "resets_at": "2026-03-20T03:59:59Z",
                     "scope": {"model": {"id": "claude-fable-5", "display_name": "Fable"}}}
                ]
            }"#,
        )
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("boundary utilizations must parse");

    usage.assert();
    assert_eq!(data.five_hour_utilization, 0.0);
    assert_eq!(data.seven_day_utilization, 100.0);
    assert_eq!(data.weekly_scoped_utilization, 137.5);
    assert_eq!(data.weekly_scoped_model.as_deref(), Some("Fable"));
}

#[test]
fn resets_at_boundary_forms_and_exactly_now_compute_hours() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // Both documented ISO shapes must compute identically (§2/§6: an ISO 8601
    // datetime with timezone offset — the API sends `+00:00` micros form and
    // `Z` second form), and a reset exactly at poll time must compute ~0h
    // remaining, not an error.
    let now = Utc::now();
    let exactly_now_offset_form = now.to_rfc3339_opts(SecondsFormat::Micros, false);
    let one_hour_out_z_form =
        (now + chrono::Duration::seconds(3600)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let body = format!(
        r#"{{"five_hour": {{"utilization": 50.0, "resets_at": "{exactly_now_offset_form}"}},
            "seven_day": {{"utilization": 50.0, "resets_at": "{one_hour_out_z_form}"}}}}"#
    );
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(body)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("boundary reset timestamps must parse");

    usage.assert();
    let five_hour = data.five_hour_hours_remaining;
    assert!(
        (-0.1..=0.1).contains(&five_hour),
        "a reset exactly now must yield ~0h, got {five_hour}"
    );
    let seven_day = data.seven_day_hours_remaining;
    assert!(
        (0.9..=1.1).contains(&seven_day),
        "the Z-suffix form must yield ~1h, got {seven_day}"
    );
}

#[test]
fn empty_response_object_yields_all_windows_non_binding() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // The degenerate schema: no window keys at all. serde's field defaults
    // must absorb it exactly like explicit nulls.
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body("{}")
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller.poll().expect("an empty object must parse");

    usage.assert();
    assert!(!data.stale);
    assert_eq!(data.five_hour_utilization, 0.0);
    assert_eq!(data.five_hour_hours_remaining, 168.0);
    assert_eq!(data.seven_day_utilization, 0.0);
    assert_eq!(data.seven_day_hours_remaining, 168.0);
    assert_eq!(data.weekly_scoped_utilization, 0.0);
    assert_eq!(data.weekly_scoped_hours_remaining, 168.0);
    assert!(data.weekly_scoped_model.is_none());
    assert!(data.limits.is_empty());
}

#[test]
fn legacy_top_level_weekly_scoped_is_ignored_in_favor_of_limits() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // poll() never reads the legacy top-level weekly_scoped window: the
    // limits[] weekly_scoped entry is the authoritative model-agnostic
    // source (poller.rs poll()). A response carrying only the legacy field
    // must therefore leave weekly_scoped non-binding, not adopt the stale
    // legacy value.
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(
            r#"{
                "weekly_scoped": {"utilization": 99.0, "resets_at": "2026-03-20T03:59:59Z"},
                "five_hour": {"utilization": 10.0, "resets_at": "2026-03-18T13:59:59Z"},
                "limits": [
                    {"kind": "session", "percent": 10,
                     "resets_at": "2026-03-18T13:59:59Z", "scope": null}
                ]
            }"#,
        )
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller
        .poll()
        .expect("the legacy field must parse without affecting weekly_scoped");

    usage.assert();
    assert_eq!(data.five_hour_utilization, 10.0); // sanity: other windows survive
    assert_eq!(data.weekly_scoped_utilization, 0.0);
    assert_eq!(data.weekly_scoped_hours_remaining, 168.0);
    assert!(data.weekly_scoped_model.is_none());
    assert_eq!(data.limits.len(), 1);
}

#[test]
fn weekly_scoped_entry_with_null_percent_is_found_but_zero() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // A weekly_scoped limits[] entry whose percent is null is still found —
    // the model label resolves and the reset carries through — but its
    // utilization degrades to 0.0. Found-but-zero is distinct from absent:
    // the scoped cap exists this period, the API just did not quantify it.
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(
            r#"{
                "limits": [
                    {"kind": "weekly_scoped", "percent": null,
                     "resets_at": "2026-03-20T03:59:59Z",
                     "scope": {"model": {"id": "claude-fable-5", "display_name": "Fable"}}}
                ]
            }"#,
        )
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller
        .poll()
        .expect("a null percent must not fail the poll");

    usage.assert();
    assert_eq!(data.weekly_scoped_utilization, 0.0);
    assert_eq!(data.weekly_scoped_model.as_deref(), Some("Fable"));
    assert_eq!(data.weekly_scoped_resets_at, "2026-03-20T03:59:59Z");
}

#[test]
fn wrong_typed_limits_percent_fails_the_poll() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // The precise boundary of the limits[] additive-tolerance contract:
    // absent and null fields are tolerated (see the two tests above), but a
    // present value of the wrong type is a hard parse failure — serde only
    // applies the field default when the key is missing. If tolerance for
    // wrong-typed entries is ever added, this test and the UsageLimit doc
    // comment claiming "never fails the whole poll" must be updated together.
    let _usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(r#"{"limits": [{"kind": "weekly_scoped", "percent": "high"}]}"#)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let err = poller
        .poll()
        .expect_err("a string percent must fail the poll");
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::ParseError(_))
        ),
        "expected ParseError, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Partial responses (claudego-48fd3fa5)
// ---------------------------------------------------------------------------

#[test]
fn partially_populated_response_parses_present_and_defaults_absent_windows() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // five_hour present; seven_day absent outright (a missing key, not a null
    // one — both must behave the same); no limits[]. The middle case between
    // the fully documented shape and the empty object: present windows parse
    // through, absent ones default to non-binding, and neither contaminates
    // the other.
    let resets_at = (Utc::now() + chrono::Duration::seconds(7200))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(format!(
            r#"{{"five_hour": {{"utilization": 33.0, "resets_at": "{resets_at}"}}}}"#
        ))
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let data = poller
        .poll()
        .expect("a partially populated response must parse");

    usage.assert();
    assert!(!data.stale);
    assert_eq!(data.five_hour_utilization, 33.0);
    assert!(
        (1.9..=2.1).contains(&data.five_hour_hours_remaining),
        "expected ~2h remaining, got {}",
        data.five_hour_hours_remaining
    );

    assert_eq!(data.seven_day_utilization, 0.0);
    assert_eq!(data.seven_day_resets_at, "");
    assert_eq!(data.seven_day_hours_remaining, 168.0);
    assert_eq!(data.weekly_scoped_utilization, 0.0);
    assert_eq!(data.weekly_scoped_hours_remaining, 168.0);
    assert!(data.weekly_scoped_model.is_none());
    assert!(data.limits.is_empty());
}

// ---------------------------------------------------------------------------
// Safe-fallback boundary (claudego-48fd3fa5)
// ---------------------------------------------------------------------------

#[test]
fn usage_endpoint_failure_does_not_fall_back_to_stale_data() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );
    let usage_ok = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    // Poll #1: a good reading to seed the cache.
    let mut poller = contract_poller(&creds, &server.url());
    let fresh = poller.poll().expect("first poll must succeed");
    usage_ok.assert();
    assert!(!fresh.stale);

    // Poll #2: the usage endpoint answers with the documented self-rate-limit
    // response (usage-tracking.md §2/§10). A cached reading exists, but the
    // stale fallback is auth-path only: a fetch failure must propagate rather
    // than be papered over with stale data, and the endpoint is never retried
    // client-side — exactly one request.
    let usage_limited = server
        .mock("GET", "/api/oauth/usage")
        .with_status(429)
        .with_body(
            r#"{"error":{"type":"rate_limit_error","message":"Rate limited. Please try again later."}}"#,
        )
        .expect(1)
        .create();

    let err = poller
        .poll()
        .expect_err("a usage-endpoint failure must propagate even with cached data");
    let msg = err.to_string();
    assert!(msg.contains("429"), "error must surface the status: {msg}");
    usage_limited.assert();
}

// ---------------------------------------------------------------------------
// Timeouts (claudego-0840eab5)
// ---------------------------------------------------------------------------

#[test]
fn hung_usage_endpoint_fails_within_the_request_timeout() {
    let _guard = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );
    let (hung, _listener) = hung_endpoint();

    // The production bound is 30s; the test shortens it to 300ms so the
    // scenario fails in milliseconds. Before the request timeout existed,
    // this call blocked forever — that is the regression being pinned.
    let mut poller = Poller::with_credentials_path(Some(creds))
        .expect("a valid credentials path should build a poller")
        .with_endpoints(hung.clone(), hung)
        .with_refresh_retry_delay(Duration::ZERO)
        .with_request_timeout(Duration::from_millis(300));

    let started = std::time::Instant::now();
    let err = poller
        .poll()
        .expect_err("a hung endpoint must time out, not block forever");
    let elapsed = started.elapsed();

    // A timeout is a transport failure: the same ApiRequestFailed variant a
    // refused connection produces.
    assert!(
        matches!(
            err.downcast_ref::<PollerError>(),
            Some(PollerError::ApiRequestFailed(_))
        ),
        "expected ApiRequestFailed from the timeout, got: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the timeout must fire promptly, took {elapsed:?}"
    );
}

#[test]
fn hung_token_refresh_degrades_to_stale_within_the_request_timeout() {
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(documented_usage_body())
        .create();

    // Poll #1: a good reading to seed the cache.
    let mut poller = contract_poller(&creds, &server.url());
    let fresh = poller.poll().expect("first poll must succeed");
    usage.assert();
    assert!(!fresh.stale);

    // The token then expires and the refresh endpoint black-holes. Both
    // refresh attempts (initial + one retry) must fail within the shortened
    // timeout, and the poll must degrade to the cached reading — the auth
    // path's stale fallback — rather than wedging the cycle on the hang.
    write_credentials(
        dir.path(),
        "expiring-access-token",
        "expiring-refresh-token",
        expiring_soon_expiry_ms(),
    );
    let (hung, _listener) = hung_endpoint();
    poller = poller
        .with_endpoints(server.url(), hung)
        .with_request_timeout(Duration::from_millis(300));

    let started = std::time::Instant::now();
    let stale = poller
        .poll()
        .expect("a hung refresh must degrade to cached data");
    let elapsed = started.elapsed();

    assert!(stale.stale, "the served reading must be marked stale");
    assert_eq!(stale.five_hour_utilization, fresh.five_hour_utilization);
    assert_eq!(stale.timestamp, fresh.timestamp);
    // Two refresh attempts at 300ms each plus overhead; before the request
    // timeout this hung forever on the first attempt.
    assert!(
        elapsed < Duration::from_secs(10),
        "the stale fallback must stay bounded by the timeout, took {elapsed:?}"
    );
}

#[test]
fn slow_response_within_the_timeout_still_succeeds() {
    let _guard = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        "access-token-1",
        "refresh-token-1",
        far_future_expiry_ms(),
    );

    // A response that arrives inside the timeout is a normal success — the
    // bound exists for hangs, not for latency. If the default is ever
    // tightened below real-world latency, this is the test that breaks.
    let slow = slow_endpoint(Duration::from_millis(100), documented_usage_body());

    let mut poller = Poller::with_credentials_path(Some(creds))
        .expect("a valid credentials path should build a poller")
        .with_endpoints(slow, dead_endpoint_url())
        .with_refresh_retry_delay(Duration::ZERO)
        .with_request_timeout(Duration::from_millis(300));
    let data = poller
        .poll()
        .expect("a response inside the timeout must succeed");

    assert!(!data.stale);
    assert_eq!(data.five_hour_utilization, 14.0);
}
