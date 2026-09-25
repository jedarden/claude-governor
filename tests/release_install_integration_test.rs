//! End-to-end release integrity: the publication gate, its uploaded bytes,
//! and the installer, as one chain (claudego-13ab5da2).
//!
//! The three release-integrity layers each have contract coverage in
//! isolation: `tests/release_publication_gate_test.rs` (claudego-78c221be)
//! and `tests/release_provenance_asset_pairing_test.rs` (claudego-3f7b7f49)
//! pin `scripts/publish-release.sh`'s four fail-closed phases, and
//! `tests/install_sh_release_verification.rs` (claudego-4531120c) pins
//! `install.sh` against a mock release server. What none of them proves is
//! that the layers *compose* — that a release the gate actually cut is one
//! the installer actually accepts, byte for byte:
//!
//! * **the happy chain** — the gate validates Forgejo provenance (origin is
//!   the source of truth, the tag points at HEAD and resolves to it on
//!   Forgejo), both architecture artifacts, their static linkage through the
//!   real `scripts/verify-release-static.sh`, and the required `sha256sum -c`
//!   sidecars; the `gh` fake stages what the gate uploaded, those exact bytes
//!   are served back by a mock release server, and the shipped installer
//!   installs the host-architecture artifact digest-verified through the
//!   *published sidecar* path, with no caller-supplied digest. This is the
//!   cross-script contract no per-layer test can catch: if the gate ever
//!   accepted a sidecar shape `install.sh`'s `sha256sum -c` run would reject
//!   (or uploaded a digest the consumer reads differently), only a chain
//!   like this one fails;
//! * **gate/installer agreement on a mismatch** — an artifact tampered after
//!   its sidecar was written is refused by the gate's sidecar phase (so
//!   static validation alone cannot admit it, and nothing is uploaded), and
//!   the same tampered bytes served anyway — as if the gate had been
//!   bypassed or the mirror swapped contents under an immutable asset — are
//!   refused by the installer too, with an existing install directory
//!   byte-for-byte unchanged and a fresh install dir never even created;
//! * **a release that lost its sidecar after the cut** (a partial mirror
//!   upload) is refused the same way, install directory untouched.
//!
//! The sandbox is the gate test's design extended one hop: both gate scripts
//! `include_str!`-embedded at compile time (close-gate extraction safe — no
//! runtime `CARGO_MANIFEST_DIR` reads), a throwaway checkout whose `origin`
//! is a local bare repo standing in for Forgejo with the tag pushed there,
//! hand-assembled static ELF64 artifacts that genuinely pass the real static
//! validator, and a recording `gh` fake whose `release create` call stages
//! every uploaded file into an `uploaded/` directory — the stand-in for what
//! lands on the release. The installer is embedded the same way, its single
//! `RELEASES_BASE` substitution asserted line-equal to the shipped literal
//! before being redirected at the mock server. Nothing here reaches the
//! network.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mockito::Mock;
use tempfile::TempDir;

/// The shipped publication gate, embedded at compile time.
const PUBLISH_SH: &str = include_str!("../scripts/publish-release.sh");

/// The shipped static validator, embedded because the gate invokes it as a
/// sibling file and the sandbox has to reproduce that layout.
const STATIC_CHECK_SH: &str = include_str!("../scripts/verify-release-static.sh");

/// The shipped installer, embedded at compile time.
const INSTALL_SH: &str = include_str!("../install.sh");

const TAG: &str = "v0.1.2";

/// The message every sandbox artifact prints: the gate's `env -i` execution
/// probe needs exit 0 with output, and the installer's post-install
/// `--version` log line needs the same.
const ARTIFACT_MESSAGE: &[u8] = b"cgov 0.1.2-integration\n";

// ---------------------------------------------------------------------------
// Hand-assembled static ELFs (the gate test's helpers, verbatim shape)
// ---------------------------------------------------------------------------

