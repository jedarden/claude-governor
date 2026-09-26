//! Publication-gate contract for `scripts/publish-release.sh`
//! (claudego-78c221be) — the fail-closed path between "artifacts are built"
//! and "a GitHub release is public". The README's provenance story stakes
//! three claims on the release pipeline, and the gate is what proves them:
//!
//! * "releases are built from the Forgejo tag": provenance fails unless the
//!   release dir is a clone of the Forgejo source of truth, the `vX.Y.Z` tag
//!   points exactly at the built HEAD, and Forgejo itself resolves that tag
//!   to the same commit;
//! * "every architecture artifact passes static validation": BOTH
//!   `cgov-linux-amd64` and `cgov-linux-arm64` must exist and each must pass
//!   the real `scripts/verify-release-static.sh` — a broken artifact on ANY
//!   architecture refuses the publication, not just the build host's;
//! * "each published GitHub asset has a matching immutable `.sha256`
//!   sidecar": each artifact needs a sidecar in exactly the `sha256sum -c`
//!   format `install.sh` consumes whose digest equals the artifact's actual
//!   digest, and after `gh release create` the published asset list is
//!   re-fetched and must pair every binary with its sidecar;
//! * "publication fails when any validation or sidecar check fails": every
//!   refusal path below asserts the `gh release create` call was NEVER made;
//! * "an artifact that never ran does not ship" (claudego-7c747ffb): the
//!   validator downgrades to linkage-only evidence, exit 0, when the host
//!   has NO way to execute a foreign-architecture artifact — the gate
//!   converts that skipped execution probe into a refusal, and only the
//!   explicit `CGOV_ALLOW_SKIPPED_PROBE=1` override (loudly noted in the
//!   output) may publish on linkage evidence alone.
//!
//! The tests execute both shipped scripts (`include_str!`-embedded at
//! compile time, following the repo's gate pattern — tests/adapter_var_sync.rs
//! and tests/install_sh_release_verification.rs) against a throwaway git
//! checkout whose origin is a local bare repo standing in for Forgejo, and a
//! recording `gh` fake that answers the post-publish asset query. The
//! static-validation phase is the REAL validator: the happy-path artifacts
//! are hand-assembled static ELF64 binaries (x86-64 and AArch64, no
//! interpreter, no NEEDED entries) that genuinely pass it, so the refusal
//! paths prove the gate wires the validator's verdict into the publish
//! decision. The sandbox `bin/` also carries `qemu-*-static` doubles for
//! BOTH architectures, so whichever artifact is foreign on this host runs
//! its execution probes under "an emulator" — the deterministic stand-in
//! for the `qemu-user-static` package cgov-ci installs; the skip path is
//! exercised deliberately in the skipped-probe tests at the bottom. Nothing
//! here reaches the network.
//!
//! The refusal family is complete, not sampled (claudego-719c1248): every
//! `die` branch the gate can take is pinned somewhere below — the usage and
//! argument-validation exits, a release dir that is not a checkout or has
//! no origin, a tag missing from the checkout (the mirror image of missing
//! from Forgejo), an unreachable Forgejo (distinct from a Forgejo that
//! answers with the wrong commit), a missing static validator, a missing
//! `gh` binary at publish time, a `gh release create` that itself
//! fails after every preflight check passed, and the skipped foreign-
//! architecture execution probe (claudego-7c747ffb).

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

/// The shipped publication gate, embedded at compile time.
const PUBLISH_SH: &str = include_str!("../scripts/publish-release.sh");

/// The shipped static validator, embedded because the gate invokes it as a
/// sibling file and the sandbox has to reproduce that layout.
const STATIC_CHECK_SH: &str = include_str!("../scripts/verify-release-static.sh");

/// The Forgejo source of truth the gate must default to — pinned here so a
/// well-meaning edit cannot silently repoint release provenance.
const FORGEJO_DEFAULT_LINE: &str =
    r#"FORGEJO_URL="${CGOV_FORGEJO_URL:-https://git.ardenone.com/jedarden/claude-governor.git}""#;

const TAG: &str = "v0.1.2";

// ---------------------------------------------------------------------------
// Hand-assembled static ELFs
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

