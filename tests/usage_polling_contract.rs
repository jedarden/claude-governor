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
