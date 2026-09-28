//! Static-linkage refusals for genuinely DYNAMIC release artifacts — the
//! defect class checks 2+3 of `scripts/verify-release-static.sh` exist for
//! (claudego-cd2a0d28).
//!
//! The refusal families elsewhere in the suite break an artifact in ways
//! that never reach those two checks: a shell script fails earlier, at
//! "could not parse it as ELF" (release_publication_gate_test.rs), a
//! stripped exec bit fails earlier still (claudego-eb2394ce), and the
//! hand-assembled fixtures everywhere else are perfectly static ELFs that
//! pass the linkage checks by construction. No fixture anywhere is a
//! *dynamic* ELF — one that requests a PT_INTERP loader or carries a
//! DT_NEEDED shared-library entry — which is exactly what the README's
//! "zero runtime dependencies — single statically-linked binary" promise
//! refuses. If checks 2+3 were dropped from the validator, every other
//! test would still pass. This file builds those artifacts by hand (the
//! same construction as the static fixtures — ELF64 header, program
//! headers, real machine code — plus an INTERP phdr, or a PT_DYNAMIC
//! segment with a DT_NEEDED entry and the string table readelf resolves it
//! against) and pins the refusal at BOTH layers:
//!
//! - the validator itself refuses: exit 1, naming the PT_INTERP segment or
//!   the NEEDED entries, and printing the resolved library name;
//! - `scripts/publish-release.sh` refuses to publish and never makes a gh
//!   call — the gate must not need to understand WHY the validator failed,
//!   only that a plausible-looking ELF (correct machine type, real code)
//!   failed it.
//!
//! Both linkage checks run before any probe decision and are
//! cross-architecture (readelf reads foreign ELFs natively), so the
//! refusals here are deterministic on any host — no emulator, no binfmt
//! state, no network. The host architecture is exercised too: the build
//! host's own artifact gets no exemption from the linkage checks.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

/// The shipped static validator, embedded at compile time — the tested text
/// cannot drift from the shipped one, and nothing is read from
/// CARGO_MANIFEST_DIR at run time (close-gate extractions are deleted).
const VERIFY_SH: &str = include_str!("../scripts/verify-release-static.sh");

/// The shipped publication gate, embedded because the gate invokes the
/// validator as a sibling file and the sandbox has to reproduce that layout.
const PUBLISH_SH: &str = include_str!("../scripts/publish-release.sh");

const TAG: &str = "v0.1.2";

/// The shipped diagnoses this file exists to pin. If the validator ever
/// rewords them, these constants are what to update — deliberately, with
/// the wording, not silently.
const INTERP_DIAGNOSIS: &str = "has a PT_INTERP segment (requests a dynamic loader)";
const NEEDED_DIAGNOSIS: &str = "has NEEDED shared-library entries";
const FAILED_SUMMARY: &str = "verify-release-static: FAILED";

// ---------------------------------------------------------------------------
// Hand-assembled dynamic ELFs
// ---------------------------------------------------------------------------

