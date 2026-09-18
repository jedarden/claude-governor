//! End-to-end pinning of the GLM quota controller's conservative stale-input
//! posture (claudego-e16a7eb1).
//!
//! The controller (`scripts/needle-glm-quota-controller`, vendored from the
//! deployed copy) must never exit 1 for upstream quota-sample staleness:
//! it degrades to `action=stale-hold-conservative` with exit 0 -- keeps
//! LOCAL_BASE enabled and started, never scales lab up, sheds at most one
//! lab unit per pass, honors MIN_SCALE_INTERVAL_SECS and operator-masked
//! units, and reserves exit 1 for its own local failures (unwritable state).
//!
//! Every test runs the real script against a backdated copy of
//! governor-state.json with `systemctl` / `ssh` / `journalctl` / `cgov`
//! shadowed by recording shims on PATH, so no live systemd unit, no lab
//! host, and no real state file is ever touched (the script's QUOTA_STATE /
//! QUOTA_CONTROLLER_STATE / CGOV_BIN / QUOTA_*_UNITS env overrides are
//! documented for exactly this).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The controller under test, embedded at compile time so the test binary is
/// self-contained. NEEDLE's close gate re-runs tests through the shared
/// /build/target-workers dir, where a binary built in one extraction is
/// reused in another; a runtime `env!("CARGO_MANIFEST_DIR")` lookup would
/// then point at a long-deleted checkout and every test fails with
/// "can't open file '/var/tmp/<deleted>/scripts/...'" (seen 2026-09-18:
/// 0/5 passed through the gate while the same commit was green in-tree).
/// Cargo tracks include_str! files as rebuild inputs, so a controller edit
/// still triggers a fresh build.
const SCRIPT_BODY: &str = include_str!("../scripts/needle-glm-quota-controller");
const SCRIPT_NAME: &str = "needle-glm-quota-controller";

const LOCAL_UNITS: &str = "fake-local-a.service,fake-local-b.service";
const LAB_UNITS: &str = "lab-a.service,lab-b.service,lab-c.service";

/// Scratch dir per test: shim bin dir, recording action log, state files.
struct Sandbox {
    #[allow(dead_code)] // kept alive so the dir outlives the test body
    dir: tempfile::TempDir,
    shim: PathBuf,
    log: PathBuf,
    script: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().expect("tempdir");
        let shim = dir.path().join("shim");
        std::fs::create_dir(&shim).expect("shim dir");
        let log = dir.path().join("actions.log");

        // The vendored controller itself, materialized from the compile-time
        // copy (see SCRIPT_BODY) so no test depends on the source checkout.
        let script = dir.path().join(SCRIPT_NAME);
        write_shim(&script, SCRIPT_BODY);

        // `systemctl`: records every invocation; is-enabled/is-active answer
        // from QUOTA_TEST_MASKED / QUOTA_TEST_ACTIVE (comma-separated lists).
        write_shim(
            &shim.join("systemctl"),
            r#"#!/usr/bin/env bash
printf 'systemctl %s\n' "$*" >> "$QUOTA_TEST_LOG"
[ "$1" = "--user" ] && shift
case "$1" in
  is-enabled) shift
    for u in "$@"; do
      case ",$QUOTA_TEST_MASKED," in *",$u"*) echo masked;; *) echo enabled;; esac
    done ;;
  is-active) shift
    for u in "$@"; do
      case ",$QUOTA_TEST_ACTIVE," in *",$u"*) echo active;; *) echo inactive;; esac
    done ;;
esac
exit 0
"#,
        );
        // `ssh lab ...`: same answers, so lab-side reads never touch the host.
        write_shim(
            &shim.join("ssh"),
            r#"#!/usr/bin/env bash
host="$5"; shift 5
printf 'ssh %s %s\n' "$host" "$*" >> "$QUOTA_TEST_LOG"
case "$1" in
  systemctl)
    shift; [ "${1:-}" = "--user" ] && shift
    case "$1" in
      is-enabled) shift
        for u in "$@"; do
          case ",$QUOTA_TEST_MASKED," in *",$u"*) echo masked;; *) echo enabled;; esac
        done ;;
      is-active) shift
        for u in "$@"; do
          case ",$QUOTA_TEST_ACTIVE," in *",$u"*) echo active;; *) echo inactive;; esac
        done ;;
    esac ;;