/// ELF64 header + single R+X PT_LOAD, no PT_INTERP, no dynamic section —
/// the exact shape `verify-release-static.sh` calls statically linked.
/// `e_machine` selects the architecture; `code` is real machine code that
/// writes `message` to stdout and exits 0, so the host-architecture artifact
/// also passes the script's `env -i` execution probe.
fn static_elf(machine: u16, code: &[u8], message: &[u8]) -> Vec<u8> {
    const BASE: u64 = 0x4000_0000;
    const CODE_OFF: usize = 0x78; // 64-byte ehdr + 56-byte phdr = 120 exactly

    let entry = BASE + CODE_OFF as u64;
    let filesz = (CODE_OFF + code.len() + message.len()) as u64;

    let mut b = Vec::with_capacity(filesz as usize);
    // e_ident: ELFmagic, 64-bit, little-endian, v1, SysV, no ABI
    b.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    b.extend_from_slice(&[0u8; 8]);
    b.extend_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
    b.extend_from_slice(&machine.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes()); // e_version
    b.extend_from_slice(&entry.to_le_bytes());
    b.extend_from_slice(&64u64.to_le_bytes()); // e_phoff
    b.extend_from_slice(&0u64.to_le_bytes()); // e_shoff
    b.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    b.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
    b.extend_from_slice(&56u16.to_le_bytes()); // e_phentsize
    b.extend_from_slice(&1u16.to_le_bytes()); // e_phnum
    b.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
    b.extend_from_slice(&0u16.to_le_bytes()); // e_shnum
    b.extend_from_slice(&0u16.to_le_bytes()); // e_shstrndx
    // Phdr: one PT_LOAD, R+X, whole file mapped at BASE
    b.extend_from_slice(&1u32.to_le_bytes()); // p_type = PT_LOAD
    b.extend_from_slice(&5u32.to_le_bytes()); // p_flags = R + X
    b.extend_from_slice(&0u64.to_le_bytes()); // p_offset
    b.extend_from_slice(&BASE.to_le_bytes()); // p_vaddr
    b.extend_from_slice(&BASE.to_le_bytes()); // p_paddr
    b.extend_from_slice(&filesz.to_le_bytes());
    b.extend_from_slice(&filesz.to_le_bytes()); // p_memsz
    b.extend_from_slice(&0x1000u64.to_le_bytes()); // p_align
    while b.len() < CODE_OFF {
        b.push(0);
    }
    b.extend_from_slice(code);
    b.extend_from_slice(message);
    assert_eq!(b.len() as u64, filesz);
    b
}

/// x86-64: write(1, message, len); exit(0). Stable kernel ABI, no libc.
fn static_elf_x86_64(message: &[u8]) -> Vec<u8> {
    let mut code: Vec<u8> = vec![
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1   (SYS_write)
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1   (stdout)
        0xbe, 0, 0, 0, 0, // mov esi, msg  (patched below)
        0xba, 0, 0, 0, 0, // mov edx, len  (patched below)
        0x0f, 0x05, // syscall
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, 60  (SYS_exit)
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
    ];
    assert_eq!(code.len(), 31);
    let msg_addr = (0x4000_0000u64 + (0x78 + code.len()) as u64) as u32;
    code[11..15].copy_from_slice(&msg_addr.to_le_bytes());
    code[16..20].copy_from_slice(&(message.len() as u32).to_le_bytes());
    static_elf(0x3e, &code, message) // EM_X86_64
}

/// AArch64: the same write/exit pair via the stable kernel ABI, no libc.
fn static_elf_aarch64(message: &[u8]) -> Vec<u8> {
    let msg_addr = 0x4000_0000u64 + 0x78 + 40; // code is 10 instructions
    let movz = |imm: u32, rd: u32| 0xd280_0000 | (imm << 5) | rd;
    // AArch64 encodes the halfword position in `hw` (shift / 16), not the
    // byte shift itself. The foreign artifact is executed whenever qemu is
    // available, so keep this fixture valid under the real probe.
    let movk = |imm: u32, shift: u32, rd: u32| {
        0xf280_0000 | ((shift / 16) << 21) | (imm << 5) | rd
    };
    let words: Vec<u32> = vec![
        movz(1, 0),                                    // mov x0, #1 (stdout)
        movz(message.len() as u32, 2),                 // mov x2, #len
        movz(msg_addr as u32 & 0xffff, 1),             // mov x1, addr lo16
        movk((msg_addr >> 16) as u32 & 0xffff, 16, 1), //         mid16
        movk((msg_addr >> 32) as u32 & 0xffff, 32, 1), //         hi16
        movz(64, 8),                                   // mov x8, #64 (SYS_write)
        0xd400_0001,                                   // svc #0
        movz(0, 0),                                    // mov x0, #0
        movz(93, 8),                                   // mov x8, #93 (SYS_exit)
        0xd400_0001,                                   // svc #0
    ];
    let mut code = Vec::with_capacity(words.len() * 4);
    for w in words {
        code.extend_from_slice(&w.to_le_bytes());
    }
    assert_eq!(code.len(), 40);
    static_elf(0xb7, &code, message) // EM_AARCH64
}

