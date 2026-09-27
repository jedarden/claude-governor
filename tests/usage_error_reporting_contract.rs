//! Contract tests for credential-safe error reporting on the
//! `/api/oauth/usage` polling path (`src/poller.rs`).
//!
//! The usage polling contract (tests/usage_polling_contract.rs) pins six of
//! the seven documented axes: authentication headers and token refresh,
//! 429/5xx rate-limit handling, request timeouts, malformed payloads,
//! `resets_at` timestamp parsing, and multi-window responses. The seventh —
//! safe error reporting without exposing credentials — is pinned here
//! (claudego-18068347).
//!
//! Every poll error is logged verbatim by the governor ("poll failed, keeping
//! previous usage data: {e}") and refresh failures are logged per attempt, so
//! an error string is an operator-visible surface, not just a return value.
//! The safety property pinned here has two halves:
//!
//! - **No response-body text in error strings.** ureq renders a non-2xx
//!   response as `Error::Status` whose Display carries the URL and the status
//!   code and nothing else, and the poller never reads an error body into a
//!   message — so even an endpoint or middlebox that *echoes* the token it
//!   was just sent cannot leak it through this surface. The echo fixtures
//!   below lock that property in: if a future change starts embedding
//!   response bodies into poll errors, these tests fire before a token can
//!   reach governor.log or an alert bead.
//! - **No credential values in error strings or logs.** A 401, a transport
//!   failure, and a malformed or corrupted credentials file all produce
//!   errors free of the credential values, and the refresh path's
//!   per-attempt warn logs — where its error text actually reaches the
//!   operator — stay free of the refresh token.
//!
//! A local mockito server stands in for the two endpoints. The test that
//! drives a refresh serializes on one lock because the refresh-failure
//! counter in `poller.rs` is process-global state, and this test binary owns
//! the process-global logger for log capture.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use claude_governor::poller::Poller;

/// The refresh-failure counter in poller.rs is `static mut` shared by every
/// Poller in the process — tests that trigger a refresh path hold this lock.
static REFRESH_COUNTER_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    REFRESH_COUNTER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// --- log capture: this test binary owns the process-global logger ---

static TEST_LOGS: std::sync::OnceLock<Mutex<Vec<String>>> = std::sync::OnceLock::new();

struct TestLogger;

impl log::Log for TestLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record) {
        TEST_LOGS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push(format!("[{}] {}", record.level(), record.args()));
    }
    fn flush(&self) {}
}

static TEST_LOGGER: TestLogger = TestLogger;

fn init_logger() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        log::set_logger(&TEST_LOGGER).expect("this binary owns the global logger");
        log::set_max_level(log::LevelFilter::Warn);
    });
}

fn captured_logs() -> Vec<String> {
    TEST_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .clone()
}

fn logs_containing(pattern: &str) -> Vec<String> {
    captured_logs()
        .into_iter()
        .filter(|line| line.contains(pattern))
        .collect()
}

// --- fixtures ---

/// Tokens distinctive enough that any unredacted occurrence in an error
/// string is a real leak, not a coincidental substring.
const ACCESS_TOKEN: &str = "access-secret-7Q9xZzTOKEN-value";
const REFRESH_TOKEN: &str = "refresh-secret-3Kk8YyTOKEN-value";

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

/// 1h out: comfortably outside the 5-minute refresh threshold, so a poll
/// never touches the refresh path.
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
        // The production 5s retry delay would sleep real wall-clock time.
        .with_refresh_retry_delay(Duration::ZERO)
}

// --- the usage path: surfaced errors carry no credential material ---

#[test]
fn unauthorized_401_error_carries_no_access_token() {
    init_logger();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        ACCESS_TOKEN,
        REFRESH_TOKEN,
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

    // The diagnostic content survives...
    assert!(
        msg.contains("401"),
        "error must keep the status line for the operator: {msg}"
    );
    // ...and none of the credential material does.
    assert!(
        !msg.contains(ACCESS_TOKEN),
        "401 error must not carry the access token: {msg}"
    );
    assert!(
        !msg.contains(REFRESH_TOKEN),
        "401 error must not carry the refresh token: {msg}"
    );
    usage.assert();
}

#[test]
fn usage_error_response_body_never_enters_the_error_string() {
    init_logger();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        ACCESS_TOKEN,
        REFRESH_TOKEN,
        far_future_expiry_ms(),
    );

    // A hostile or broken endpoint — or a middlebox echoing the request —
    // reflects the Authorization value back in the error body, alongside a
    // non-secret marker proving the *body* is what must stay out.
    let body = format!(
        r#"{{"error":{{"message":"ECHO-BODY-MARKER bad request for bearer {} ({})"}}}}"#,
        ACCESS_TOKEN, ACCESS_TOKEN
    );
    let usage = server
        .mock("GET", "/api/oauth/usage")
        .with_status(500)
        .with_body(body)
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let err = poller.poll().expect_err("500 must fail the poll");
    let msg = err.to_string();

    // The diagnostic content survives: the status line does reach the
    // operator.
    assert!(
        msg.contains("500"),
        "error must keep the status line for the operator: {msg}"
    );
    // ...but no part of the response body does — not the echoed token, not
    // any other body text.
    assert!(
        !msg.contains("ECHO-BODY-MARKER"),
        "error must not embed the response body text: {msg}"
    );
    assert!(
        !msg.contains(ACCESS_TOKEN),
        "error body echo must not leak the access token: {msg}"
    );
    usage.assert();
}