esac
exit 0
"#,
        );
        // No matching journal lines -> zero throttles.
        write_shim(&shim.join("journalctl"), "#!/usr/bin/env bash\nexit 0\n");
        // Self-sample always fails -> the controller lands in the
        // conservative hold, which is the behavior under test.
        write_shim(
            &shim.join("cgov"),
            "#!/usr/bin/env bash\nexit 1\n",
        );
        Sandbox { dir, shim, log, script }
    }

    fn write_backdated_state(&self, name: &str, age: chrono::Duration) -> PathBuf {
        let updated_at = (chrono::Utc::now() - age)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let resets_at = (chrono::Utc::now() + chrono::Duration::hours(2))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let body = serde_json::json!({
            "updated_at": updated_at,
            "usage": {"five_hour_pct": 40.0, "five_hour_resets_at": resets_at}
        });
        let path = self.dir.path().join(name);
        std::fs::write(&path, body.to_string()).expect("write quota state");
        path
    }

    fn write_raw_state(&self, name: &str, body: serde_json::Value) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, body.to_string()).expect("write raw state");
        path
    }

    fn controller_state(&self, name: &str, last_scale_at: f64) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(
            &path,
            serde_json::json!({"samples": [], "last_scale_at": last_scale_at}).to_string(),
        )
        .expect("write controller state");
        path
    }

    fn log_contents(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Run the vendored controller with every external effect sandboxed.
    fn run(&self, quota_state: &Path, controller_state: &Path) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        Command::new("python3")
            .arg(&self.script)
            .env("PATH", format!("{}:{}", self.shim.display(), path))
            .env("QUOTA_STATE", quota_state)
            .env("QUOTA_CONTROLLER_STATE", controller_state)
            .env("CGOV_BIN", self.shim.join("cgov"))
            .env("QUOTA_LOCAL_UNITS", LOCAL_UNITS)
            .env("QUOTA_LAB_UNITS", LAB_UNITS)
            .env("QUOTA_TEST_LOG", &self.log)
            .env("QUOTA_TEST_MASKED", "lab-c.service")
            .env("QUOTA_TEST_ACTIVE", "lab-a.service,lab-b.service,lab-c.service")
            .output()
            .expect("run controller; python3 must be available")
    }
}

fn write_shim(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).expect("write shim");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod shim");
}

/// The JSON summary line is the last non-empty stdout line.
fn summary_json(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("");
    serde_json::from_str(last).unwrap_or_else(|e| {
        panic!("stdout does not end in a JSON summary: {e}\nstdout:\n{stdout}")
    })
}

fn controller_state_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read controller state"))
        .expect("controller state is valid JSON")
}

/// Skip cleanly where python3 is absent so the suite still passes in image
///-restricted environments; on this host (and NEEDLE's close gate) it runs.
fn python3_available() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

fn stale_two_hours() -> chrono::Duration {
    chrono::Duration::hours(2)
}

#[test]
fn stale_sample_holds_conservatively_and_sheds_one_unmasked_lab_unit() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let sb = Sandbox::new();
    let quota = sb.write_backdated_state("quota.json", stale_two_hours());
    let cstate = sb.controller_state("cstate.json", 0.0); // never scaled

    let output = sb.run(&quota, &cstate);
    assert!(output.status.success(), "stale input must exit 0");

    let summary = summary_json(&output);
    assert_eq!(summary["action"], "stale-hold-conservative");
    let age = summary["quota_sample_age_secs"].as_f64().expect("age present");
    assert!(
        (7100.0..=7400.0).contains(&age),
        "sample age should reflect the ~2h-old backdated sample, got {age}"
    );

    let state = controller_state_json(&cstate);
    assert_eq!(state["action"], "stale-hold-conservative");
    let saved_age = state["quota_sample_age_secs"].as_f64().expect("age saved to state");
    assert!(
        (7100.0..=7400.0).contains(&saved_age),
        "state file should carry the ~2h sample age, got {saved_age}"
    );
    assert_eq!(state["lab_workers"], 1, "3 active, 1 masked, 1 shed -> 1 left");

    let log = sb.log_contents();
    // LOCAL_BASE enabled and started, masked-unit check applied first.
    assert!(log.contains("systemctl --user enable fake-local-a.service fake-local-b.service"));
    assert!(log.contains("systemctl --user start --no-block fake-local-a.service fake-local-b.service"));
    // Exactly one shed, and it is the last active UNMASKED unit (lab-b):
    // lab-c is masked and must never be touched; lab-a must survive.
    assert!(log.contains("ssh lab systemctl --user disable lab-b.service"));
    assert!(log.contains("ssh lab systemctl --user stop --no-block lab-b.service"));
    for line in log.lines() {
        assert!(
            !line.contains("disable") || !line.contains("lab-c.service"),
            "masked unit must not be disabled: {line}"
        );
        assert!(
            !line.contains("disable") || !line.contains("lab-a.service"),
            "only the last active unit sheds, not lab-a: {line}"
        );
    }
    // No lab scale-up of any kind in the conservative posture.
    for line in log.lines() {
        assert!(
            !(line.starts_with("ssh lab") && (line.contains(" enable ") || line.contains(" start "))),
            "conservative hold must not scale lab up: {line}"
        );
    }
}

