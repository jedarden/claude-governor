//! Release provenance and asset-pairing gates for `scripts/publish-release.sh`
//! (claudego-3f7b7f49) — the coverage `tests/release_publication_gate_test.rs`
//! (claudego-78c221be, commit b37de77) deliberately left open. That file pins
//! the headline refusals; this one pins the branches it does not reach:
//!
//! * **tag mismatch, remote side** — Forgejo itself resolving `vX.Y.Z` to a
//!   commit that is not the built HEAD (a re-pushed or re-pointed remote tag
//!   with a perfectly consistent local checkout) must refuse; and the
//!   annotated-tag path must be accepted, because `ls-remote` answers with a
//!   raw tag-object sha plus a peeled `^{}` entry and the gate must compare
//!   the peeled one;
//! * **missing architecture, host side** — `cgov-linux-amd64` absent must
//!   refuse just like the foreign architecture (the every-architecture rule
//!   applies to the first entry of the artifact list too, not only the last);
//! * **invalid sidecar shape, hex case** — a 64-character digest that is not
//!   lowercase hex (`sha256sum` never emits uppercase, so a hand-mangled or
//!   foreign-tool sidecar) must refuse at the format check, not at the
//!   digest comparison;
//! * **unpaired uploaded assets, every direction** — a published `.sha256`
//!   whose binary peer is missing, an asset list with nothing on it at all,
//!   and an asset listing that cannot be fetched must each fail the run
//!   after the release is live, loudly;
//! * **preflight happens before upload** — a sweep over every preflight
//!   refusal scenario (this file's and the sibling's) asserting the `gh`
//!   fake received NO call of any kind, not merely no `release create`.
//!
//! The sandbox is the sibling file's design, restated: both shipped scripts
//! `include_str!`-embedded at compile time (close-gate extraction safe), a
//! throwaway checkout whose `origin` is a local bare repo standing in for
//! Forgejo, hand-assembled static ELF64 artifacts that genuinely pass
//! `scripts/verify-release-static.sh`, and a recording `gh` fake whose
//! `release view` answers from a scenario file (and can be told to fail).
//! Nothing here reaches the network.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// The shipped publication gate, embedded at compile time.
const PUBLISH_SH: &str = include_str!("../scripts/publish-release.sh");

/// The shipped static validator, embedded because the gate invokes it as a
/// sibling file and the sandbox has to reproduce that layout.
const STATIC_CHECK_SH: &str = include_str!("../scripts/verify-release-static.sh");

const TAG: &str = "v0.1.2";

// ---------------------------------------------------------------------------
// Hand-assembled static ELFs (mirrors tests/release_publication_gate_test.rs)
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
// Sandbox: git checkout + Forgejo stand-in + recording gh fake
// ---------------------------------------------------------------------------

/// How the release tag is cut in the sandbox checkout. Forgejo serves both
/// shapes and the gate must read the peeled `^{}` entry for annotated tags.
#[derive(Clone, Copy)]
enum TagStyle {
    Lightweight,
    Annotated,
}

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
            "user.email=claudego-3f7b7f49@test",
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

/// Like [`git`] but returns trimmed stdout — for plumbing commands whose
/// output the test feeds onward (commit-tree shas).
fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=claudego-3f7b7f49@test",
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
}

fn sha256_hex(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
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

/// Build the full sandbox: both scripts materialized as siblings under
/// `scripts/`, a one-commit git checkout whose `origin` is a local bare repo
/// (the Forgejo stand-in), `TAG` cut in `style` at HEAD and pushed to the
/// stand-in, and a `bin/gh` fake that appends its argv to the log file and
/// answers `release view --json assets` from the `CGOV_FAKE_GH_ASSETS`
/// scenario file — unless `CGOV_FAKE_GH_VIEW_RC` is non-zero, in which case
/// the view call itself fails (the unreachable-asset-listing scenario).
/// The release dir starts with both artifacts and matching sidecars.
fn sandbox(style: TagStyle) -> Sandbox {
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
    // gets the linkage checks with its probe skipped).
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
    match style {
        TagStyle::Lightweight => git(&release_dir, &["tag", TAG]),
        TagStyle::Annotated => git(&release_dir, &["tag", "-a", TAG, "-m", "sandbox release"]),
    }
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
# the post-publish asset query from a scenario file (or fails it, when
# CGOV_FAKE_GH_VIEW_RC is non-zero, simulating a listing that cannot be
# fetched).
{{ printf '%s' "$*" | tr '\n' ' '; printf '\n'; }} >> "{log_display}"
case " $1 $2 " in
  *" release create "*) exit 0 ;;
  *" release view "*)
    [ "${{CGOV_FAKE_GH_VIEW_RC:-0}}" -eq 0 ] || exit "${{CGOV_FAKE_GH_VIEW_RC}}"
    cat "${{CGOV_FAKE_GH_ASSETS:?}}"; exit 0 ;;
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
    }
}