/// AArch64: write(1, message, len); exit(0), built from movz/movk/svc.
fn static_elf_aarch64(message: &[u8]) -> Vec<u8> {
    let msg_addr = 0x4000_0000u64 + 0x78 + 40; // code is 10 instructions
    let movz = |imm: u32, rd: u32| 0xd280_0000 | (imm << 5) | rd;
    // `hw` (bits 21-22) is shift/16, not the raw byte count — shifting the
    // byte count in corrupts the opcode field and the instruction is illegal
    // when executed. Latent until claudego-8af2d72b: the foreign-arch
    // artifact was never executed before it gained an emulator path.
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
// Sandbox: git checkout + Forgejo stand-in + recording gh fake
// ---------------------------------------------------------------------------

struct Sandbox {
    dir: TempDir,
    release_dir: PathBuf,
    origin_url: String,
    gh_log: PathBuf,
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=claudego-78c221be@test",
            "-c",
            "user.name=gate-test",
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

/// `git()` for commands whose stdout the test needs (`rev-parse`): same
/// identity and config isolation, stdout trimmed and returned.
fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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
        .expect("spawn sha256sum (coreutils, required by the gate itself)");
    assert!(out.status.success(), "sha256sum exited non-zero");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum prints a digest")
        .to_string()
}

fn sidecar_bytes(digest: &str, artifact: &str) -> Vec<u8> {
    format!("{digest}  {artifact}\n").into_bytes()
}

/// This host's architecture mapped the way the validator's `host_machine`
/// maps it — the artifact of the OTHER architecture is the foreign one.
fn host_machine() -> String {
    let out = Command::new("uname")
        .arg("-m")
        .output()
        .expect("uname -m (the validator itself calls it)");
    match String::from_utf8_lossy(&out.stdout).trim() {
        "aarch64" | "arm64" => "aarch64".to_string(),
        _ => "x86_64".to_string(),
    }
}

fn foreign_machine() -> String {
    if host_machine() == "aarch64" {
        "x86_64".to_string()
    } else {
        "aarch64".to_string()
    }
}

/// A `qemu-<machine>-static` test double in the sandbox `bin/` — the shape
/// the static-validation suite uses. The validator's probe runs it under
/// `env -i`, so it may use only shell builtins and an absolute interpreter
/// path; it reports exit 0 with output, which is all the probe requires.
/// It stands in for the `qemu-user-static` package cgov-ci installs, making
/// the emulated-probe path deterministic on every host — which matters now
/// that the gate refuses a skipped probe (claudego-7c747ffb): without a way
/// to run the foreign artifact, every good release would be refused on an
/// emulator-less host.
fn install_fake_emulator(dir: &Path, machine: &str) {
    write_executable(
        &dir.join("bin").join(format!("qemu-{machine}-static")),
        &format!(
            r#"#!/bin/sh
# Test double for qemu-{machine}-static: builtins only (the probe runs it
# under env -i); success with output is all the probe requires.
echo "fake-qemu: executed $*"
exit 0
"#
        )
        .into_bytes(),
    );
}

/// Build the full sandbox: both scripts materialized as siblings under
/// `scripts/`, a one-commit git checkout whose `origin` is a local bare repo
/// (the Forgejo stand-in), tag `TAG` at HEAD and pushed to the stand-in, a
/// `bin/gh` fake that appends its argv to the log file and answers
/// `release view --json assets` from the `CGOV_FAKE_GH_ASSETS` scenario
/// file, and `qemu-*-static` doubles for both architectures in the same
/// `bin/` (the foreign artifact's probes run under the fake). The release
/// dir starts with both artifacts and matching sidecars.
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
    // real validator (the host-arch one genuinely executes natively; the
    // foreign one executes under the sandbox's emulator double below — the
    // deterministic stand-in for the emulator cgov-ci installs).
    let msg = b"cgov 0.1.2-sandbox\n";
    write_executable(&release_dir.join("cgov-linux-amd64"), &static_elf_x86_64(msg));
    write_executable(&release_dir.join("cgov-linux-arm64"), &static_elf_aarch64(msg));
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
    // Recording gh fake. Newlines inside argv (the gate's --notes span
    // lines) are collapsed so one gh call is exactly one log line.
    let gh_log = dir.path().join("gh.log");
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).expect("bin dir");
    let log_display = gh_log.to_str().expect("log path").to_string();
    write_executable(
        &bin.join("gh"),
        &format!(
            r#"#!/usr/bin/env bash
# Test double for the GitHub CLI: records every argv as one line, answers
# the post-publish asset query from a scenario file.
{{ printf '%s' "$*" | tr '\n' ' '; printf '\n'; }} >> "{log_display}"
case " $1 $2 " in
  *" release create "*) exit "${{CGOV_FAKE_GH_CREATE_RC:-0}}" ;;
  *" release view "*) cat "${{CGOV_FAKE_GH_ASSETS:?}}"; exit 0 ;;
  *) echo "fake gh: unexpected call: $*" >&2; exit 64 ;;
esac
"#
        )
        .into_bytes(),
    );

    // Emulator doubles for BOTH architectures: whichever artifact is
    // foreign on this host runs its execution probes under the fake, so
    // the static-validation phase exercises genuine execution everywhere.
    // Without it the gate would (correctly) refuse every good release on
    // an emulator-less host — the skipped-probe tests below strip these
    // from PATH to reach exactly that state deliberately.
    install_fake_emulator(dir.path(), "aarch64");
    install_fake_emulator(dir.path(), "x86_64");

    Sandbox {
        dir,
        release_dir,
        origin_url,
        gh_log,
    }
}

fn good_assets_json() -> String {
    // Compact shape gh --json assets prints; only the names matter to the gate.
    r#"{"assets":[{"name":"cgov-linux-amd64","size":150},{"name":"cgov-linux-amd64.sha256","size":69},{"name":"cgov-linux-arm64","size":150},{"name":"cgov-linux-arm64.sha256","size":69}]}"#
        .to_string()
}

/// Run the materialized gate. `PATH` is prefixed with the sandbox `bin/`
/// (the fake gh), `CGOV_FORGEJO_URL` points provenance at the stand-in.
fn run_gate(sb: &Sandbox, assets_json: &str, extra_args: &[&str]) -> (bool, String) {
    let (code, text) = run_gate_raw(sb, assets_json, extra_args, &[]);
    (code == 0, text)
}