#[test]
fn usage_transport_error_carries_no_access_token() {
    init_logger();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        ACCESS_TOKEN,
        REFRESH_TOKEN,
        far_future_expiry_ms(),
    );

    // Port 1 on localhost is reserved and refuses connections.
    let mut poller = contract_poller(&creds, "http://127.0.0.1:1");
    let err = poller
        .poll()
        .expect_err("an unreachable endpoint must fail the poll");
    let msg = err.to_string();

    assert!(
        !msg.contains(ACCESS_TOKEN),
        "transport error must not carry the access token: {msg}"
    );
    assert!(
        !msg.contains(REFRESH_TOKEN),
        "transport error must not carry the refresh token: {msg}"
    );
}

// --- the refresh path: per-attempt errors reach the operator via logs ---

#[test]
fn refresh_failure_error_and_logs_never_carry_the_refresh_token() {
    init_logger();
    let _guard = lock();
    let mut server = mockito::Server::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = write_credentials(
        dir.path(),
        ACCESS_TOKEN,
        REFRESH_TOKEN,
        expiring_soon_expiry_ms(),
    );

    // The refresh endpoint sees the token in the request body; a broken one
    // echoing it back in the 400 body — alongside a non-secret body marker —
    // must not leak either into the per-attempt warn logs (where the refresh
    // error actually reaches the operator) or the surfaced error.
    let body = format!(
        r#"{{"error":"ECHO-BODY-MARKER invalid grant: {}"}}"#,
        REFRESH_TOKEN
    );
    let refresh = server
        .mock("POST", "/v1/oauth/token")
        .with_status(400)
        .with_body(body)
        .expect(2) // initial attempt + the one documented retry
        .create();

    let mut poller = contract_poller(&creds, &server.url());
    let err = poller
        .poll()
        .expect_err("a failed refresh with no cached reading must fail the poll");
    refresh.assert();

    let attempt_logs = logs_containing("refresh attempt 1 failed");
    assert!(
        !attempt_logs.is_empty(),
        "the failed attempt must be warn-logged for the operator"
    );
    for line in captured_logs() {
        assert!(
            !line.contains(REFRESH_TOKEN),
            "log line must not carry the refresh token: {line}"
        );
        assert!(
            !line.contains("ECHO-BODY-MARKER"),
            "log line must not embed the response body text: {line}"
        );
    }
    let msg = err.to_string();
    assert!(
        !msg.contains(REFRESH_TOKEN) && !msg.contains("ECHO-BODY-MARKER"),
        "surfaced error must not carry the refresh token or the body: {msg}"
    );
}

// --- the credentials file: parse/corruption errors quote no secret values ---

#[test]
fn malformed_credentials_error_carries_no_token_material() {
    init_logger();
    let dir = tempfile::tempdir().expect("tempdir");

    // Truncated mid-token: the file's secret value sits on disk, and the
    // parse error must report the failure without quoting any of it.
    let raw = format!(r#"{{"claudeAiOauth": {{"accessToken": "{}","#, ACCESS_TOKEN);
    let path = dir.path().join(".credentials.json");
    std::fs::write(&path, raw).expect("write raw credentials file");

    let mut poller = Poller::with_credentials_path(Some(path.to_string_lossy().into_owned()))
        .expect("a valid credentials path should build a poller");
    let err = poller
        .poll()
        .expect_err("a malformed credentials file must fail the poll");
    let msg = err.to_string();

    assert!(
        !msg.contains(ACCESS_TOKEN),
        "credentials parse error must not quote the file's token material: {msg}"
    );
}

#[test]
fn corrupted_credentials_error_names_only_the_file() {
    init_logger();
    let dir = tempfile::tempdir().expect("tempdir");
    let creds_path = write_credentials(dir.path(), "", REFRESH_TOKEN, far_future_expiry_ms());

    let mut poller = contract_poller(&creds_path, "http://127.0.0.1:1");
    let err = poller
        .poll()
        .expect_err("an empty access token must fail the poll before any network call");
    let msg = err.to_string();

    assert!(
        msg.contains("corrupted") && msg.contains(".credentials.json"),
        "corruption guard must name the defect and the file: {msg}"
    );
    assert!(
        !msg.contains(REFRESH_TOKEN),
        "corruption error must not carry the file's other token values: {msg}"
    );
}