fn sandbox_with_good_release() -> Sandbox {
    sandbox(TagStyle::Lightweight)
}

fn good_assets_json() -> String {
    r#"{"assets":[{"name":"cgov-linux-amd64","size":150},{"name":"cgov-linux-amd64.sha256","size":69},{"name":"cgov-linux-arm64","size":150},{"name":"cgov-linux-arm64.sha256","size":69}]}"#
        .to_string()
}

/// Run the materialized gate. `PATH` is prefixed with the sandbox `bin/`
/// (the fake gh), `CGOV_FORGEJO_URL` points provenance at the stand-in.
fn run_gate(
    sb: &Sandbox,
    assets_json: &str,
    extra_args: &[&str],
    extra_envs: &[(&str, String)],
) -> (bool, String) {
    let assets_file = sb.dir.path().join("assets.json");
    fs::write(&assets_file, assets_json).expect("write fake gh assets scenario");
    let fake_bin = sb.dir.path().join("bin");
    let path = std::env::var("PATH").unwrap_or_default();
    let mut cmd = Command::new("bash");
    cmd.arg(sb.dir.path().join("scripts/publish-release.sh"))
        .args(["--version", TAG, "--release-dir"])
        .arg(&sb.release_dir)
        .args(extra_args)
        .env("CGOV_FORGEJO_URL", &sb.origin_url)
        .env("CGOV_FAKE_GH_ASSETS", &assets_file)
        .env("PATH", format!("{}:{}", fake_bin.display(), path))
        .env_remove("CGOV_GH_REPO")
        .env_remove("CGOV_FAKE_GH_VIEW_RC");
    for (k, v) in extra_envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn bash on the sandboxed gate");
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

fn create_calls(sb: &Sandbox) -> Vec<String> {
    gh_log_lines(sb)
        .into_iter()
        .filter(|l| {
            l.split_whitespace().take(2).collect::<Vec<_>>() == ["release", "create"]
        })
        .collect()
}

/// Path of the bare Forgejo stand-in inside the sandbox.
fn standin(sb: &Sandbox) -> PathBuf {
    sb.dir.path().join("forgejo-standin.git")
}

/// Re-point the stand-in's `TAG` at a commit that is NOT the built HEAD: a
/// sibling commit object is synthesized with commit-tree (same tree, so the
/// release dir stays untouched), pushed to the stand-in as a side branch,
/// and the remote tag ref is moved onto it. The local checkout and its tag
/// remain perfectly consistent — only Forgejo diverges, exactly like a
/// re-pushed remote tag.
fn retag_standin_at_divergent_commit(sb: &Sandbox) {
    let divergent = git_out(
        &sb.release_dir,
        &["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "divergent"],
    );
    let push_src = format!("{divergent}:refs/heads/divergence");
    git(&sb.release_dir, &["push", "-q", "origin", &push_src]);
    let tag_ref = format!("refs/tags/{TAG}");
    git(&standin(sb), &["update-ref", &tag_ref, &divergent]);
}

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

#[test]
fn forgejo_tag_divergence_refuses_and_never_uploads() {
    let sb = sandbox_with_good_release();
    // Local tag == HEAD == what was built; Forgejo's tag points elsewhere.
    // A purely local tag proves nothing, and neither does a local match —
    // the remote resolution is the provenance that matters.
    retag_standin_at_divergent_commit(&sb);
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[], &[]);
    assert!(
        !ok,
        "Forgejo resolving the tag to a different commit must refuse:\n{out}"
    );
    assert!(
        out.contains("Forgejo's v0.1.2 points at"),
        "refusal must name Forgejo's divergent resolution:\n{out}"
    );
    assert!(
        out.contains("not the built commit"),
        "refusal must contrast the remote commit with the built one:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "a provenance refusal must reach no gh call at all — nothing may be \
         uploaded before every preflight phase passes"
    );
}

#[test]
fn annotated_forgejo_tag_publishes_with_paired_assets() {
    let sb = sandbox(TagStyle::Annotated);
    // ls-remote answers an annotated tag with a raw tag-object sha AND a
    // peeled ^{} entry; the gate must compare the peeled commit, not the
    // tag object, or every annotated-tag release would be refused.
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[], &[]);
    assert!(ok, "an annotated tag at the built commit must publish:\n{out}");
    assert!(
        out.contains("Forgejo resolves v0.1.2 to the built commit"),
        "the peeled remote resolution must be proven:\n{out}"
    );
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "exactly one gh release create:\n{out}"
    );
    assert!(
        out.contains("publish-release: OK"),
        "the run must finish with the paired-assets confirmation:\n{out}"
    );
}