/// `run_gate` with per-call environment overrides and the raw exit code —
/// the shape the refusal tests need (a different Forgejo URL, a failing
/// fake gh, a PATH without gh). `extra_args` may carry a second
/// `--release-dir`: the gate's parser keeps the last occurrence of a
/// repeated flag, so it overrides the sandbox default.
fn run_gate_raw(
    sb: &Sandbox,
    assets_json: &str,
    extra_args: &[&str],
    envs: &[(&str, String)],
) -> (i32, String) {
    let release_arg = sb.release_dir.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec!["--version", TAG, "--release-dir", &release_arg];
    args.extend_from_slice(extra_args);
    run_gate_args(sb, assets_json, &args, envs)
}

/// Low-level gate runner with complete argv control: the usage-error tests
/// pass their own flags (or none), so there is no default preamble here.
/// The fake-gh scenario file is written from `assets_json`; the standard
/// sandbox environment is applied first and `envs` after, so a per-call
/// override wins. Returns the raw exit code (the gate's usage errors are
/// 2, its validation refusals 1) and the combined output.
fn run_gate_args(
    sb: &Sandbox,
    assets_json: &str,
    args: &[&str],
    envs: &[(&str, String)],
) -> (i32, String) {
    let assets_file = sb.dir.path().join("assets.json");
    fs::write(&assets_file, assets_json).expect("write fake gh assets scenario");
    let fake_bin = sb.dir.path().join("bin");
    let path = std::env::var("PATH").unwrap_or_default();
    let mut cmd = Command::new("bash");
    cmd.arg(sb.dir.path().join("scripts/publish-release.sh"))
        .args(args)
        .env("CGOV_FORGEJO_URL", &sb.origin_url)
        .env("CGOV_FAKE_GH_ASSETS", &assets_file)
        .env("PATH", format!("{}:{}", fake_bin.display(), path))
        .env_remove("CGOV_GH_REPO");
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn bash on the sandboxed gate");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

fn gh_log_lines(sb: &Sandbox) -> Vec<String> {
    match fs::read_to_string(&sb.gh_log) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) if !sb.gh_log.exists() => Vec::new(),
        Err(e) => panic!("read gh log: {e}"),
    }
}

fn create_calls(sb: &Sandbox) -> Vec<String> {
    gh_log_lines(sb)
        .into_iter()
        .filter(|l| {
            l.split_whitespace().take(2).collect::<Vec<_>>() == ["release", "create"]
        })
        .collect()
}