// ---------------------------------------------------------------------------
// Sandbox: git checkout + Forgejo stand-in + uploading gh fake
// ---------------------------------------------------------------------------

struct Sandbox {
    dir: TempDir,
    release_dir: PathBuf,
    origin_url: String,
    gh_log: PathBuf,
    /// Where the gh fake stages every file passed to `release create` — the
    /// stand-in for the bytes that land on the published release.
    uploaded: PathBuf,
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=claudego-13ab5da2@test",
            "-c",
            "user.name=chain-test",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
}

fn sha256_hex(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("spawn sha256sum (coreutils, required by the gate and installer alike)");
    assert!(out.status.success(), "sha256sum exited non-zero");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum prints a digest")
        .to_string()
}

/// Sidecar in the published format both scripts agree on — the gate
/// validates it and `install.sh` feeds it to `sha256sum -c`.
fn sidecar_bytes(digest: &str, artifact: &str) -> Vec<u8> {
    format!("{digest}  {artifact}\n").into_bytes()
}

/// Build the chain sandbox: both gate scripts materialized as siblings under
/// `scripts/`, a one-commit git checkout whose `origin` is a local bare repo
/// (the Forgejo stand-in), tag `TAG` at HEAD and pushed to the stand-in, and
/// a `bin/gh` fake that logs its argv, stages `release create` uploads into
/// `uploaded/`, and answers the post-publish asset query from the
/// `CGOV_FAKE_GH_ASSETS` scenario file. The release dir starts with both
/// artifacts and matching sidecars.
fn sandbox_with_good_release() -> Sandbox {
    let dir = TempDir::new().expect("sandbox");

    // The gate and its sibling validator, exactly the committed bytes.
    fs::create_dir_all(dir.path().join("scripts")).expect("scripts dir");
    write_executable(
        &dir.path().join("scripts/publish-release.sh"),
        PUBLISH_SH.as_bytes(),
    );
    write_executable(
        &dir.path().join("scripts/verify-release-static.sh"),
        STATIC_CHECK_SH.as_bytes(),
    );

    // Release dir = a git checkout whose origin is a local bare repo.
    let release_dir = dir.path().join("release");
    let origin = dir.path().join("forgejo-standin.git");
    fs::create_dir(&release_dir).expect("release dir");
    fs::create_dir(&origin).expect("bare stand-in dir");
    git(&release_dir, &["init", "-q", "-b", "main"]);
    fs::write(release_dir.join("README.md"), "built artifact\n").expect("seed file");
    git(&release_dir, &["add", "README.md"]);
    git(&release_dir, &["commit", "-q", "-m", "release commit"]);

    git(&origin, &["init", "-q", "--bare", "-b", "main"]);
    let origin_url = origin.to_str().expect("origin path").to_string();
    git(&release_dir, &["remote", "add", "origin", &origin_url]);

    // Both architecture artifacts: hand-assembled static ELFs that pass the
    // real validator (the host-arch one genuinely executes; the foreign one
    // gets the linkage checks, and its probe only runs if this host can
    // emulate it).
    write_executable(
        &release_dir.join("cgov-linux-amd64"),
        &static_elf_x86_64(ARTIFACT_MESSAGE),
    );
    write_executable(
        &release_dir.join("cgov-linux-arm64"),
        &static_elf_aarch64(ARTIFACT_MESSAGE),
    );
    for name in ["cgov-linux-amd64", "cgov-linux-arm64"] {
        let digest = sha256_hex(&release_dir.join(name));
        fs::write(
            release_dir.join(format!("{name}.sha256")),
            sidecar_bytes(&digest, name),
        )
        .expect("write sidecar");
    }

    // Tag at HEAD, and on the Forgejo stand-in.
    git(&release_dir, &["tag", TAG]);
    git(
        &release_dir,
        &["push", "-q", "origin", &format!("refs/tags/{TAG}")],
    );

    // Recording + staging gh fake. Newlines inside argv (the gate's --notes
    // span lines) are collapsed so one gh call is exactly one log line;
    // `release create` additionally copies every absolute-path argument into
    // uploaded/ — those staged bytes are what the mock release server below
    // serves back to the installer.
    let gh_log = dir.path().join("gh.log");
    let uploaded = dir.path().join("uploaded");
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).expect("bin dir");
    fs::create_dir_all(&uploaded).expect("uploaded dir");
    let log_display = gh_log.to_str().expect("log path").to_string();
    let uploaded_display = uploaded.to_str().expect("uploaded path").to_string();
    write_executable(
        &bin.join("gh"),
        &format!(
            r#"#!/usr/bin/env bash
# Test double for the GitHub CLI: records every argv as one line, stages
# release create uploads, answers the post-publish asset query from a
# scenario file.
{{ printf '%s' "$*" | tr '\n' ' '; printf '\n'; }} >> "{log_display}"
case " $1 $2 " in
  *" release create "*)
    for a in "$@"; do
      case "$a" in /*) cp -p "$a" "{uploaded_display}/" ;; esac
    done
    exit 0 ;;
  *" release view "*) cat "${{CGOV_FAKE_GH_ASSETS:?}}"; exit 0 ;;
  *) echo "fake gh: unexpected call: $*" >&2; exit 64 ;;
esac
"#
        )
        .into_bytes(),
    );

    Sandbox {
        dir,
        release_dir,
        origin_url,
        gh_log,
        uploaded,
    }
}

fn good_assets_json() -> String {
    // Compact shape gh --json assets prints; only the names matter to the gate.
    r#"{"assets":[{"name":"cgov-linux-amd64","size":154},{"name":"cgov-linux-amd64.sha256","size":69},{"name":"cgov-linux-arm64","size":154},{"name":"cgov-linux-arm64.sha256","size":69}]}"#
        .to_string()
}

/// Run the materialized gate. `PATH` is prefixed with the sandbox `bin/`
/// (the fake gh), `CGOV_FORGEJO_URL` points provenance at the stand-in.
fn run_gate(sb: &Sandbox, assets_json: &str) -> (bool, String) {
    let assets_file = sb.dir.path().join("assets.json");
    fs::write(&assets_file, assets_json).expect("write fake gh assets scenario");
    let fake_bin = sb.dir.path().join("bin");
    let path = std::env::var("PATH").unwrap_or_default();
    let out = Command::new("bash")
        .arg(sb.dir.path().join("scripts/publish-release.sh"))
        .args(["--version", TAG, "--release-dir"])
        .arg(&sb.release_dir)
        .env("CGOV_FORGEJO_URL", &sb.origin_url)
        .env("CGOV_FAKE_GH_ASSETS", &assets_file)
        .env("PATH", format!("{}:{}", fake_bin.display(), path))
        .env_remove("CGOV_GH_REPO")
        .output()
        .expect("spawn bash on the sandboxed gate");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn gh_log_lines(sb: &Sandbox) -> Vec<String> {
    match fs::read_to_string(&sb.gh_log) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) if !sb.gh_log.exists() => Vec::new(),
        Err(e) => panic!("read gh log: {e}"),
    }
}

/// The asset basenames the fake gh received on `release create`, in upload
/// order — the published asset set, as the gate cut it.
fn uploaded_asset_names(sb: &Sandbox) -> Vec<String> {
    gh_log_lines(sb)
        .into_iter()
        .filter(|l| {
            l.split_whitespace().take(2).collect::<Vec<_>>() == ["release", "create"]
        })
        .flat_map(|l| {
            l.split_whitespace()
                .filter(|a| a.starts_with('/'))
                .map(|a| Path::new(a).file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The artifact install.sh downloads on this host — `uname -s` must be Linux
/// (the script refuses anything else) and the arch maps exactly the way the
/// script's own case statement maps it.
fn host_artifact_name() -> String {
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

/// Serve a staged release from the sandbox server under the pinned-tag
/// download path install.sh builds from `CGOV_VERSION={TAG}`:
/// `<base>/releases/download/{TAG}/<asset>`. `sidecar: None` publishes the
/// artifact without its digest sidecar — mockito 404s unregistered routes,
/// which is exactly a sidecar missing from the release.
fn serve_release(
    server: &mut mockito::Server,
    artifact: &str,
    bytes: &[u8],
    sidecar: Option<Vec<u8>>,
) -> (Mock, Option<Mock>) {
    let prefix = format!("/releases/download/{TAG}");
    let artifact_mock = server
        .mock("GET", format!("{prefix}/{artifact}").as_str())
        .with_status(200)
        .with_body(bytes.to_vec())
        .create();
    let sidecar_mock = sidecar.map(|body| {
        server
            .mock("GET", format!("{prefix}/{artifact}.sha256").as_str())
            .with_status(200)
            .with_body(body)
            .create()
    });
    (artifact_mock, sidecar_mock)
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

/// Run the sandboxed installer pinned to `TAG` with a hermetic environment:
/// HOME pointed at the sandbox (so no real dotfile is consulted), CI=true
/// (the interactive `cgov init` branch is for terminals only), and all three
/// CGOV_ variables set explicitly — empty means "unset" to the script's
/// `${VAR:-}` reads, so inherited values from the worker environment cannot
/// leak into a case.
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
        .env("CGOV_VERSION", TAG)
        .env("CGOV_SHA256", "");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn bash on the sandboxed install.sh");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// Sandbox HOME — so the child bash never resolves dotfiles against the
/// worker's real home.
fn sandbox_home(sandbox: &TempDir) -> PathBuf {
    let home = sandbox.path().join("home");
    fs::create_dir_all(&home).expect("create sandbox HOME");
    home
}

fn fresh_install_dir(sandbox: &TempDir) -> PathBuf {
    sandbox.path().join("bin")
}

/// Seed an installation directory before a refusal-path test: a working
/// binary at a distinctive mode plus an unrelated file. A failed
/// verification must preserve an existing installation just as carefully as
/// it avoids creating a new one.
fn existing_install_dir(sandbox: &TempDir) -> (PathBuf, Vec<(String, u32, Vec<u8>)>) {
    let install_dir = fresh_install_dir(sandbox);
    fs::create_dir_all(&install_dir).expect("create existing install dir");
    fs::write(install_dir.join("cgov"), b"existing cgov\n").expect("seed existing binary");
    fs::write(install_dir.join("keep.txt"), b"leave me alone\n").expect("seed marker");
    fs::set_permissions(install_dir.join("cgov"), fs::Permissions::from_mode(0o700))
        .expect("set existing binary mode");
    let snapshot = snapshot_dir(&install_dir);
    (install_dir, snapshot)
}

/// Full recursive snapshot of an install dir: (relative path, mode, bytes),
/// sorted. "Nothing was written" means this is byte-identical afterwards —
/// no new file, no removal, no overwrite, no mode change.
fn snapshot_dir(dir: &Path) -> Vec<(String, u32, Vec<u8>)> {
    fn walk(base: &Path, rel: &str, out: &mut Vec<(String, u32, Vec<u8>)>) {
        for entry in fs::read_dir(base).expect("read install dir") {
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
        "installed bytes differ from the uploaded artifact"
    );
    let version = Command::new(&installed)
        .arg("--version")
        .output()
        .expect("run installed binary");
    assert!(
        version.status.success(),
        "installed binary did not execute; mode broken"
    );
    assert!(
        String::from_utf8_lossy(&version.stdout).contains(
            String::from_utf8_lossy(ARTIFACT_MESSAGE).trim(),
        ),
        "installed binary produced wrong --version output"
    );
}

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

#[test]
fn gate_cut_release_installs_end_to_end_from_its_uploaded_bytes() {
    let sb = sandbox_with_good_release();

    // The gate must prove the whole documented story before uploading:
    // Forgejo origin, tag at HEAD resolving on Forgejo, both architecture
    // artifacts, real static validation, matching sidecars.
    let (ok, out) = run_gate(&sb, &good_assets_json());
    assert!(
        ok,
        "gate refused a well-formed release:\n{out}\nrelease dir: {}",
        sb.release_dir.display()
    );
    for phase in ["provenance", "artifact: cgov-linux-amd64", "artifact: cgov-linux-arm64",
                  "static: cgov-linux-amd64", "static: cgov-linux-arm64",
                  "sidecar: cgov-linux-amd64.sha256", "sidecar: cgov-linux-arm64.sha256"] {
        assert!(
            out.contains(&format!("PASS: {phase}")),
            "gate output must record phase {phase:?} passing:\n{out}"
        );
    }

    // The cut uploaded exactly the four assets, in the gate's own order.
    assert_eq!(
        uploaded_asset_names(&sb),
        vec![
            "cgov-linux-amd64",
            "cgov-linux-amd64.sha256",
            "cgov-linux-arm64",
            "cgov-linux-arm64.sha256",
        ],
        "gh must have received both binaries and both sidecars"
    );
    // And the staged bytes are the release-dir bytes, not rewrites: the
    // published digest is the artifact's actual digest.
    let staged_amd64 = fs::read(sb.uploaded.join("cgov-linux-amd64")).expect("staged amd64");
    assert_eq!(
        staged_amd64,
        fs::read(sb.release_dir.join("cgov-linux-amd64")).expect("release amd64"),
        "the fake gh must stage what the gate uploaded, unmodified"
    );

    // Serve the uploaded bytes back — the exact release a user's installer
    // sees — and run the shipped installer against it with no caller-supplied
    // digest, so verification rides the published sidecar.
    let sandbox = TempDir::new().expect("installer sandbox");
    let host_artifact = host_artifact_name();
    let staged_artifact = fs::read(sb.uploaded.join(&host_artifact)).expect("staged artifact");
    let staged_sidecar =
        fs::read(sb.uploaded.join(format!("{host_artifact}.sha256"))).expect("staged sidecar");

    let mut server = mockito::Server::new();
    let (artifact_mock, sidecar_mock) =
        serve_release(&mut server, &host_artifact, &staged_artifact, Some(staged_sidecar));

    let script = materialize_installer(sandbox.path(), &server.url());
    let install_dir = fresh_install_dir(&sandbox);
    let (installed, install_out) = run_installer(
        &script,
        &sandbox_home(&sandbox),
        &install_dir,
        &[],
    );
    assert!(
        installed,
        "installer refused the release the gate cut:\n{install_out}\n(server at {})",
        server.url()
    );
    artifact_mock.assert();
    sidecar_mock.expect("sidecar route must be registered").assert();
    assert!(
        install_out.contains("Checksum OK (published sidecar)"),
        "the install must be verified through the published sidecar, \
         not a caller-supplied digest:\n{install_out}"
    );
    assert_installed_binary(&install_dir, &staged_artifact);
}

#[test]
fn tampered_artifact_the_gate_refuses_is_rejected_by_the_installer_too_without_modifying_the_install_dir()
{
    let sb = sandbox_with_good_release();
    // Corrupt the host artifact AFTER its sidecar was written — the shape of
    // a build or disk defect the published digest still remembers. Static
    // validation still passes (the ELF linkage is intact), so this isolates
    // the sidecar phase of the gate.
    let host_artifact = host_artifact_name();
    let artifact_path = sb.release_dir.join(&host_artifact);
    let mut bytes = fs::read(&artifact_path).expect("read artifact");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    fs::write(&artifact_path, &bytes).expect("tamper artifact");

    // The gate refuses, in its sidecar phase, and uploads nothing at all.
    let (ok, out) = run_gate(&sb, &good_assets_json());
    assert!(
        !ok,
        "the gate must refuse a release whose artifact no longer matches its \
         sidecar:\n{out}"
    );
    assert!(
        out.contains("FAIL: sidecar:"),
        "refusal must come from the sidecar phase (static linkage is intact):\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "a preflight refusal must reach gh never — not even one call"
    );

    // Serve the tampered bytes with the now-stale published sidecar anyway —
    // the gate-bypassed / mirror-swapped release.
    let stale_sidecar =
        fs::read(sb.release_dir.join(format!("{host_artifact}.sha256"))).expect("stale sidecar");

    // An existing installation survives byte-for-byte: no overwrite, no mode
    // change, no new files.
    let sandbox = TempDir::new().expect("installer sandbox");
    let mut server = mockito::Server::new();
    let (artifact_mock, sidecar_mock) =
        serve_release(&mut server, &host_artifact, &bytes, Some(stale_sidecar.clone()));
    let script = materialize_installer(sandbox.path(), &server.url());
    let (install_dir, before) = existing_install_dir(&sandbox);
    let (installed, install_out) =
        run_installer(&script, &sandbox_home(&sandbox), &install_dir, &[]);
    assert!(
        !installed,
        "the installer must reject what the gate would have rejected:\n{install_out}"
    );
    assert!(
        install_out.contains("Checksum verification FAILED"),
        "refusal must name the checksum failure:\n{install_out}"
    );
    assert!(
        install_out.contains("Nothing was written"),
        "refusal must say the install dir was untouched:\n{install_out}"
    );
    assert_eq!(
        snapshot_dir(&install_dir),
        before,
        "a digest-mismatch refusal must not modify the install directory"
    );
    artifact_mock.assert();
    sidecar_mock.expect("sidecar route must be registered").assert();

    // A fresh install dir is never even created: the installer's mkdir runs
    // only after verification passes.
    let sandbox2 = TempDir::new().expect("second installer sandbox");
    let mut server2 = mockito::Server::new();
    let (artifact_mock2, sidecar_mock2) =
        serve_release(&mut server2, &host_artifact, &bytes, Some(stale_sidecar));
    let script2 = materialize_installer(sandbox2.path(), &server2.url());
    let fresh_dir = fresh_install_dir(&sandbox2);
    let (installed2, install_out2) =
        run_installer(&script2, &sandbox_home(&sandbox2), &fresh_dir, &[]);
    assert!(!installed2, "the fresh-dir install must refuse too:\n{install_out2}");
    assert!(
        !fresh_dir.exists(),
        "a refused install must not create the install dir"
    );
    artifact_mock2.assert();
    sidecar_mock2.expect("sidecar route must be registered").assert();
}

#[test]
fn release_missing_its_published_sidecar_is_refused_without_modifying_the_install_dir() {
    let sb = sandbox_with_good_release();
    let (ok, out) = run_gate(&sb, &good_assets_json());
    assert!(ok, "gate refused a well-formed release:\n{out}");

    // The release lost the host artifact's sidecar after the cut — a partial
    // mirror upload. The artifact itself is served.
    let host_artifact = host_artifact_name();
    let staged_artifact = fs::read(sb.uploaded.join(&host_artifact)).expect("staged artifact");

    let sandbox = TempDir::new().expect("installer sandbox");
    let mut server = mockito::Server::new();
    let (artifact_mock, sidecar_mock) =
        serve_release(&mut server, &host_artifact, &staged_artifact, None);
    let script = materialize_installer(sandbox.path(), &server.url());
    let (install_dir, before) = existing_install_dir(&sandbox);
    let (installed, install_out) =
        run_installer(&script, &sandbox_home(&sandbox), &install_dir, &[]);
    assert!(
        !installed,
        "a release without its published digest must not install:\n{install_out}"
    );
    assert!(
        install_out.contains("Digest sidecar download failed"),
        "refusal must name the missing sidecar:\n{install_out}"
    );
    assert_eq!(
        snapshot_dir(&install_dir),
        before,
        "a missing-sidecar refusal must not modify the install directory"
    );
    artifact_mock.assert();
    assert!(
        sidecar_mock.is_none(),
        "no sidecar route was published; the installer must have failed on its absence"
    );
}