#[test]
fn missing_host_architecture_artifact_refuses_and_never_uploads() {
    let sb = sandbox_with_good_release();
    // The symmetric gap: the host architecture's own artifact missing. The
    // every-architecture rule must hold for the FIRST entry of the artifact
    // list too — the loop may not stop checking after one good entry.
    fs::remove_file(sb.release_dir.join("cgov-linux-amd64")).expect("remove amd64 artifact");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[], &[]);
    assert!(
        !ok,
        "a missing host architecture must refuse publication — no partial \
         releases:\n{out}"
    );
    assert!(
        out.contains("cgov-linux-amd64 is missing"),
        "refusal must name the amd64 artifact:\n{out}"
    );
    assert!(
        out.contains("every supported architecture"),
        "refusal must state the every-architecture rule:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "no gh call of any kind may happen before the artifact phase passes"
    );
}

#[test]
fn non_lowercase_digest_sidecar_refuses_and_never_uploads() {
    let sb = sandbox_with_good_release();
    // The true digest, uppercased: 64 hex characters, well-formed line,
    // wrong case. `sha256sum -c` tolerates it, but the gate's format
    // contract is stricter than what would merely work — it must fail at
    // the 64-hex-lowercase check, not reach the digest comparison.
    let digest = sha256_hex(&sb.release_dir.join("cgov-linux-arm64"));
    fs::write(
        sb.release_dir.join("cgov-linux-arm64.sha256"),
        sidecar_bytes(&digest.to_uppercase(), "cgov-linux-arm64"),
    )
    .expect("write uppercase-digest sidecar");
    let (ok, out) = run_gate(&sb, &good_assets_json(), &[], &[]);
    assert!(
        !ok,
        "a non-lowercase digest sidecar must refuse publication:\n{out}"
    );
    assert!(
        out.contains("does not carry a 64-hex lowercase sha256 digest"),
        "refusal must name the hex-case format violation:\n{out}"
    );
    assert!(
        gh_log_lines(&sb).is_empty(),
        "no gh call of any kind may happen before the sidecar phase passes"
    );
}

#[test]
fn orphaned_published_sidecar_fails_the_run() {
    let sb = sandbox_with_good_release();
    // The reverse pairing direction: the release carries the arm64 SIDECAR
    // but its binary never landed. The README promises unpaired assets fail
    // "in either direction" — an orphan digest is as useless as a digest-less
    // binary, and install.sh on arm64 would find no artifact to verify.
    let orphaned = r#"{"assets":[{"name":"cgov-linux-amd64","size":150},{"name":"cgov-linux-amd64.sha256","size":69},{"name":"cgov-linux-arm64.sha256","size":69}]}"#;
    let (ok, out) = run_gate(&sb, orphaned, &[], &[]);
    assert!(
        !ok,
        "a published sidecar with no binary peer must fail the run:\n{out}"
    );
    assert!(
        out.contains("sidecar cgov-linux-arm64.sha256 has no cgov-linux-arm64 asset"),
        "failure must name the orphaned sidecar and its missing peer:\n{out}"
    );
    assert!(
        out.contains("release is live but incomplete"),
        "the message must say the live release needs fixing:\n{out}"
    );
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "the create itself did happen in this scenario:\n{out}"
    );
}

#[test]
fn empty_published_asset_list_fails_the_run() {
    let sb = sandbox_with_good_release();
    // The degenerate asset list: the release exists but carries nothing at
    // all (every upload failed, or the create was raced). An empty list is
    // not "everything paired" — it must fail before the pairing loop can
    // misread vacancy as success.
    let (ok, out) = run_gate(&sb, r#"{"assets":[]}"#, &[], &[]);
    assert!(
        !ok,
        "a published release with no assets must fail the run:\n{out}"
    );
    assert!(
        out.contains("has no assets"),
        "failure must state the release is asset-less:\n{out}"
    );
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "the create itself did happen in this scenario:\n{out}"
    );
}

#[test]
fn unreachable_asset_listing_fails_the_run() {
    let sb = sandbox_with_good_release();
    // gh release view itself fails (auth, network, rate limit): the pairing
    // check cannot be skipped silently just because it could not run.
    let (ok, out) = run_gate(
        &sb,
        &good_assets_json(),
        &[],
        &[("CGOV_FAKE_GH_VIEW_RC", "1".to_string())],
    );
    assert!(
        !ok,
        "an asset listing that cannot be fetched must fail the run:\n{out}"
    );
    assert!(
        out.contains("cannot list assets"),
        "failure must name the unreachable asset listing:\n{out}"
    );
    assert_eq!(
        create_calls(&sb).len(),
        1,
        "the create itself did happen in this scenario:\n{out}"
    );
}