fn asset_names_of(create_line: &str) -> Vec<String> {
    create_line
        .split_whitespace()
        .filter(|a| a.starts_with('/'))
        .map(|a| Path::new(a).file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

/// Delete `TAG` from the bare Forgejo stand-in — the remote keeps its
/// object store, only the ref goes away, exactly like a Forgejo-side delete.
fn delete_tag_on_standin(sb: &Sandbox) {
    let bare = sb.dir.path().join("forgejo-standin.git");
    let out = Command::new("git")
        .arg("-C")
        .arg(&bare)
        .args(["update-ref", "-d", &format!("refs/tags/{TAG}")])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn git update-ref on the stand-in");
    assert!(
        out.status.success(),
        "deleting the tag on the stand-in failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Move `TAG` on the bare Forgejo stand-in to `commit` (which must already
/// exist in the stand-in's object store) — the checkout's own ref is
/// untouched, exactly the shape of a Forgejo-side tag re-cut.
fn move_tag_on_standin(sb: &Sandbox, commit: &str) {
    let bare = sb.dir.path().join("forgejo-standin.git");
    let out = Command::new("git")
        .arg("-C")
        .arg(&bare)
        .args(["update-ref", &format!("refs/tags/{TAG}"), commit])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn git update-ref on the stand-in");
    assert!(
        out.status.success(),
        "moving the tag on the stand-in failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

#[test]
fn forgejo_default_is_the_source_of_truth() {
    assert!(
        PUBLISH_SH.contains(FORGEJO_DEFAULT_LINE),
        "the gate's default Forgejo URL drifted from the source of truth;\n\
         expected the shipped assignment to be:\n{FORGEJO_DEFAULT_LINE}"
    );
}

#[test]
fn happy_path_publishes_both_binaries_and_both_sidecars() {
    let sb = sandbox_with_good_release();
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(ok, "happy-path publication failed:\n{out}");

    let creates = create_calls(&sb);
    assert_eq!(creates.len(), 1, "exactly one gh release create:\n{out}");
    let uploaded = asset_names_of(&creates[0]);
    for expected in [
        "cgov-linux-amd64",
        "cgov-linux-amd64.sha256",
        "cgov-linux-arm64",
        "cgov-linux-arm64.sha256",
    ] {
        assert!(
            uploaded.iter().any(|b| b == expected),
            "release create must upload {expected}; uploaded {uploaded:?}:\n{out}"
        );
    }

    // The post-publish asset query must follow the create, never precede it.
    let log = gh_log_lines(&sb);
    let create_pos = log
        .iter()
        .position(|l| l.starts_with("release create"))
        .expect("create call logged");
    let view_pos = log
        .iter()
        .position(|l| l.starts_with("release view"))
        .expect("post-publish asset query logged");
    assert!(
        view_pos > create_pos,
        "asset verification must run after publication:\n{log:?}"
    );
    assert!(
        out.contains(
            "post-publish: cgov-linux-amd64 and cgov-linux-amd64.sha256 are both published"
        ),
        "output must confirm the published pairing:\n{out}"
    );
}

#[test]
fn dry_run_validates_everything_and_never_publishes() {
    let sb = sandbox_with_good_release();
    let (ok, out) = run_gate(&sb, &good_assets_json(), &["--dry-run"]);
    assert!(ok, "dry-run over a good release must pass:\n{out}");
    assert!(
        out.contains("dry-run OK"),
        "dry-run must announce itself:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "dry-run must make no gh call at all"
    );
}

#[test]
fn missing_sidecar_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    fs::remove_file(sb.release_dir.join("cgov-linux-arm64.sha256")).expect("remove sidecar");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(!ok, "a missing sidecar must refuse publication:\n{out}");
    assert!(
        out.contains("sidecar:"),
        "refusal must name the sidecar phase:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn tampered_sidecar_digest_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // The scenario the immutable sidecar exists for: binary real, digest
    // swapped for some other bytes' digest. Well-formed 64-hex, so it fails
    // exactly at the digest comparison.
    fs::write(
        sb.release_dir.join("cgov-linux-arm64.sha256"),
        sidecar_bytes(&"0".repeat(64), "cgov-linux-arm64"),
    )
    .expect("write tampered sidecar");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "a tampered sidecar digest must refuse publication:\n{out}"
    );
    assert!(
        out.contains("!= actual artifact digest"),
        "refusal must name the digest mismatch:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn sidecar_named_for_wrong_artifact_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // Correct digest of a real artifact, wrong filename: install.sh runs
    // `sha256sum -c cgov-linux-arm64.sha256` from the download dir, so a
    // sidecar naming cgov-linux-amd64 would fail there — the gate must
    // catch it before anything is published.
    let amd64_digest = sha256_hex(&sb.release_dir.join("cgov-linux-amd64"));
    fs::write(
        sb.release_dir.join("cgov-linux-arm64.sha256"),
        sidecar_bytes(&amd64_digest, "cgov-linux-amd64"),
    )
    .expect("write misnamed sidecar");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "a sidecar naming the wrong artifact must refuse:\n{out}"
    );
    assert!(
        out.contains("names 'cgov-linux-amd64'"),
        "refusal must name the sidecar's artifact mismatch:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn malformed_sidecar_shape_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // Not the "<digest>␠␠<name>" line sha256sum -c consumes.
    fs::write(sb.release_dir.join("cgov-linux-arm64.sha256"), "deadbeef\n")
        .expect("write malformed sidecar");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(!ok, "a malformed sidecar must refuse publication:\n{out}");
    assert!(
        out.contains("sha256sum -c format"),
        "refusal must explain the required sidecar shape:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn static_validation_failure_on_any_architecture_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // Break the FOREIGN architecture only: the build host's own artifact is
    // perfect, and the release must still be refused. A shell script fails
    // the validator at "could not parse it as ELF". The sidecar is
    // regenerated to match, so the refusal is attributable to the static
    // phase alone.
    let arm64 = sb.release_dir.join("cgov-linux-arm64");
    fs::write(&arm64, b"#!/bin/sh\necho not-an-elf\n").expect("write broken arm64 artifact");
    fs::set_permissions(&arm64, fs::Permissions::from_mode(0o755)).expect("chmod");
    let digest = sha256_hex(&arm64);
    fs::write(
        sb.release_dir.join("cgov-linux-arm64.sha256"),
        sidecar_bytes(&digest, "cgov-linux-arm64"),
    )
    .expect("re-sidecar the broken artifact");

    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "a foreign-architecture artifact failing static validation must refuse \
         publication:\n{out}"
    );
    assert!(
        out.contains("static: cgov-linux-arm64 failed"),
        "refusal must attribute the failure to the arm64 artifact:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn unexecutable_artifact_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // Strip the exec bit from the amd64 artifact (claudego-eb2394ce). The
    // bytes are untouched, so the sidecar digest stays correct and the
    // gate's own existence check passes — only the validator's
    // executable-regular-file check stands between this release and
    // publication. amd64 is broken here because the sibling test above
    // breaks arm64: together the two pin "on any architecture".
    let amd64 = sb.release_dir.join("cgov-linux-amd64");
    fs::set_permissions(&amd64, fs::Permissions::from_mode(0o644)).expect("chmod 0644");
    let sidecar = fs::read_to_string(sb.release_dir.join("cgov-linux-amd64.sha256"))
        .expect("read sidecar");
    assert_eq!(
        sidecar.split_whitespace().next().unwrap(),
        sha256_hex(&amd64),
        "chmod must not disturb the bytes: the sidecar still matches, so \
         the refusal below is attributable to executability alone"
    );

    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "an artifact without its exec bit must refuse publication:\n{out}"
    );
    assert!(
        out.contains("not an executable regular file"),
        "the validator's diagnosis must surface through the gate:\n{out}"
    );
    assert!(
        out.contains("static: cgov-linux-amd64 failed"),
        "refusal must attribute the failure to the amd64 artifact:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn missing_foreign_architecture_artifact_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    fs::remove_file(sb.release_dir.join("cgov-linux-arm64")).expect("remove arm64 artifact");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "a missing architecture must refuse publication — no partial releases:\n{out}"
    );
    assert!(
        out.contains("every supported architecture"),
        "refusal must state the every-architecture rule:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn missing_host_architecture_artifact_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // The mirror of missing_foreign_architecture_artifact: the build host's
    // OWN architecture is the absent one. amd64 heads the gate's artifact
    // list, so this is the loop's first iteration — a partial release is a
    // refusal whichever architecture is missing, including the one the
    // validating host could have run itself.
    fs::remove_file(sb.release_dir.join("cgov-linux-amd64")).expect("remove amd64 artifact");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "a missing host architecture must refuse publication — no partial \
         releases:\n{out}"
    );
    assert!(
        out.contains("cgov-linux-amd64 is missing"),
        "refusal must name the absent amd64 artifact:\n{out}"
    );
    assert!(
        out.contains("every supported architecture"),
        "refusal must state the every-architecture rule:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn tag_not_pointing_at_head_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // Move HEAD past the tagged commit: the artifacts were built from a
    // commit the tag no longer names.
    fs::write(sb.release_dir.join("README.md"), "post-tag work\n").expect("dirty the tree");
    git(&sb.release_dir, &["commit", "-qam", "post-tag work"]);
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "a tag pointing at an older commit must refuse publication:\n{out}"
    );
    assert!(
        out.contains("points at"),
        "refusal must show the tag/HEAD divergence:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn tag_absent_from_forgejo_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // The tag exists locally at HEAD but not on Forgejo (someone deleted it
    // remotely, or never pushed it): "built from the Forgejo tag" cannot be
    // proven, so nothing may publish.
    delete_tag_on_standin(&sb);
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "a tag missing from Forgejo must refuse publication:\n{out}"
    );
    assert!(
        out.contains("is not on Forgejo"),
        "refusal must name the Forgejo gap:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn forgejo_tag_pointing_elsewhere_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // The branch the module header calls distinct from an unreachable
    // Forgejo: the source of truth was REACHED and its tag names a
    // different commit than the one the artifacts were built from — a
    // Forgejo-side tag re-cut onto a divergent commit. Every local check
    // passes (the checkout's tag still points exactly at HEAD), so only
    // Forgejo's own answer can expose that the release would not be built
    // from the commit its Forgejo tag names.
    git(&sb.release_dir, &["checkout", "-q", "-b", "divergent"]);
    fs::write(sb.release_dir.join("README.md"), "a divergent commit\n").expect("dirty the tree");
    git(&sb.release_dir, &["commit", "-qam", "divergent commit"]);
    let divergent = git_out(&sb.release_dir, &["rev-parse", "HEAD"]);
    git(&sb.release_dir, &["checkout", "-q", "main"]);
    git(&sb.release_dir, &["push", "-q", "origin", "divergent"]);
    move_tag_on_standin(&sb, &divergent);

    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "a Forgejo tag naming another commit must refuse publication:\n{out}"
    );
    assert!(
        out.contains("not the built commit"),
        "refusal must name the Forgejo/HEAD divergence (distinct from the \
         local tag check's wording):\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn annotated_forgejo_tag_proves_provenance_through_the_peel() {
    let sb = sandbox_with_good_release();
    // Releases can be cut with annotated tags: ls-remote then lists TWO
    // entries, `refs/tags/vX.Y.Z` — whose sha is the TAG OBJECT, not a
    // commit — and `refs/tags/vX.Y.Z^{}`, the peeled commit. Only the peel
    // can equal the built HEAD, so provenance that compared the raw entry
    // would refuse every correctly-cut annotated release. Recut the
    // sandbox tag as annotated and pin the peel as the answer.
    git(&sb.release_dir, &["tag", "-d", TAG]);
    delete_tag_on_standin(&sb);
    git(&sb.release_dir, &["tag", "-a", TAG, "-m", "Claude Governor v0.1.2"]);
    git(
        &sb.release_dir,
        &["push", "-q", "origin", &format!("refs/tags/{TAG}")],
    );

    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        ok,
        "an annotated Forgejo tag at the built commit must publish:\n{out}"
    );
    assert!(
        out.contains("Forgejo resolves"),
        "provenance must pass on Forgejo's own peeled resolution:\n{out}"
    );
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "the annotated-tag release publishes exactly once:\n{out}"
    );
}

#[test]
fn wrong_origin_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // Re-point origin at the GitHub mirror: publication may only be proven
    // against the Forgejo source of truth.
    git(
        &sb.release_dir,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/jedarden/claude-governor.git",
        ],
    );
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        !ok,
        "origin must be the Forgejo source of truth, not the mirror:\n{out}"
    );
    assert!(
        out.contains("not the Forgejo source of truth"),
        "refusal must name the origin problem:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn post_publish_sidecar_gap_fails_the_run() {
    let sb = sandbox_with_good_release();
    // The release was cut, but the arm64 sidecar never landed (upload
    // failure, manual edit...). The create has already happened, so the
    // gate cannot un-publish — it must still fail the CI run loudly.
    let incomplete = r#"{"assets":[{"name":"cgov-linux-amd64","size":150},{"name":"cgov-linux-amd64.sha256","size":69},{"name":"cgov-linux-arm64","size":150}]}"#;
    let (ok, out) = run_gate(&sb, incomplete, &[]);
    assert!(
        !ok,
        "a published release with an unpaired asset must fail the run:\n{out}"
    );
    assert!(
        out.contains("has no cgov-linux-arm64.sha256"),
        "failure must name the missing sidecar asset:\n{out}"
    );
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "the create itself did happen in this scenario:\n{out}"
    );
    assert!(
        out.contains("release is live but incomplete"),
        "the message must say the live release needs fixing:\n{out}"
    );
}

#[test]
fn bare_version_is_normalized_to_the_release_tag() {
    let sb = sandbox_with_good_release();
    let assets_file = sb.dir.path().join("assets.json");
    fs::write(&assets_file, good_assets_json()).expect("write assets scenario");
    let fake_bin = sb.dir.path().join("bin");
    let path = std::env::var("PATH").unwrap_or_default();
    let out = Command::new("bash")
        .arg(sb.dir.path().join("scripts/publish-release.sh"))
        .args(["--version", "0.1.2", "--release-dir"])
        .arg(&sb.release_dir)
        .env("CGOV_FORGEJO_URL", &sb.origin_url)
        .env("CGOV_FAKE_GH_ASSETS", &assets_file)
        .env("PATH", format!("{}:{}", fake_bin.display(), path))
        .env_remove("CGOV_GH_REPO")
        .output()
        .expect("spawn bash on the sandboxed gate");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "bare 0.1.2 must normalize:\n{text}");
    let creates = create_calls(&sb);
    assert_eq!(creates.len(), 1, "one create after normalization:\n{text}");
    assert!(
        creates[0].contains(TAG),
        "gh release create must target the normalized tag:\n{}",
        creates[0]
    );
}

// ---------------------------------------------------------------------------
// The refusal family, complete: every remaining `die` branch
// (claudego-719c1248)
// ---------------------------------------------------------------------------

/// A directory of symlinks to exactly the executables the gate and its
/// static validator invoke before the publish phase, minus the tools named
/// in `omit` — the lever that builds a PATH missing `gh` (the publish-phase
/// refusal) or missing the foreign-architecture emulators (the skipped-probe
/// refusal). Host tools resolve via `command -v` so the sandbox works
/// whatever the host layout is (a tool the host lacks, like `file(1)`, is
/// simply absent, exactly as the validator expects when it downgrades the
/// file(1) cross-check); the sandbox's own doubles (`gh`, the qemu fakes)
/// are symlinked from the sandbox `bin/` unless omitted, so a stripped PATH
/// differs from the happy path only in the named omission.
fn gate_tools_bin(sb: &Sandbox, dest_name: &str, omit: &[&str]) -> PathBuf {
    let bin = sb.dir.path().join(dest_name);
    fs::create_dir_all(&bin).expect("create stripped bin dir");
    let sandbox_bin = sb.dir.path().join("bin");
    let mut tools: Vec<String> = [
        "bash", "git", "dirname", "basename", "sed", "awk", "grep", "cut",
        "head", "tr", "cat", "mktemp", "sha256sum", "env", "uname",
        "readlink", "readelf", "objdump", "file", "rm",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    // The sandbox doubles: gh plus an emulator double for either
    // architecture — whichever artifact is foreign on this host is the one
    // whose probes need it.
    tools.push("gh".into());
    tools.push("qemu-aarch64-static".into());
    tools.push("qemu-x86_64-static".into());
    for tool in tools {
        if omit.contains(&tool.as_str()) {
            continue;
        }
        let target = if tool == "gh" || tool.starts_with("qemu-") {
            sandbox_bin.join(&tool)
        } else {
            let found = Command::new("sh")
                .args(["-c", &format!("command -v {tool}")])
                .output()
                .expect("probe the host for a gate tool");
            if !found.status.success() {
                continue; // optional to the gate's pre-publish path on this host
            }
            PathBuf::from(String::from_utf8_lossy(&found.stdout).trim())
        };
        std::os::unix::fs::symlink(&target, bin.join(&tool))
            .expect("symlink a gate tool into the stripped bin dir");
    }
    bin
}

/// A PATH whose bin dir holds the gate tools and the fake gh but NO emulator
/// for the foreign architecture, plus a pinned-empty binfmt_misc dir: the
/// validator has no way to execute the foreign artifact and must skip its
/// probe — loudly — exactly the state production CI must never publish from.
/// BINFMT_MISC_DIR is pinned (a validator test hook) so the outcome never
/// depends on the host's real registrations.
fn stripped_of_emulators(sb: &Sandbox) -> (PathBuf, PathBuf) {
    let bin = gate_tools_bin(
        sb,
        "bin-no-qemu",
        &[
            "qemu-aarch64-static",
            "qemu-aarch64",
            "qemu-x86_64-static",
            "qemu-x86_64",
        ],
    );
    let binfmt_empty = sb.dir.path().join("binfmt-empty");
    fs::create_dir_all(&binfmt_empty).expect("empty binfmt dir");
    (bin, binfmt_empty)
}

#[test]
fn absent_version_argument_is_a_usage_refusal() {
    let sb = sandbox_with_good_release();
    // No --version at all: usage, exit 2 — the gate refuses before touching
    // the checkout, Forgejo, or gh. Exit 2, not 1, is part of the contract:
    // CI can tell a misconfigured invocation from a failed validation.
    let (code, out) = run_gate_args(&sb, &good_assets_json(), &[], &[]);
    assert_eq!(code, 2, "a bare invocation is usage, not validation:\n{out}");
    assert!(
        gh_log_lines(&sb).is_empty(),
        "usage errors must make no gh call at all"
    );
}

#[test]
fn unknown_argument_is_a_usage_refusal() {
    let sb = sandbox_with_good_release();
    let (code, out) = run_gate_args(&sb, &good_assets_json(), &["--frobnicate"], &[]);
    assert_eq!(code, 2, "an unrecognized flag is usage, not validation:\n{out}");
    assert!(
        gh_log_lines(&sb).is_empty(),
        "usage errors must make no gh call at all"
    );
}

#[test]
fn version_without_a_value_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // A dangling --version is distinct from a missing one: die (exit 1),
    // not usage (exit 2).
    let (code, out) = run_gate_args(&sb, &good_assets_json(), &["--version"], &[]);
    assert_eq!(code, 1, "a dangling --version dies, it does not print usage:\n{out}");
    assert!(
        out.contains("--version requires a value"),
        "refusal must name the dangling flag:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "argument errors must make no gh call at all"
    );
}

#[test]
fn malformed_version_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // "1.2" is normalized to "v1.2" and THEN rejected: bare versions may
    // grow a v, but partial versions may not pass as releases.
    let (code, out) = run_gate_args(&sb, &good_assets_json(), &["--version", "1.2"], &[]);
    assert_eq!(code, 1, "a partial version must refuse publication:\n{out}");
    assert!(
        out.contains("must be a vX.Y.Z release tag"),
        "refusal must state the tag shape:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "argument errors must make no gh call at all"
    );
}

