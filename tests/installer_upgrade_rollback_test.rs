//! Upgrade and rollback contract for `install.sh` (claudego-765c10d9) —
//! the behavior `docs/notes/installer-upgrade-and-rollback.md` specifies:
//! re-running the installer is the upgrade, it replaces exactly one file,
//! everything else survives, a failed or interrupted attempt leaves the
//! prior version installed and working, and rollback is the same operation
//! pinned to an older, digest-verified release.
//!
//! The refusal paths' pre-write guarantee (nothing written on checksum,
//! sidecar, and network failures) is already pinned in
//! `tests/install_sh_release_verification.rs`; what that suite cannot see is
//! the *lifecycle* around them:
//!
//! * **upgrade over a working install** — a pinned v0.1.2 install over an
//!   existing v0.1.1 replaces only `cgov` (mode 0755, exact verified bytes,
//!   the new binary actually executes) and leaves every other file in the
//!   install dir *and* the whole sandbox HOME — `governor.yaml`,
//!   `governor-state.json`, an unrelated app's config — byte- and
//!   mode-identical;
//! * **interrupted upgrade** — the release server accepts the connection,
//!   sends a partial body, and drops it mid-transfer. curl fails, the
//!   installer aborts with `Download failed`, the install dir is
//!   byte-identical, and the *prior version still executes* — the property
//!   that makes a failed upgrade safe to retry from;
//! * **failed execution probe** — an artifact whose bytes match its
//!   published sidecar but which cannot execute (truncated-ELF garbage) DOES
//!   install: verification gates the bytes, not the behavior, and the
//!   post-install `--version` probe is best-effort by design — the run
//!   exits 0, reports `(version check failed)`, and the (broken) upgrade
//!   takes effect;
//! * **rollback** — from that broken install, re-running pinned to v0.1.1
//!   *with its caller-supplied digest* restores a working binary; the
//!   pinned branch must not consult the sidecar at all (the mirror cannot
//!   influence a digest-pinned rollback), and config/state again survive.
//!
//! Sandbox is the installer-suite design: `include_str!`-embedded script
//! (close-gate extraction safe), the single `RELEASES_BASE` substitution
//! asserted line-equal to the shipped literal, mockito for well-formed
//! releases, a raw `TcpListener` for the truncated transfer (mockito cannot
//! sever a connection mid-body), and shell-script stand-ins as artifacts so
//! "executes" is checkable natively. Nothing here reaches the network.

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mockito::Mock;
use tempfile::TempDir;

/// The shipped installer, embedded at compile time (see module docs).
const INSTALL_SH: &str = include_str!("../install.sh");

// ---------------------------------------------------------------------------
// Artifact fixtures
// ---------------------------------------------------------------------------

/// A runnable stand-in artifact for release `vX.Y.Z-sandbox`: prints its
/// version on `--version` so "the installed binary works" is checkable
/// natively without an ELF toolchain.
fn runnable_artifact(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"cgov {version}-sandbox\"\n").into_bytes()
}