/// What makes the artifact dynamic. `Interp` adds a PT_INTERP program
/// header naming a real-world loader path; `Needed` adds a PT_DYNAMIC
/// segment carrying DT_NEEDED plus the DT_STRTAB/DT_STRSZ pair readelf
/// needs to resolve the name.
#[derive(Clone, Copy)]
enum DynamicKind {
    Interp(&'static str),
    Needed(&'static str),
}

/// ELF64 header + one R+X PT_LOAD covering the whole file (the static
/// fixture's shape) + one extra program header per the requested dynamic
/// kind. `code` is real machine code for `machine` that writes `message`
/// and exits 0, so the artifact differs from a good release binary in
/// exactly the linkage defect — nothing else about it is degenerate.
fn dynamic_elf(machine: u16, message: &[u8], kind: DynamicKind) -> Vec<u8> {
    const BASE: u64 = 0x4000_0000;

    let (interp, needed) = match kind {
        DynamicKind::Interp(p) => (Some(p), None),
        DynamicKind::Needed(n) => (None, Some(n)),
    };
    let phnum = 1 + usize::from(interp.is_some()) + usize::from(needed.is_some());
    let code_off = 64 + 56 * phnum;

    let code = match machine {
        0x3e => code_x86_64(message, code_off),
        0xb7 => code_aarch64(message, code_off),
        m => panic!("unsupported machine {m:#x}"),
    };
    let code_len = code.len();

    // Payload layout after the code: the interp string, then the dynamic
    // array, then the string table — each's offset recorded as placed.
    let mut off = code_off + code_len;
    let interp_off = interp.map(|p| {
        let o = off;
        off += p.len() + 1; // NUL-terminated, as the kernel expects
        (o, p)
    });
    // DT_NEEDED, DT_STRTAB, DT_STRSZ, DT_NULL — 4 entries.
    const DYNAMIC_BYTES: usize = 4 * 16;
    let dyn_off = needed.map(|_| {
        let o = off;
        off += DYNAMIC_BYTES;
        o
    });
    let strtab: Vec<u8> = needed
        .map(|n| {
            let mut s = n.as_bytes().to_vec();
            s.push(0);
            off += s.len();
            s
        })
        .unwrap_or_default();
    let filesz = off;

    let mut b = Vec::with_capacity(filesz);
    // e_ident: ELFmagic, 64-bit, little-endian, v1, SysV, no ABI
    b.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    b.extend_from_slice(&[0u8; 8]);
    b.extend_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
    b.extend_from_slice(&machine.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes()); // e_version
    b.extend_from_slice(&(BASE + code_off as u64).to_le_bytes()); // e_entry
    b.extend_from_slice(&64u64.to_le_bytes()); // e_phoff
    b.extend_from_slice(&0u64.to_le_bytes()); // e_shoff
    b.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    b.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
    b.extend_from_slice(&56u16.to_le_bytes()); // e_phentsize
    b.extend_from_slice(&(phnum as u16).to_le_bytes()); // e_phnum
    b.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
    b.extend_from_slice(&0u16.to_le_bytes()); // e_shnum
    b.extend_from_slice(&0u16.to_le_bytes()); // e_shstrndx

    // PT_LOAD, R+X, whole file mapped at BASE — same as the static fixture.
    b.extend_from_slice(&1u32.to_le_bytes()); // p_type
    b.extend_from_slice(&5u32.to_le_bytes()); // p_flags = R + X
    b.extend_from_slice(&0u64.to_le_bytes()); // p_offset
    b.extend_from_slice(&BASE.to_le_bytes()); // p_vaddr
    b.extend_from_slice(&BASE.to_le_bytes()); // p_paddr
    b.extend_from_slice(&(filesz as u64).to_le_bytes());
    b.extend_from_slice(&(filesz as u64).to_le_bytes()); // p_memsz
    b.extend_from_slice(&0x1000u64.to_le_bytes()); // p_align

    if let Some((o, p)) = interp_off {
        b.extend_from_slice(&3u32.to_le_bytes()); // PT_INTERP
        b.extend_from_slice(&4u32.to_le_bytes()); // p_flags = R
        b.extend_from_slice(&(o as u64).to_le_bytes()); // p_offset
        b.extend_from_slice(&(BASE + o as u64).to_le_bytes()); // p_vaddr
        b.extend_from_slice(&(BASE + o as u64).to_le_bytes()); // p_paddr
        b.extend_from_slice(&((p.len() + 1) as u64).to_le_bytes()); // p_filesz
        b.extend_from_slice(&((p.len() + 1) as u64).to_le_bytes()); // p_memsz
        b.extend_from_slice(&1u64.to_le_bytes()); // p_align
    }
    if let Some(o) = dyn_off {
        b.extend_from_slice(&2u32.to_le_bytes()); // PT_DYNAMIC
        b.extend_from_slice(&6u32.to_le_bytes()); // p_flags = RW
        b.extend_from_slice(&(o as u64).to_le_bytes()); // p_offset
        b.extend_from_slice(&(BASE + o as u64).to_le_bytes()); // p_vaddr
        b.extend_from_slice(&(BASE + o as u64).to_le_bytes()); // p_paddr
        b.extend_from_slice(&(DYNAMIC_BYTES as u64).to_le_bytes());
        b.extend_from_slice(&(DYNAMIC_BYTES as u64).to_le_bytes()); // p_memsz
        b.extend_from_slice(&8u64.to_le_bytes()); // p_align
    }

    assert_eq!(b.len(), code_off, "phdr block must fill exactly to code_off");
    b.extend_from_slice(&code);
    if let Some((o, p)) = interp_off {
        assert_eq!(b.len(), o, "interp string must land at its phdr offset");
        b.extend_from_slice(p.as_bytes());
        b.push(0);
    }
    if let Some(o) = dyn_off {
        assert_eq!(b.len(), o, "dynamic array must land at its phdr offset");
        b.extend_from_slice(&1u64.to_le_bytes()); // DT_NEEDED
        b.extend_from_slice(&0u64.to_le_bytes()); // strtab offset 0
        b.extend_from_slice(&5u64.to_le_bytes()); // DT_STRTAB
        let strtab_off = o + DYNAMIC_BYTES;
        b.extend_from_slice(&(BASE + strtab_off as u64).to_le_bytes());
        b.extend_from_slice(&10u64.to_le_bytes()); // DT_STRSZ
        b.extend_from_slice(&(strtab.len() as u64).to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes()); // DT_NULL
        b.extend_from_slice(&0u64.to_le_bytes());
    }
    if !strtab.is_empty() {
        b.extend_from_slice(&strtab);
    }
    assert_eq!(b.len(), filesz);
    b
}

/// x86-64: write(1, message, len); exit(0). Stable kernel ABI, no libc.
/// `code_off` is where the caller places the code in the file — the message
/// lands directly after it, and the write syscall's address is patched to
/// match, so any program-header count produces an artifact that genuinely
/// prints `message`.
fn code_x86_64(message: &[u8], code_off: usize) -> Vec<u8> {
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
    let msg_addr = (0x4000_0000u64 + code_off as u64 + code.len() as u64) as u32;
    code[11..15].copy_from_slice(&msg_addr.to_le_bytes());
    code[16..20].copy_from_slice(&(message.len() as u32).to_le_bytes());
    code.extend_from_slice(message);
    code
}

/// AArch64: write(1, message, len); exit(0), built from movz/movk/svc.
fn code_aarch64(message: &[u8], code_off: usize) -> Vec<u8> {
    let msg_addr = 0x4000_0000u64 + code_off as u64 + 40; // code is 10 instructions
    let movz = |imm: u32, rd: u32| 0xd280_0000 | (imm << 5) | rd;
    // `hw` (bits 21-22) is shift/16, not the raw byte count — see the
    // note on the same builder in the sibling suites.
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
    let mut code = Vec::with_capacity(words.len() * 4 + message.len());
    for w in words {
        code.extend_from_slice(&w.to_le_bytes());
    }
    assert_eq!(code.len(), 40);
    code.extend_from_slice(message);
    code
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// This host's architecture mapped the way the validator's `host_machine`
/// maps it; the artifact of the OTHER architecture is the foreign one.
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

fn machine_elf_code(machine: &str) -> u16 {
    match machine {
        "x86_64" => 0x3e,    // EM_X86_64
        "aarch64" => 0xb7,   // EM_AARCH64
        m => panic!("unsupported machine {m}"),
    }
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

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=claudego-cd2a0d28@test",
            "-c",
            "user.name=linkage-test",
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

// ---------------------------------------------------------------------------
// Validator-level sandbox
// ---------------------------------------------------------------------------

/// The validator materialized under its own `scripts/` path shape, plus an
/// empty binfmt_misc dir and a bin/ dir for the emulator double. Always run
/// with BINFMT_MISC_DIR pinned so the outcome never depends on the host's
/// real registrations.
struct ValidatorSandbox {
    dir: TempDir,
}

impl ValidatorSandbox {
    fn new() -> Self {
        let dir = TempDir::new().expect("sandbox");
        fs::create_dir_all(dir.path().join("scripts")).expect("scripts dir");
        write_executable(
            &dir.path().join("scripts/verify-release-static.sh"),
            VERIFY_SH.as_bytes(),
        );
        fs::create_dir(dir.path().join("binfmt-empty")).expect("binfmt dir");
        fs::create_dir(dir.path().join("bin")).expect("bin dir");
        ValidatorSandbox { dir }
    }

    /// A `qemu-<machine>-static` test double that succeeds with output —
    /// the probe's only requirement. Its presence on PATH means the probe
    /// RUNS for a foreign artifact, so a refusal below is attributable to
    /// the linkage defect alone, not to any inability to execute.
    fn install_fake_emulator(&self, machine: &str) {
        write_executable(
            &self.dir.path().join("bin").join(format!("qemu-{machine}-static")),
            br#"#!/bin/sh
# Test double for qemu-<machine>-static: builtins only (the probe runs it
# under env -i); success with output is all the probe requires.
echo "fake-qemu: executed $*"
exit 0
"#,
        );
    }

    fn run(&self, artifact: &Path, machine_on_path: Option<&str>) -> (bool, String) {
        if let Some(m) = machine_on_path {
            self.install_fake_emulator(m);
        }
        let mut cmd = Command::new("bash");
        cmd.arg(self.dir.path().join("scripts/verify-release-static.sh"))
            .arg(artifact)
            .env("BINFMT_MISC_DIR", self.dir.path().join("binfmt-empty"));
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env(
            "PATH",
            format!("{}:{}", self.dir.path().join("bin").display(), path),
        );
        let out = cmd.output().expect("spawn bash on the sandboxed validator");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }
}

fn dynamic_artifact(machine: &str, kind: DynamicKind) -> Vec<u8> {
    dynamic_elf(
        machine_elf_code(machine),
        b"cgov 0.1.2-dynamic\n",
        kind,
    )
}

// ---------------------------------------------------------------------------
// Gate-level sandbox (the release_publication_gate_test.rs harness, subset)
// ---------------------------------------------------------------------------

struct GateSandbox {
    dir: TempDir,
    release_dir: PathBuf,
    origin_url: String,
    gh_log: PathBuf,
}

/// The full good-release sandbox: both scripts materialized as siblings, a
/// one-commit checkout whose origin is a local bare Forgejo stand-in with
/// the tag pushed, both static artifacts with matching sidecars, a
/// recording gh fake, and emulator doubles for both architectures.
fn gate_sandbox_with_good_release() -> GateSandbox {
    let dir = TempDir::new().expect("sandbox");
    fs::create_dir_all(dir.path().join("scripts")).expect("scripts dir");
    write_executable(
        &dir.path().join("scripts/publish-release.sh"),
        PUBLISH_SH.as_bytes(),
    );
    write_executable(
        &dir.path().join("scripts/verify-release-static.sh"),
        VERIFY_SH.as_bytes(),
    );

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

    let msg = b"cgov 0.1.2-sandbox\n";
    // The two good artifacts are the STATIC fixtures the sibling suites
    // prove pass: built here through `static_via_dynamic_builder`, which
    // shares the code builders but never touches the dynamic-kind plumbing.
    write_executable(
        &release_dir.join("cgov-linux-amd64"),
        &static_via_dynamic_builder(0x3e, msg),
    );
    write_executable(
        &release_dir.join("cgov-linux-arm64"),
        &static_via_dynamic_builder(0xb7, msg),
    );
    for name in ["cgov-linux-amd64", "cgov-linux-arm64"] {
        let digest = sha256_hex(&release_dir.join(name));
        fs::write(
            release_dir.join(format!("{name}.sha256")),
            sidecar_bytes(&digest, name),
        )
        .expect("write sidecar");
    }

    git(&release_dir, &["tag", TAG]);
    git(
        &release_dir,
        &["push", "-q", "origin", &format!("refs/tags/{TAG}")],
    );

    // Recording gh fake: one line per call, asset query from a scenario file.
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
  *" release create "*) exit 0 ;;
  *" release view "*) cat "${{CGOV_FAKE_GH_ASSETS:?}}"; exit 0 ;;
  *) echo "fake gh: unexpected call: $*" >&2; exit 64 ;;
esac
"#
        )
        .into_bytes(),
    );

    GateSandbox {
        dir,
        release_dir,
        origin_url,
        gh_log,
    }
}