#[test]
fn nonexistent_release_dir_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    let missing = sb.dir.path().join("no-such-release-dir");
    let (ok, out) = run_gate(
        &sb,
        &good_assets_json(),
        &["--release-dir", missing.to_str().expect("release dir path")],
    );
    assert!(!ok, "a nonexistent release dir must refuse publication:\n{out}");
    assert!(
        out.contains("--release-dir does not exist"),
        "refusal must name the missing dir:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "argument errors must make no gh call at all"
    );
}

#[test]
fn release_dir_that_is_not_a_git_checkout_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // A plain directory with artifacts but no git history: provenance
    // cannot even be asked, let alone answered. The parser keeps the last
    // --release-dir, so this overrides the sandbox default.
    let plain = sb.dir.path().join("not-a-checkout");
    fs::create_dir_all(&plain).expect("plain non-checkout dir");
    let (ok, out) = run_gate(
        &sb,
        &good_assets_json(),
        &["--release-dir", plain.to_str().expect("plain dir path")],
    );
    assert!(!ok, "a non-checkout release dir must refuse publication:\n{out}");
    assert!(
        out.contains("is not a git checkout"),
        "refusal must name the missing git history:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "preflight failure must make no gh call at all"
    );
}

#[test]
fn checkout_without_origin_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // No origin remote at all: there is nothing to compare against the
    // Forgejo source of truth, so provenance is unprovable.
    git(&sb.release_dir, &["remote", "remove", "origin"]);
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(!ok, "an originless checkout must refuse publication:\n{out}");
    assert!(
        out.contains("has no origin remote"),
        "refusal must name the missing remote:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "preflight failure must make no gh call at all"
    );
}

