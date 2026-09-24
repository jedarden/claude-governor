//! Release-verification contract for `install.sh` (claudego-4531120c) — the
//! installer README.md stakes its provenance story on: "The installer
//! downloads the binary plus its published `.sha256` sidecar and refuses to
//! install on any mismatch", "a bare `0.1.1` is normalized to `v0.1.1`", "A
//! non-matching `CGOV_SHA256` aborts the install before anything is written",
//! and "mirror lag can never swap contents under a pinned version".
//!
//! The tests execute the shipped script against a local mockito release
//! server and a throwaway install dir, asserting:
//!
//! * happy path (latest release, published sidecar) installs a mode-0755
//!   binary whose bytes equal the artifact and which actually runs;
//! * a tampered sidecar → refusal, and the install dir is never created;
//! * a wrong `CGOV_SHA256` → abort, and the install dir is never created;
//! * a matching `CGOV_SHA256` → installs without fetching any sidecar;
//! * a malformed digest pin → rejected before any download;
//! * `CGOV_VERSION=0.1.1` downloads from the `v0.1.1` tag URL.
//!
//! The script is embedded with `include_str!` at compile time, following the
//! repo's gate pattern (tests/adapter_var_sync.rs): the tested text cannot
//! drift from the shipped one, cargo tracks the file as a rebuild input, and
//! nothing is read from `CARGO_MANIFEST_DIR` at run time — which matters
//! because the close gate reuses test binaries across `git archive`
//! extractions whose checkouts are deleted (env!(...) runtime reads ENOENT
//! there). The only sandbox substitution is the `RELEASES_BASE` assignment,
//! asserted line-equal to the shipped literal before being redirected at the
//! mock server; every other byte of the executed script is the committed one.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mockito::Mock;
use tempfile::TempDir;

/// The shipped installer, embedded at compile time (see module docs).
const INSTALL_SH: &str = include_str!("../install.sh");

/// The release artifact install.sh would download on this host — `uname -s`
/// must be Linux (the script refuses anything else) and the arch maps exactly
/// the way the script's own case statement maps it.
fn release_artifact_name() -> String {
    let os = String::from_utf8_lossy(&Command::new("uname").arg("-s").output().expect("uname -s").stdout)
        .trim()
        .to_string();
    assert!(
        os.starts_with("Linux"),
        "install.sh only installs on Linux; this host reports {os}"
    );
    let arch = String::from_utf8_lossy(&Command::new("uname").arg("-m").output().expect("uname -m").stdout)
        .trim()
        .to_string();
    match arch.as_str() {
        "x86_64" | "amd64" => "cgov-linux-amd64".to_string(),
        "aarch64" | "arm64" => "cgov-linux-arm64".to_string(),
        other => panic!(
            "install.sh would refuse this host's architecture ({other}); \
             no installer contract to exercise here"
        ),
    }
}

/// Stand-in release artifact: a tiny runnable script, so the happy path can
/// also prove the installed file executes (the installer runs
/// `"$BINARY_PATH" --version` best-effort after install).
fn artifact_bytes() -> Vec<u8> {
    b"#!/bin/sh\necho \"cgov 0.0.0-sandbox\"\n".to_vec()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sha256sum (coreutils, required by install.sh itself)");
    child
        .stdin
        .take()
        .expect("sha256sum stdin")
        .write_all(bytes)
        .expect("feed sha256sum");
    let out = child.wait_with_output().expect("wait for sha256sum");
    assert!(out.status.success(), "sha256sum exited non-zero");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum prints a digest")
        .to_string()
}