/// A good static artifact through the same builder: `dynamic_elf` with no
/// dynamic kind is byte-shape-equivalent to the sibling suites' static
/// fixture (one PT_LOAD phdr, code, message).
fn static_via_dynamic_builder(machine: u16, message: &[u8]) -> Vec<u8> {
    // Constructed by hand here rather than through dynamic_elf(…, kind) so
    // the good artifacts never depend on the dynamic-kind plumbing: this
    // mirrors the sibling suites' own static builder directly.
    const BASE: u64 = 0x4000_0000;
    let code_off = 64 + 56; // one phdr
    let code = match machine {
        0x3e => code_x86_64(message, code_off),
        0xb7 => code_aarch64(message, code_off),
        m => panic!("unsupported machine {m:#x}"),
    };
    let filesz = code_off + code.len();
    let mut b = Vec::with_capacity(filesz);
    b.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    b.extend_from_slice(&[0u8; 8]);
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&machine.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&(BASE + code_off as u64).to_le_bytes());
    b.extend_from_slice(&64u64.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&64u16.to_le_bytes());
    b.extend_from_slice(&56u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&64u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&5u32.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&BASE.to_le_bytes());
    b.extend_from_slice(&BASE.to_le_bytes());
    b.extend_from_slice(&(filesz as u64).to_le_bytes());
    b.extend_from_slice(&(filesz as u64).to_le_bytes());
    b.extend_from_slice(&0x1000u64.to_le_bytes());
    assert_eq!(b.len(), code_off);
    b.extend_from_slice(&code);
    assert_eq!(b.len(), filesz);
    b
}