#[test]
fn local_tag_absent_from_the_checkout_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // The mirror image of tag_absent_from_forgejo: Forgejo still has the
    // tag, but this checkout does not. The gate resolves the tag locally
    // first, so the refusal fires before any Forgejo query — and the
    // remote's good opinion cannot rescue a checkout that cannot name the
    // commit its artifacts came from.
    git(&sb.release_dir, &["tag", "-d", TAG]);
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(!ok, "a checkout without the tag must refuse publication:\n{out}");
    assert!(
        out.contains("does not exist in the checkout"),
        "refusal must distinguish the local gap from the remote one:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "preflight failure must make no gh call at all"
    );
}

#[test]
fn unreachable_forgejo_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // A Forgejo that answers with the wrong commit is refused elsewhere;
    // here the source of truth cannot be reached AT ALL. Both origin and
    // CGOV_FORGEJO_URL point at a file:// URL whose repository does not
    // exist, so the origin canon check still passes and `git ls-remote`
    // fails instantly and deterministically — no network, no timeout.
    let dead = format!("file://{}/forgejo-unreachable.git", sb.dir.path().display());
    git(&sb.release_dir, &["remote", "set-url", "origin", &dead]);
    let (code, out) = run_gate_raw(
        &sb,
        &good_assets_json(),
        &[],
        &[("CGOV_FORGEJO_URL", dead)],
    );
    assert_eq!(code, 1, "an unreachable Forgejo must refuse publication:\n{out}");
    assert!(
        out.contains("cannot reach Forgejo"),
        "refusal must distinguish unreachable from divergent:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "preflight failure must make no gh call at all"
    );
}