/// Materialize the embedded installer into the sandbox with its single
/// substitution applied: `RELEASES_BASE` redirected at the mock server. The
/// substituted line must still be byte-identical to the shipped assignment —
/// if install.sh ever reshapes that line, the test fails here instead of
/// silently testing a rewrite the shipped script no longer contains.
fn materialize_installer(dir: &Path, server_base: &str) -> PathBuf {
    const SHIPPED_LINE: &str = r#"RELEASES_BASE="https://github.com/${REPO}/releases""#;
    let mut substitutions = 0;
    let rewritten: Vec<String> = INSTALL_SH
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("RELEASES_BASE=") {
                assert_eq!(
                    line, SHIPPED_LINE,
                    "install.sh's RELEASES_BASE assignment drifted; update the \
                     sandbox substitution in this test module"
                );
                substitutions += 1;
                format!(r#"RELEASES_BASE="{server_base}/releases""#)
            } else {
                line.to_string()
            }
        })
        .collect();
    assert_eq!(
        substitutions, 1,
        "expected exactly one RELEASES_BASE assignment in install.sh"
    );
    let mut text = rewritten.join("\n");
    text.push('\n');
    let path = dir.join("install.sh");
    fs::write(&path, text).expect("write sandboxed install.sh");
    path
}

/// Publish the artifact (and optionally its sidecar) on the mock release
/// server under `path_prefix` — `/releases/latest/download` for the latest
/// branch, `/releases/download/<tag>` for a pinned one.
fn serve_release(
    server: &mut mockito::Server,
    path_prefix: &str,
    artifact: &str,
    bytes: &[u8],
    sidecar: Option<Vec<u8>>,
) -> (Mock, Option<Mock>) {
    let artifact_mock = server
        .mock("GET", format!("{path_prefix}/{artifact}").as_str())
        .with_status(200)
        .with_body(bytes.to_vec())
        .create();
    let sidecar_mock = sidecar.map(|body| {
        server
            .mock("GET", format!("{path_prefix}/{artifact}.sha256").as_str())
            .with_status(200)
            .with_body(body)
            .create()
    });
    (artifact_mock, sidecar_mock)
}

/// Sidecar in the published format `sha256sum -c` consumes: `<digest>␠␠<name>`.
fn sidecar_bytes(digest: &str, artifact: &str) -> Vec<u8> {
    format!("{digest}  {artifact}\n").into_bytes()
}

/// Assert a route published via `serve_release` was served exactly once. A
/// named helper because `Option::expect` here is easy to misread as mockito's
/// builder `.expect(hits)`, which means something entirely different.
fn assert_hit_once(mock: Option<Mock>) {
    mock.expect("mock route must be registered on the sandbox server")
        .assert();
}

/// Run the sandboxed installer with a hermetic environment: HOME pointed at
/// the sandbox (so no real dotfile is consulted), CI=true (the installer's
/// interactive `cgov init` branch is for terminals only, and CI=true keeps it
/// off even if a future check widens), and all three CGOV_ variables set
/// explicitly — empty means "unset" to the script's `${VAR:-}` reads, so
/// inherited values from the worker environment cannot leak into a case.
fn run_installer(
    script: &Path,
    home: &Path,
    install_dir: &Path,
    extra_env: &[(&str, &str)],
) -> (bool, String) {
    let mut cmd = Command::new("bash");
    cmd.arg(script)
        .env("HOME", home)
        .env("CI", "true")
        .env("CGOV_INSTALL_DIR", install_dir)
        .env("CGOV_VERSION", "")
        .env("CGOV_SHA256", "");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn bash on the sandboxed install.sh");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// A fresh, deliberately non-existent install dir: after every refusal path
/// the test asserts it still does not exist, which is the strongest form of
/// "nothing was written" — the installer's `mkdir -p` itself must not have run.
fn fresh_install_dir(sandbox: &TempDir) -> PathBuf {
    sandbox.path().join("bin")
}

/// Sandbox HOME — created so the child bash never resolves dotfiles against
/// the worker's real home.
fn sandbox_home(sandbox: &TempDir) -> PathBuf {
    let home = sandbox.path().join("home");
    fs::create_dir_all(&home).expect("create sandbox HOME");
    home
}

fn assert_installed_binary(install_dir: &Path, artifact: &[u8]) {
    let installed = install_dir.join("cgov");
    let meta = fs::metadata(&installed)
        .unwrap_or_else(|e| panic!("expected {} installed: {}", installed.display(), e));
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o755,
        "installer must install with mode 0755 (install -m 0755)"
    );
    assert_eq!(
        fs::read(&installed).expect("read installed binary"),
        artifact,
        "installed bytes differ from the verified artifact"
    );
    let version = Command::new(&installed)
        .arg("--version")
        .output()
        .expect("run installed binary");
    assert!(
        version.status.success(),
        "installed binary did not execute; mode/shebang broken"
    );
    assert!(
        String::from_utf8_lossy(&version.stdout).contains("cgov 0.0.0-sandbox"),
        "installed binary produced wrong --version output"
    );
}

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