/// Digest-valid but non-executable: real ELF magic, then garbage. The kernel
/// rejects it (bad ELF headers → ENOEXEC), and the NUL bytes make bash refuse
/// its own script fallback ("cannot execute binary file") — so the
/// installer's post-install `--version` probe fails deterministically on any
/// host, which is the shape of a release cut from a broken build yet shipped
/// with a matching sidecar.
fn non_executable_artifact() -> Vec<u8> {
    let mut b = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
    b.extend_from_slice(&[0u8; 120]);
    b.extend_from_slice(b"truncated build output, not a program");
    b
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

/// Sidecar in the published format `sha256sum -c` consumes: `<digest>␠␠<name>`.
fn sidecar_bytes(digest: &str, artifact: &str) -> Vec<u8> {
    format!("{digest}  {artifact}\n").into_bytes()
}

/// The artifact install.sh downloads on this host — `uname -s` must be Linux
/// (the script refuses anything else) and the arch maps exactly the way the
/// script's own case statement maps it.
fn release_artifact_name() -> String {
    let os = String::from_utf8_lossy(
        &Command::new("uname").arg("-s").output().expect("uname -s").stdout,
    )
    .trim()
    .to_string();
    assert!(
        os.starts_with("Linux"),
        "install.sh only installs on Linux; this host reports {os}"
    );
    let arch = String::from_utf8_lossy(
        &Command::new("uname").arg("-m").output().expect("uname -m").stdout,
    )
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

// ---------------------------------------------------------------------------
// Sandbox: embedded installer + mock release server + hermetic run
// ---------------------------------------------------------------------------

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

/// Publish the artifact and its matching sidecar under the pinned-tag
/// download path install.sh builds from `CGOV_VERSION={tag}`:
/// `<base>/releases/download/{tag}/<asset>`.
fn serve_release(
    server: &mut mockito::Server,
    tag: &str,
    artifact: &str,
    bytes: &[u8],
) -> (Mock, Mock) {
    let prefix = format!("/releases/download/{tag}");
    let artifact_mock = server
        .mock("GET", format!("{prefix}/{artifact}").as_str())
        .with_status(200)
        .with_body(bytes.to_vec())
        .create();
    let digest = sha256_hex(bytes);
    let sidecar_mock = server
        .mock("GET", format!("{prefix}/{artifact}.sha256").as_str())
        .with_status(200)
        .with_body(sidecar_bytes(&digest, artifact))
        .create();
    (artifact_mock, sidecar_mock)
}

/// Run the sandboxed installer with a hermetic environment: HOME pointed at
/// the sandbox (so no real dotfile is consulted), CI=true (the installer's
/// interactive `cgov init` branch is for terminals only), and the three
/// CGOV_ variables set explicitly — empty means "unset" to the script's
/// `${VAR:-}` reads, so inherited values from the worker environment cannot
/// leak into a case. Every case in this module pins a tag, which also keeps
/// the installer off its best-effort latest-release redirect probe.
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

/// Sandbox HOME with a realistic config/state tree an upgrade must preserve:
/// the seeded governor config, the daemon-written state file, and an
/// unrelated app's directory (a blanket `.config` clobber would take it too).
/// Returns the home path and the full before-snapshot to compare against.
fn seeded_home(sandbox: &TempDir) -> (PathBuf, Vec<(String, u32, Vec<u8>)>) {
    let home = sandbox.path().join("home");
    let cfg = home.join(".config/claude-governor");
    fs::create_dir_all(&cfg).expect("create config dir");
    fs::write(
        cfg.join("governor.yaml"),
        b"agents:\n  claude-print-opus:\n    min_workers: 1\npricing:\n  opus: 15.0\n",
    )
    .expect("seed governor.yaml");
    fs::write(
        cfg.join("governor-state.json"),
        b"{\"capacity_forecast\":{\"learned\":true},\"cycles\":412}\n",
    )
    .expect("seed governor-state.json");
    // The learned state took real cycles to accumulate; mode-strictness on the
    // config is deliberate (0600 is what a credentials-carrying config wants).
    fs::set_permissions(cfg.join("governor.yaml"), fs::Permissions::from_mode(0o600))
        .expect("set config mode");
    fs::set_permissions(cfg.join("governor-state.json"), fs::Permissions::from_mode(0o600))
        .expect("set state mode");
    let other = home.join(".config/other-app");
    fs::create_dir_all(&other).expect("create unrelated app dir");
    fs::write(other.join("keep.txt"), b"not the governor's\n").expect("seed unrelated app");
    let before = snapshot_dir(&home);
    (home, before)
}

/// An install dir seeded the way a real prior install leaves it: a runnable
/// `cgov` at 0755 plus an unrelated file the installer must never notice.
fn existing_install(sandbox: &TempDir, version: &str) -> (PathBuf, Vec<(String, u32, Vec<u8>)>) {
    let install_dir = sandbox.path().join("bin");
    fs::create_dir_all(&install_dir).expect("create existing install dir");
    fs::write(install_dir.join("cgov"), runnable_artifact(version)).expect("seed prior binary");
    fs::set_permissions(install_dir.join("cgov"), fs::Permissions::from_mode(0o755))
        .expect("set prior binary mode");
    fs::write(install_dir.join("keep.txt"), b"leave me alone\n").expect("seed marker");
    let before = snapshot_dir(&install_dir);
    (install_dir, before)
}

/// Full recursive snapshot of a directory: (relative path, mode, bytes),
/// sorted. "Preserved" means byte- and mode-identical, no additions, no
/// removals.
fn snapshot_dir(dir: &Path) -> Vec<(String, u32, Vec<u8>)> {
    fn walk(base: &Path, rel: &str, out: &mut Vec<(String, u32, Vec<u8>)>) {
        for entry in fs::read_dir(base).expect("read dir") {
            let entry = entry.expect("read entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let path = entry.path();
            let meta = entry.metadata().expect("stat entry");
            if meta.is_dir() {
                walk(&path, &child_rel, out);
            } else {
                let mode = meta.permissions().mode() & 0o777;
                let contents = fs::read(&path).expect("read file");
                out.push((child_rel, mode, contents));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn assert_exec_output(path: &Path, expected: &str) {
    let out = Command::new(path)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("{} did not execute: {e}", path.display()));
    assert!(
        out.status.success(),
        "{} exited non-zero: {}{}",
        path.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(expected),
        "{} printed the wrong version: {}",
        path.display(),
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The `cgov` entry of a snapshot, replaced by a fresh 0755 install of
/// `bytes` — the expected post-upgrade state of an install dir whose other
/// entries must survive unchanged.
fn with_binary_replaced(before: &[(String, u32, Vec<u8>)], bytes: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
    before
        .iter()
        .map(|(name, mode, old)| {
            if name == "cgov" {
                ("cgov".to_string(), 0o755, bytes.to_vec())
            } else {
                (name.clone(), *mode, old.clone())
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

#[test]
fn upgrade_over_existing_install_replaces_only_the_binary() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let new = runnable_artifact("0.1.2");

    let mut server = mockito::Server::new();
    let (artifact_mock, sidecar_mock) = serve_release(&mut server, "v0.1.2", &artifact, &new);

    let script = materialize_installer(sandbox.path(), &server.url());
    let (install_dir, before) = existing_install(&sandbox, "0.1.1");
    let (home, home_before) = seeded_home(&sandbox);

    let (ok, out) = run_installer(
        &script,
        &home,
        &install_dir,
        &[("CGOV_VERSION", "v0.1.2")],
    );
    assert!(
        ok,
        "upgrade over an existing install must succeed:\n{out}\n(server at {})",
        server.url()
    );
    assert!(
        out.contains("Checksum OK (published sidecar)"),
        "the upgrade must be sidecar-verified:\n{out}"
    );
    artifact_mock.assert();
    sidecar_mock.assert();

    // The binary was replaced: exact verified bytes, mode 0755, and it runs.
    assert_eq!(
        snapshot_dir(&install_dir),
        with_binary_replaced(&before, &new),
        "the upgrade must replace cgov and nothing else in the install dir"
    );
    assert_exec_output(&install_dir.join("cgov"), "cgov 0.1.2-sandbox");

    // Configuration and daemon state are not the installer's business.
    assert_eq!(
        snapshot_dir(&home),
        home_before,
        "an upgrade must not touch ~/.config (config, state, unrelated apps)"
    );
}

#[test]
fn interrupted_transfer_aborts_upgrade_and_prior_version_keeps_working() {
    let sandbox = TempDir::new().expect("sandbox");
    // The artifact name is not served by the raw listener below, but resolving
    // it asserts this host is a platform install.sh supports at all.
    let _artifact = release_artifact_name();

    // A release server that accepts the request, starts the body, and dies
    // mid-transfer: mockito cannot sever a connection partway, so serve the
    // truncated response from a raw listener. Content-Length promises 8192
    // bytes; 64 arrive; the socket closes. curl fails the transfer (exit 18),
    // which is the interrupted-upgrade shape — not an HTTP error the -f flag
    // would also catch.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind raw server");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept installer connection");
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request); // consume the GET (size irrelevant)
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 8192\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(head);
        let _ = stream.write_all(&[0u8; 64]); // ...then the transfer dies
        drop(stream);
    });

    let script = materialize_installer(sandbox.path(), &format!("http://127.0.0.1:{port}"));

    let (install_dir, before) = existing_install(&sandbox, "0.1.1");
    let (home, home_before) = seeded_home(&sandbox);

    let (ok, out) = run_installer(
        &script,
        &home,
        &install_dir,
        &[("CGOV_VERSION", "v0.9.0")],
    );
    assert!(
        !ok,
        "a truncated transfer must fail the upgrade:\n{out}"
    );
    assert!(
        out.contains("Download failed"),
        "refusal must identify the failed download:\n{out}"
    );

    // Nothing was written, and the prior version still executes — the safe
    // state a retried or rolled-back upgrade starts from.
    assert_eq!(
        snapshot_dir(&install_dir),
        before,
        "an interrupted upgrade must not modify the install dir"
    );
    assert_eq!(
        snapshot_dir(&home),
        home_before,
        "an interrupted upgrade must not touch ~/.config"
    );
    assert_exec_output(&install_dir.join("cgov"), "cgov 0.1.1-sandbox");
}

#[test]
fn verified_but_non_executable_artifact_installs_and_reports_the_failed_probe() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let broken = non_executable_artifact();

    let mut server = mockito::Server::new();
    let (artifact_mock, sidecar_mock) = serve_release(&mut server, "v0.2.0", &artifact, &broken);

    let script = materialize_installer(sandbox.path(), &server.url());
    let (install_dir, before) = existing_install(&sandbox, "0.1.1");
    let (home, home_before) = seeded_home(&sandbox);

    let (ok, out) = run_installer(
        &script,
        &home,
        &install_dir,
        &[("CGOV_VERSION", "v0.2.0")],
    );
    // Documented behavior: verification gates the bytes, not the behavior.
    // A digest-valid artifact installs even if it cannot run; the post-install
    // --version probe is best-effort and must not fail the run — the release
    // IS what the digest vouched for.
    assert!(
        ok,
        "a digest-valid artifact must install regardless of executability:\n{out}"
    );
    assert!(
        out.contains("Checksum OK (published sidecar)"),
        "the artifact must have been sidecar-verified:\n{out}"
    );
    assert!(
        out.contains("(version check failed)"),
        "the failed post-install probe must be reported, not silent:\n{out}"
    );
    artifact_mock.assert();
    sidecar_mock.assert();

    // The upgrade took effect — the working binary was replaced by the broken
    // one, at the standard mode — and nothing else changed. This is the state
    // rollback (next test) exists to repair.
    assert_eq!(
        snapshot_dir(&install_dir),
        with_binary_replaced(&before, &broken),
        "the broken-but-verified artifact replaces the installed binary only"
    );
    let run = Command::new(install_dir.join("cgov")).arg("--version").output();
    let executed = match &run {
        // ENOEXEC (os error 8): the kernel refused the file outright —
        // matched by raw code, not ErrorKind, which is unstable for this case.
        Err(e) if e.raw_os_error() == Some(8) => false,
        Err(e) => panic!("unexpected spawn error (not an exec-format refusal): {e}"),
        Ok(o) => o.status.success(),
    };
    assert!(
        !executed,
        "the installed artifact must indeed fail to execute: {run:?}"
    );
    assert_eq!(
        snapshot_dir(&home),
        home_before,
        "an upgrade with a broken artifact still must not touch ~/.config"
    );
}

#[test]
fn rollback_to_a_prior_pinned_version_restores_the_working_binary() {
    let sandbox = TempDir::new().expect("sandbox");
    let artifact = release_artifact_name();
    let broken = non_executable_artifact();
    let good = runnable_artifact("0.1.1");
    let good_digest = sha256_hex(&good);

    // Release v0.2.0: broken but sidecar-consistent (what a bad release looks
    // like). Release v0.1.1: the known-good prior version, published with a
    // sidecar that the digest-pinned rollback must never consult.
    let mut server = mockito::Server::new();
    let (broken_artifact_mock, broken_sidecar_mock) =
        serve_release(&mut server, "v0.2.0", &artifact, &broken);
    let good_artifact_mock = server
        .mock("GET", format!("/releases/download/v0.1.1/{artifact}").as_str())
        .with_status(200)
        .with_body(good.clone())
        .create();
    let never_consulted_sidecar = server
        .mock("GET", format!("/releases/download/v0.1.1/{artifact}.sha256").as_str())
        .with_status(200)
        .with_body(sidecar_bytes(&good_digest, &artifact))
        .expect(0) // assert() below enforces: never fetched
        .create();

    let script = materialize_installer(sandbox.path(), &server.url());
    let (install_dir, _seed_snapshot_unused) = existing_install(&sandbox, "0.1.1");
    let (home, home_before) = seeded_home(&sandbox);

    // Step 1: the bad release installs (as in the previous test).
    let (ok, out) = run_installer(
        &script,
        &home,
        &install_dir,
        &[("CGOV_VERSION", "v0.2.0")],
    );
    assert!(ok, "the bad release must install (its bytes verify):\n{out}");
    broken_artifact_mock.assert();
    broken_sidecar_mock.assert();

    // Step 2: roll back — tag AND digest pinned to the prior verified
    // version. The installer overwrites the broken binary unconditionally;
    // rollback is the same operation as install.
    let (ok, out) = run_installer(
        &script,
        &home,
        &install_dir,
        &[("CGOV_VERSION", "v0.1.1"), ("CGOV_SHA256", &good_digest)],
    );
    assert!(ok, "the digest-pinned rollback must succeed:\n{out}");
    assert!(
        out.contains("Checksum OK (caller-supplied digest)"),
        "the rollback must be verified through the caller's digest:\n{out}"
    );
    good_artifact_mock.assert();
    never_consulted_sidecar.assert();

    // The prior version is back: exact bytes, mode 0755, and it runs.
    assert_eq!(
        fs::read(install_dir.join("cgov")).expect("read rolled-back binary"),
        good,
        "the rolled-back binary must be the digest-pinned bytes"
    );
    assert_eq!(
        fs::metadata(install_dir.join("cgov"))
            .expect("stat rolled-back binary")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "the rolled-back binary must be installed at mode 0755"
    );
    assert_exec_output(&install_dir.join("cgov"), "cgov 0.1.1-sandbox");
    assert_eq!(
        snapshot_dir(&home),
        home_before,
        "a rollback must not touch ~/.config — same config, older binary"
    );
}