#[test]
fn missing_static_validator_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // The gate checks sidecars but never generates them, and it validates
    // linkage through the shipped sibling script — a sandbox without that
    // script cannot fall back to trusting the artifacts.
    fs::remove_file(sb.dir.path().join("scripts/verify-release-static.sh"))
        .expect("remove the static validator");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(!ok, "a missing validator must refuse publication:\n{out}");
    assert!(
        out.contains("static validator missing"),
        "refusal must name the missing validator:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "preflight failure must make no gh call at all"
    );
}

#[test]
fn missing_gh_cli_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // Every preflight phase passes, then `command -v gh` fails: the gate
    // must refuse rather than quietly skip the publish step and exit 0 —
    // a green run that published nothing would be worse than a red one.
    // The emulator doubles stay on PATH so the static phase passes and the
    // refusal is attributable to the missing gh alone.
    let bin = gate_tools_bin(&sb, "bin-no-gh", &["gh"]);
    let (code, out) = run_gate_raw(
        &sb,
        &good_assets_json(),
        &[],
        &[("PATH", bin.to_string_lossy().into_owned())],
    );
    assert_eq!(code, 1, "a missing gh must fail the run:\n{out}");
    assert!(
        out.contains("gh (GitHub CLI) is required"),
        "refusal must name the missing tool:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "no gh call can have been made without the binary"
    );
}