#[test]
fn happy_path_installs_digest_verified_binary_mode_0755() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let bytes = artifact_bytes();
    let digest = sha256_hex(&bytes);

    let mut server = mockito::Server::new();
    let (artifact_mock, sidecar_mock) = serve_release(
        &mut server,
        "/releases/latest/download",
        &artifact,
        &bytes,
        Some(sidecar_bytes(&digest, &artifact)),
    );

    let script = materialize_installer(sandbox.path(), &server.url());
    let install_dir = fresh_install_dir(&sandbox);
    let (ok, out) = run_installer(&script, &sandbox_home(&sandbox), &install_dir, &[]);
    assert!(
        ok,
        "happy-path install failed:\n{out}\n(server at {})",
        server.url()
    );
    artifact_mock.assert();
    assert_hit_once(sidecar_mock);
    assert!(
        out.contains("Checksum OK (published sidecar)"),
        "output must credit the published sidecar:\n{out}"
    );
    assert_installed_binary(&install_dir, &bytes);
}

#[test]
fn tampered_sidecar_refuses_and_writes_nothing() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let bytes = artifact_bytes();

    let mut server = mockito::Server::new();
    let (artifact_mock, sidecar_mock) = serve_release(
        &mut server,
        "/releases/latest/download",
        &artifact,
        &bytes,
        // The mirror's nightmare scenario: binary real, sidecar swapped.
        Some(sidecar_bytes(&sha256_hex(b"tampered payload"), &artifact)),
    );

    let script = materialize_installer(sandbox.path(), &server.url());
    let install_dir = fresh_install_dir(&sandbox);
    let (ok, out) = run_installer(&script, &sandbox_home(&sandbox), &install_dir, &[]);
    assert!(!ok, "a tampered sidecar must fail the install:\n{out}");
    assert!(
        out.contains("Checksum verification FAILED"),
        "refusal must name the checksum failure:\n{out}"
    );
    assert!(
        !install_dir.exists(),
        "refused install must not create the install dir"
    );
    artifact_mock.assert();
    assert_hit_once(sidecar_mock);
}

#[test]
fn wrong_pinned_digest_aborts_before_any_write() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let bytes = artifact_bytes();

    let mut server = mockito::Server::new();
    let (artifact_mock, _never_sidecar) = serve_release(
        &mut server,
        "/releases/download/v0.1.1",
        &artifact,
        &bytes,
        // The pinned-digest branch must never consult a sidecar; publish a
        // 404 and assert zero hits below.
        None,
    );
    let unused_sidecar = server
        .mock("GET", format!("/releases/download/v0.1.1/{artifact}.sha256").as_str())
        .with_status(404)
        .expect(0) // assert() below enforces: never fetched
        .create();

    // Well-formed 64-hex, deliberately not the artifact's digest.
    let wrong_digest = format!("{}1", "0".repeat(63));
    let script = materialize_installer(sandbox.path(), &server.url());
    let install_dir = fresh_install_dir(&sandbox);
    let (ok, out) = run_installer(
        &script,
        &sandbox_home(&sandbox),
        &install_dir,
        &[("CGOV_VERSION", "v0.1.1"), ("CGOV_SHA256", &wrong_digest)],
    );
    assert!(!ok, "a wrong CGOV_SHA256 must abort the install:\n{out}");
    assert!(
        out.contains("Checksum MISMATCH"),
        "abort must name the digest mismatch:\n{out}"
    );
    assert!(
        !install_dir.exists(),
        "abort must happen before anything is written"
    );
    artifact_mock.assert();
    unused_sidecar.assert();
}