fn good_assets_json() -> String {
    r#"{"assets":[{"name":"cgov-linux-amd64","size":150},{"name":"cgov-linux-amd64.sha256","size":69},{"name":"cgov-linux-arm64","size":150},{"name":"cgov-linux-arm64.sha256","size":69}]}"#
        .to_string()
}

fn run_gate(sb: &GateSandbox) -> (i32, String) {
    let assets_file = sb.dir.path().join("assets.json");
    fs::write(&assets_file, good_assets_json()).expect("write fake gh assets scenario");
    let release_arg = sb.release_dir.to_string_lossy().into_owned();
    let path = std::env::var("PATH").unwrap_or_default();
    let out = Command::new("bash")
        .arg(sb.dir.path().join("scripts/publish-release.sh"))
        .args(["--version", TAG, "--release-dir", &release_arg])
        .env("CGOV_FORGEJO_URL", &sb.origin_url)
        .env("CGOV_FAKE_GH_ASSETS", &assets_file)
        .env("PATH", format!("{}:{}", sb.dir.path().join("bin").display(), path))
        .env_remove("CGOV_GH_REPO")
        .output()
        .expect("spawn bash on the sandboxed gate");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

fn create_calls(sb: &GateSandbox) -> Vec<String> {
    match fs::read_to_string(&sb.gh_log) {
        Ok(text) => text
            .lines()
            .filter(|l| {
                l.split_whitespace().take(2).collect::<Vec<_>>() == ["release", "create"]
            })
            .map(str::to_string)
            .collect(),
        Err(_) if !sb.gh_log.exists() => Vec::new(),
        Err(e) => panic!("read gh log: {e}"),
    }
}

/// Swap the arm64 artifact for a dynamic one and re-sidecar it, so the
/// gate's refusal is attributable to the static phase alone — the sidecar
/// provably matches the bytes being refused.
fn install_dynamic_arm64_artifact(sb: &GateSandbox, kind: DynamicKind) {
    let arm64 = sb.release_dir.join("cgov-linux-arm64");
    write_executable(&arm64, &dynamic_artifact("aarch64", kind));
    let digest = sha256_hex(&arm64);
    fs::write(
        sb.release_dir.join("cgov-linux-arm64.sha256"),
        sidecar_bytes(&digest, "cgov-linux-arm64"),
    )
    .expect("re-sidecar the dynamic artifact");
}

// ---------------------------------------------------------------------------
// The contract — validator level
// ---------------------------------------------------------------------------

#[test]
fn pt_interp_artifact_is_refused_by_the_validator() {
    // A foreign-architecture artifact WITH an emulator available: every
    // check runs, the probe even passes, and the PT_INTERP segment still
    // fails the script. Linkage evidence is not negotiable.
    let m = foreign_machine();
    let sb = ValidatorSandbox::new();
    let artifact = sb.dir.path().join(format!("cgov-dynamic-{m}"));
    write_executable(&artifact, &dynamic_artifact(&m, DynamicKind::Interp(interp_path(&m))));

    let (ok, out) = sb.run(&artifact, Some(&m));
    assert!(
        !ok,
        "an artifact requesting a dynamic loader must fail the validator:\n{out}"
    );
    assert!(
        out.contains(INTERP_DIAGNOSIS),
        "the refusal must name the PT_INTERP segment:\n{out}"
    );
    assert!(
        out.contains(FAILED_SUMMARY),
        "the failure must be tallied into the script's verdict:\n{out}"
    );
    assert!(
        !out.contains("no PT_INTERP"),
        "the passing form of the INTERP check must be absent:\n{out}"
    );
}

#[test]
fn pt_interp_artifact_is_refused_on_the_host_architecture_too() {
    // The mirror on the build host's own machine: the architecture the
    // validator would natively execute gets no exemption from the linkage
    // checks — "statically linked" is a property of the bytes, not of
    // whether this host could run them.
    let m = host_machine();
    let sb = ValidatorSandbox::new();
    let artifact = sb.dir.path().join(format!("cgov-dynamic-{m}"));
    write_executable(&artifact, &dynamic_artifact(&m, DynamicKind::Interp(interp_path(&m))));

    let (ok, out) = sb.run(&artifact, None);
    assert!(
        !ok,
        "a host-architecture artifact requesting a dynamic loader must fail \
         the validator:\n{out}"
    );
    assert!(
        out.contains(INTERP_DIAGNOSIS),
        "the refusal must name the PT_INTERP segment:\n{out}"
    );
    assert!(out.contains(FAILED_SUMMARY), "verdict must tally:\n{out}");
}

#[test]
fn dt_needed_shared_library_artifact_is_refused_by_the_validator() {
    // The second linkage check, exercised on its own: no PT_INTERP, but a
    // PT_DYNAMIC segment carrying DT_NEEDED. readelf must resolve the name
    // through the fixture's string table — the validator prints the
    // offending entries, and the refusal is a genuine "this binary needs a
    // shared library at runtime", not a parse artifact.
    let m = foreign_machine();
    let sb = ValidatorSandbox::new();
    let artifact = sb.dir.path().join(format!("cgov-dynamic-{m}"));
    write_executable(
        &artifact,
        &dynamic_artifact(&m, DynamicKind::Needed("libc.so.6")),
    );

    let (ok, out) = sb.run(&artifact, Some(&m));
    assert!(
        !ok,
        "an artifact with a NEEDED shared library must fail the validator:\n{out}"
    );
    assert!(
        out.contains(NEEDED_DIAGNOSIS),
        "the refusal must name the NEEDED entries:\n{out}"
    );
    assert!(
        out.contains("Shared library: [libc.so.6]"),
        "the validator must print the resolved library name readelf resolved \
         through the fixture's string table:\n{out}"
    );
    assert!(
        out.contains(FAILED_SUMMARY),
        "the failure must be tallied into the script's verdict:\n{out}"
    );
    assert!(
        !out.contains(INTERP_DIAGNOSIS),
        "this fixture carries no PT_INTERP — only the NEEDED check may fire:\n{out}"
    );
}

/// The real-world loader path for `machine` — what a genuinely dynamically
/// linked binary of that architecture carries in its PT_INTERP.
fn interp_path(machine: &str) -> &'static str {
    match machine {
        "x86_64" => "/lib64/ld-linux-x86-64.so.2",
        "aarch64" => "/lib/ld-linux-aarch64.so.1",
        m => panic!("unsupported machine {m}"),
    }
}