#[test]
fn failed_gh_create_fails_the_run_and_stops_before_the_asset_query() {
    let sb = sandbox_with_good_release();
    // Every preflight check passes, but `gh release create` itself fails
    // (expired auth, a tag race, an API outage). The gate must fail the
    // run, and the post-publish asset query must not run against a release
    // that was never created — exactly one gh call, the create, may land.
    let (code, out) = run_gate_raw(
        &sb,
        &good_assets_json(),
        &[],
        &[("CGOV_FAKE_GH_CREATE_RC", "1".to_string())],
    );
    assert_eq!(code, 1, "a failed create must fail the run:\n{out}");
    assert!(
        out.contains("gh release create failed"),
        "failure must name the failed create:\n{out}"
    );
    let calls = gh_log_lines(&sb);
    assert_eq!(
        calls.len(),
        1,
        "the create was attempted once and nothing may follow it:\n{out}\ncalls: {calls:?}"
    );
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "the single gh call was the create itself:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// The skipped foreign-architecture execution probe (claudego-7c747ffb)
// ---------------------------------------------------------------------------

#[test]
fn skipped_foreign_probe_refuses_and_never_publishes() {
    let sb = sandbox_with_good_release();
    // Everything else is perfect — Forgejo provenance, both artifacts, both
    // sidecars — but nothing on this host can EXECUTE the foreign artifact:
    // no emulator on the (stripped) PATH and no binfmt_misc registration.
    // The validator still exits 0, downgrading to linkage-only evidence with
    // a loud note; the gate must convert that downgrade into a refusal,
    // because a probe that never ran proves nothing about the bytes that
    // would ship. This is the property cgov-ci's grep enforced from outside
    // the gate; the gate now enforces it itself, before any gh call.
    let (bin, binfmt_empty) = stripped_of_emulators(&sb);
    let (code, out) = run_gate_raw(
        &sb,
        &good_assets_json(),
        &[],
        &[
            ("PATH", bin.to_string_lossy().into_owned()),
            (
                "BINFMT_MISC_DIR",
                binfmt_empty.to_string_lossy().into_owned(),
            ),
        ],
    );
    assert_eq!(
        code, 1,
        "a skipped foreign probe must refuse publication:\n{out}"
    );
    assert!(
        out.contains("EXECUTION PROBE SKIPPED"),
        "the validator's loud skip note must reach the gate output:\n{out}"
    );
    assert!(
        out.contains("execution probe was skipped"),
        "refusal must attribute the run's failure to the skipped probe:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "a skipped-probe refusal must make no gh call at all"
    );
}

#[test]
fn skipped_foreign_probe_publishes_only_under_the_explicit_override() {
    let sb = sandbox_with_good_release();
    // The escape hatch is deliberate and loud: the identical no-way-to-run
    // host with CGOV_ALLOW_SKIPPED_PROBE=1 publishes on linkage evidence
    // alone, and the output must carry the override note so a
    // linkage-only release can never pass silently.
    let (bin, binfmt_empty) = stripped_of_emulators(&sb);
    let (code, out) = run_gate_raw(
        &sb,
        &good_assets_json(),
        &[],
        &[
            ("PATH", bin.to_string_lossy().into_owned()),
            (
                "BINFMT_MISC_DIR",
                binfmt_empty.to_string_lossy().into_owned(),
            ),
            ("CGOV_ALLOW_SKIPPED_PROBE", "1".to_string()),
        ],
    );
    assert_eq!(code, 0, "the override must let the run publish:\n{out}");
    assert!(
        out.contains("CGOV_ALLOW_SKIPPED_PROBE=1 accepts linkage-only evidence"),
        "the override must be loudly noted in the output:\n{out}"
    );
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "the run went on to publish exactly once:\n{out}"
    );
    assert!(
        out.contains("publish-release: OK"),
        "the overridden run completes like any other:\n{out}"
    );
}

#[test]
fn gate_surfaces_the_emulated_foreign_probe_before_publishing() {
    let sb = sandbox_with_good_release();
    // The positive control for the refusal above: with a way to run the
    // foreign artifact — the sandbox's emulator double here, the
    // qemu-user-static package in production — the validator reports both
    // probes UNDER THE EMULATOR and the gate publishes. The skip refusal is
    // wired to genuine skips only, not to emulation or foreignness itself.
    let m = foreign_machine();
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[]);
    assert!(
        ok,
        "a release whose foreign probe ran under emulation must publish:\n{out}"
    );
    for probe in ["--version", "--help"] {
        assert!(
            out.contains(&format!("{probe} under qemu-{m}-static in env -i")),
            "the gate output must show the foreign {probe} ran under the \
             emulator, not merely that the validator exited 0:\n{out}"
        );
    }
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "the fully-executed release publishes exactly once:\n{out}"
    );
}