/// The bead's headline claim, stated once for every preflight refusal at
/// once: whichever phase rejects the release — provenance (clone origin,
/// local tag placement, remote tag absence, remote tag divergence),
/// artifacts (either architecture), or sidecars (missing, tampered,
/// mis-named, malformed, wrong case) — the `gh` binary must never have been
/// invoked AT ALL. "Before upload" means zero gh calls, not merely zero
/// `release create` calls.
/// One preflight-refusal scenario for the sweep: a name and the mutation
/// that breaks an otherwise-good sandbox.
type Refusal = (&'static str, Box<dyn Fn(&Sandbox)>);

#[test]
fn every_preflight_refusal_makes_no_gh_call_at_all() {
    let refusals: Vec<Refusal> = vec![
        (
            "origin repointed at the GitHub mirror",
            Box::new(|sb| {
                git(
                    &sb.release_dir,
                    &[
                        "remote",
                        "set-url",
                        "origin",
                        "https://github.com/jedarden/claude-governor.git",
                    ],
                );
            }),
        ),
        (
            "tag pointing at an older commit than HEAD",
            Box::new(|sb| {
                fs::write(sb.release_dir.join("README.md"), "post-tag work\n")
                    .expect("dirty the tree");
                git(&sb.release_dir, &["commit", "-qam", "post-tag work"]);
            }),
        ),
        (
            "tag deleted on Forgejo",
            Box::new(|sb| {
                let tag_ref = format!("refs/tags/{TAG}");
                git(&standin(sb), &["update-ref", "-d", &tag_ref]);
            }),
        ),
        (
            "Forgejo tag re-pointed at a different commit",
            Box::new(retag_standin_at_divergent_commit),
        ),
        (
            "missing host-architecture artifact",
            Box::new(|sb| {
                fs::remove_file(sb.release_dir.join("cgov-linux-amd64"))
                    .expect("remove amd64 artifact");
            }),
        ),
        (
            "missing foreign-architecture artifact",
            Box::new(|sb| {
                fs::remove_file(sb.release_dir.join("cgov-linux-arm64"))
                    .expect("remove arm64 artifact");
            }),
        ),
        (
            "missing sidecar",
            Box::new(|sb| {
                fs::remove_file(sb.release_dir.join("cgov-linux-arm64.sha256"))
                    .expect("remove sidecar");
            }),
        ),
        (
            "tampered sidecar digest",
            Box::new(|sb| {
                fs::write(
                    sb.release_dir.join("cgov-linux-arm64.sha256"),
                    sidecar_bytes(&"0".repeat(64), "cgov-linux-arm64"),
                )
                .expect("write tampered sidecar");
            }),
        ),
        (
            "sidecar naming the wrong artifact",
            Box::new(|sb| {
                let amd64_digest = sha256_hex(&sb.release_dir.join("cgov-linux-amd64"));
                fs::write(
                    sb.release_dir.join("cgov-linux-arm64.sha256"),
                    sidecar_bytes(&amd64_digest, "cgov-linux-amd64"),
                )
                .expect("write misnamed sidecar");
            }),
        ),
        (
            "malformed sidecar shape",
            Box::new(|sb| {
                fs::write(sb.release_dir.join("cgov-linux-arm64.sha256"), "deadbeef\n")
                    .expect("write malformed sidecar");
            }),
        ),
        (
            "non-lowercase digest sidecar",
            Box::new(|sb| {
                let digest = sha256_hex(&sb.release_dir.join("cgov-linux-arm64"));
                fs::write(
                    sb.release_dir.join("cgov-linux-arm64.sha256"),
                    sidecar_bytes(&digest.to_uppercase(), "cgov-linux-arm64"),
                )
                .expect("write uppercase-digest sidecar");
            }),
        ),
    ];

    for (name, apply) in refusals {
        let sb = sandbox_with_good_release();
        apply(&sb);
        let (ok, out) = run_gate(&sb, &good_assets_json(), &[], &[]);
        assert!(!ok, "scenario must refuse publication: {name}:\n{out}");
        let calls = gh_log_lines(&sb);
        assert!(
            calls.is_empty(),
            "preflight failure ({name}) must make NO gh call at all — the \
             gate reached gh with {calls:?}:\n{out}"
        );
    }
}