#[test]
fn matching_pinned_digest_installs_without_sidecar() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let bytes = artifact_bytes();
    let digest = sha256_hex(&bytes);

    let mut server = mockito::Server::new();
    let (artifact_mock, _unused) = serve_release(
        &mut server,
        "/releases/download/v0.1.1",
        &artifact,
        &bytes,
        None,
    );
    let unused_sidecar = server
        .mock("GET", format!("/releases/download/v0.1.1/{artifact}.sha256").as_str())
        .with_status(404)
        .expect(0) // assert() below enforces: never fetched
        .create();

    let script = materialize_installer(sandbox.path(), &server.url());
    let install_dir = fresh_install_dir(&sandbox);
    let (ok, out) = run_installer(
        &script,
        &sandbox_home(&sandbox),
        &install_dir,
        &[("CGOV_VERSION", "v0.1.1"), ("CGOV_SHA256", &digest)],
    );
    assert!(ok, "matching digest pin must install:\n{out}");
    assert!(
        out.contains("Checksum OK (caller-supplied digest)"),
        "output must credit the caller-supplied digest:\n{out}"
    );
    artifact_mock.assert();
    unused_sidecar.assert();
    assert_installed_binary(&install_dir, &bytes);
}

#[test]
fn malformed_digest_pin_is_rejected_before_any_download() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let bytes = artifact_bytes();

    let mut server = mockito::Server::new();
    // The validation must run before any download: publish the artifact with
    // an expected-hit count of zero, so assert() fails if it was fetched.
    let artifact_mock = server
        .mock("GET", format!("/releases/download/v0.1.1/{artifact}").as_str())
        .with_status(200)
        .with_body(bytes.clone())
        .expect(0)
        .create();

    let script = materialize_installer(sandbox.path(), &server.url());
    let install_dir = fresh_install_dir(&sandbox);
    let (ok, out) = run_installer(
        &script,
        &sandbox_home(&sandbox),
        &install_dir,
        &[("CGOV_VERSION", "v0.1.1"), ("CGOV_SHA256", "deadbeef")],
    );
    assert!(!ok, "a malformed digest must be rejected:\n{out}");
    assert!(
        out.contains("64-character hex sha256 digest"),
        "rejection must explain the digest shape:\n{out}"
    );
    assert!(
        !install_dir.exists(),
        "rejection must happen before anything is written"
    );
    artifact_mock.assert();
}

#[test]
fn bare_version_is_normalized_to_the_release_tag() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let bytes = artifact_bytes();
    let digest = sha256_hex(&bytes);

    let mut server = mockito::Server::new();
    // Only the normalized v-prefixed tag is published. If the installer used
    // the bare 0.1.1 URL, the download 501s (no mock) and the run fails.
    let (artifact_mock, sidecar_mock) = serve_release(
        &mut server,
        "/releases/download/v0.1.1",
        &artifact,
        &bytes,
        Some(sidecar_bytes(&digest, &artifact)),
    );

    let script = materialize_installer(sandbox.path(), &server.url());
    let install_dir = fresh_install_dir(&sandbox);
    let (ok, out) = run_installer(
        &script,
        &sandbox_home(&sandbox),
        &install_dir,
        &[("CGOV_VERSION", "0.1.1")],
    );
    assert!(ok, "bare CGOV_VERSION must normalize to v0.1.1:\n{out}");
    artifact_mock.assert();
    assert_hit_once(sidecar_mock);
    assert!(
        out.contains("Release:     v0.1.1"),
        "installer must report the normalized tag:\n{out}"
    );
    assert_installed_binary(&install_dir, &bytes);
}