#[test]
fn min_scale_interval_blocks_the_conservative_shed() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let sb = Sandbox::new();
    let quota = sb.write_backdated_state("quota.json", stale_two_hours());
    let cstate = sb.controller_state(
        "cstate.json",
        (chrono::Utc::now() - chrono::Duration::seconds(60)).timestamp() as f64,
    );

    let output = sb.run(&quota, &cstate);
    assert!(output.status.success());

    let summary = summary_json(&output);
    assert_eq!(summary["action"], "stale-hold-conservative");

    let log = sb.log_contents();
    assert!(
        !log.lines().any(|l| l.contains("disable") || l.contains(" stop")),
        "shed must honor MIN_SCALE_INTERVAL_SECS: {}",
        log
    );
    let state = controller_state_json(&cstate);
    assert_eq!(state["lab_workers"], 2, "masked unit excluded, nothing shed");
}

#[test]
fn missing_state_file_degrades_with_null_sample_age() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let sb = Sandbox::new();
    let quota = sb.dir.path().join("absent.json"); // never created
    let cstate = sb.controller_state("cstate.json", 0.0);

    let output = sb.run(&quota, &cstate);
    assert!(output.status.success(), "missing input must exit 0");

    let summary = summary_json(&output);
    assert_eq!(summary["action"], "stale-hold-conservative");
    assert!(
        summary["quota_sample_age_secs"].is_null(),
        "an unreadable sample has no age"
    );
}

#[test]
fn fresh_sample_missing_usage_subtree_degrades_instead_of_exit_1() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let sb = Sandbox::new();
    // Fresh updated_at but no `usage` key at all: structurally unusable
    // upstream data. This used to raise KeyError out of main (exit 1) --
    // the regression this test pins.
    let quota = sb.write_raw_state(
        "quota.json",
        serde_json::json!({
            "updated_at": (chrono::Utc::now() - chrono::Duration::seconds(90))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        }),
    );
    let cstate = sb.controller_state("cstate.json", 0.0);

    let output = sb.run(&quota, &cstate);
    assert!(
        output.status.success(),
        "malformed upstream sample is not a local error; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(summary_json(&output)["action"], "stale-hold-conservative");
}

#[test]
fn unwritable_controller_state_is_still_a_local_error_exit_1() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let sb = Sandbox::new();
    let quota = sb.write_backdated_state("quota.json", stale_two_hours());

    // A read-only directory makes the controller unable to persist its own
    // state -- the one failure class allowed to exit 1.
    let ro = sb.dir.path().join("readonly");
    std::fs::create_dir(&ro).expect("mkdir");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).expect("chmod ro");
    let cstate = ro.join("state.json");

    let path = std::env::var("PATH").unwrap_or_default();
    let output = Command::new("python3")
        .arg(&sb.script)
        .env("PATH", format!("{}:{}", sb.shim.display(), path))
        .env("QUOTA_STATE", &quota)
        .env("QUOTA_CONTROLLER_STATE", &cstate)
        .env("CGOV_BIN", sb.shim.join("cgov"))
        .env("QUOTA_LOCAL_UNITS", LOCAL_UNITS)
        .env("QUOTA_LAB_UNITS", LAB_UNITS)
        .env("QUOTA_TEST_LOG", &sb.log)
        .env("QUOTA_TEST_MASKED", "")
        .env("QUOTA_TEST_ACTIVE", "")
        .output()
        .expect("run controller");

    let _ = std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755));

    assert_eq!(
        output.status.code(),
        Some(1),
        "unwritable own state is a genuine local error"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).trim().is_empty(),
        "the local error must be reported on stderr"
    );
}