// ---------------------------------------------------------------------------
// The contract — publication gate level
// ---------------------------------------------------------------------------

#[test]
fn pt_interp_arm64_artifact_refuses_publication_and_never_publishes() {
    let sb = gate_sandbox_with_good_release();
    install_dynamic_arm64_artifact(&sb, DynamicKind::Interp(interp_path("aarch64")));

    let (code, out) = run_gate(&sb);
    assert_eq!(
        code, 1,
        "a release binary requesting a dynamic loader must refuse publication:\n{out}"
    );
    assert!(
        out.contains("static: cgov-linux-arm64 failed"),
        "the refusal must be attributed to the arm64 artifact's static phase:\n{out}"
    );
    assert!(
        out.contains(INTERP_DIAGNOSIS),
        "the validator's PT_INTERP diagnosis must surface through the gate:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}

#[test]
fn dt_needed_arm64_artifact_refuses_publication_and_never_publishes() {
    let sb = gate_sandbox_with_good_release();
    install_dynamic_arm64_artifact(&sb, DynamicKind::Needed("libc.so.6"));

    let (code, out) = run_gate(&sb);
    assert_eq!(
        code, 1,
        "a release binary with a NEEDED shared library must refuse publication:\n{out}"
    );
    assert!(
        out.contains("static: cgov-linux-arm64 failed"),
        "the refusal must be attributed to the arm64 artifact's static phase:\n{out}"
    );
    assert!(
        out.contains(NEEDED_DIAGNOSIS),
        "the validator's NEEDED diagnosis must surface through the gate:\n{out}"
    );
    assert!(
        create_calls(&sb).is_empty(),
        "refused publication must not call gh release create"
    );
}
