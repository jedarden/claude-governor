//! The cgov-ci arm64 execution-proof contract (claudego-9ede8418).
//!
//! `cgov-ci` — the release WorkflowTemplate in
//! `declarative-config/k8s/iad-ci/argo-workflows/cgov-ci.yaml`, outside this
//! repository — builds `cgov-linux-arm64` in its debian container, installs
//! `qemu-user-static` there (and fail-closes on `command -v
//! qemu-aarch64-static`), runs the shipped validator over the artifact, and
//! refuses to publish unless BOTH smoke probes report a PASS verdict
//! `under qemu-aarch64-static in env -i` — the documented dependency-free
//! environment (README "Zero-dependency validation";
//! `docs/notes/release-static-validation.md` check 5: `env -i`, empty cwd,
//! PATH pointed at an empty directory). A skipped probe is a refusal, not a
//! downgrade.
//!
//! Neither half of that interface is pinned anywhere else:
//!
//! - `tests/release_static_validation_test.rs` pins the *validator's* own
//!   per-architecture behavior — it cannot see the workflow's grep.
//! - `tests/release_publication_gate_test.rs` pins `publish-release.sh`'s
//!   refusal of a skipped probe — a different consumer with its own logic.
//! - The workflow's guard lives in another repository, so it can drift in
//!   two directions with nothing catching it at review time: the validator
//!   rewords its PASS lines and every release starts failing its grep (or
//!   someone "fixes" that by weakening the grep), or the grep loses a probe
//!   / the `^PASS: ` anchor and an arm64 artifact that never executed — or
//!   executed and failed — ships. This file pins the workflow-side consumer
//!   by replaying its gate against the real validator, so drift fails
//!   `cargo test` — the same suite cgov-ci runs as its first release step.
//!
//! The mirrored gate text below was taken verbatim from the applied
//! WorkflowTemplate on 2026-09-27 (only the log path is sandbox-local
//! instead of `/tmp/cgov-arm64-verify.log`). It is a contract, not a copy
//! kept in sync by a gate: when the template's arm64 block changes, update
//! the mirror deliberately and re-verify — the same discipline
//! `tests/bead_rs_contract_test.rs` applies to NEEDLE's own source.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// The shipped validator, embedded at compile time — the tested text cannot
/// drift from the shipped one, and nothing is read from CARGO_MANIFEST_DIR
/// at run time (close-gate test binaries are reused from a shared cache
/// where the extraction may already be deleted).
const VERIFY_SH: &str = include_str!("../scripts/verify-release-static.sh");

/// The workflow's arm64 execution-proof gate, mirrored from the applied
/// `cgov-ci` release step. Everything except `@LOG@` (sandbox-local stand-in
/// for `/tmp/cgov-arm64-verify.log`) is byte-identical to the template,
/// including the anchored grep, the both-probe loop, and the two refusal
/// messages. Runs with the template's own shell options: a validator failure
/// trips `pipefail`, and the grep loop is the second, independent refusal.
const WORKFLOW_GATE_SH: &str = r#"#!/usr/bin/env bash
# Mirror of the cgov-ci release step's arm64 execution-proof block
# (declarative-config/k8s/iad-ci/argo-workflows/cgov-ci.yaml, mirrored
# 2026-09-27; @LOG@ stands in for /tmp/cgov-arm64-verify.log). Pinned by
# tests/cgov_ci_arm64_probe_contract.rs — update deliberately.
set -ex -o pipefail
command -v qemu-aarch64-static >/dev/null || {
  echo "FAIL: qemu-user-static did not provide qemu-aarch64-static — the arm64 execution probe cannot run"
  exit 1
}
qemu-aarch64-static --version >/dev/null
scripts/verify-release-static.sh cgov-linux-arm64 \
  | tee @LOG@
# PASS-specific: a FAIL line names the same probe, so a bare
# match could satisfy a failed run; require PASS verdicts for
# both probes before allowing the artifact to ship.
for probe in --version --help; do
  grep -q -- "^PASS: .* ${probe} under qemu-aarch64-static in env -i" @LOG@ || {
    echo "FAIL: arm64 artifact was not executed under qemu-aarch64-static for ${probe} — refusing to publish an unexecuted artifact"
    exit 1
  }
done
"#;

/// The workflow's probe gate as a Rust predicate — the literal translation
/// of `grep -q -- "^PASS: .* ${probe} under qemu-aarch64-static in env -i"`:
/// `^PASS: ` anchors to a PASS verdict, `.*` spans the artifact path, and
/// the remainder is a plain literal (no other regex semantics involved).
/// Used to pin *why* the mirrored gate decides what it decides; the bash
/// replay is the authoritative verdict.
fn workflow_probe_passed(log: &str, probe: &str) -> bool {
    log.lines().any(|line| {
        line.starts_with("PASS: ")
            && line.contains(&format!(" {probe} under qemu-aarch64-static in env -i"))
    })
}

/// The gate's grep must never be satisfiable by a bare probe mention — that
/// is what the template's PASS-specific comment buys, and what a weakened
/// grep would give away. The naive form (what a "simplified" gate would
/// match on) is spelled out here so the anchor's necessity stays visible.
fn naive_probe_mention(log: &str, probe: &str) -> bool {
    log.contains(&format!(" {probe} under qemu-aarch64-static in env -i"))
}

/// The workflow contract presumes an x86_64 build host — the only shape the
/// deployed template can run in, where arm64 is foreign and can execute only
/// under the emulator it installs. On an aarch64 host the artifact is
/// native, the emulator path (and with it this contract) is unreachable, so
/// the suite says so loudly and moves on rather than pretending to pin
/// something it cannot reach.
fn require_x86_64_host(test_name: &str) -> bool {
    if std::env::consts::ARCH == "x86_64" {
        return true;
    }
    println!(
        "SKIP {test_name}: the cgov-ci arm64 probe contract presumes an x86_64 \
         build host (arm64 foreign, executed under qemu-aarch64-static); on this \
         {} host the workflow's emulated-probe gate cannot be exercised",
        std::env::consts::ARCH
    );
    false
}

/// ELF64 header + single R+X PT_LOAD, no PT_INTERP, no dynamic section —
/// the exact shape the validator calls statically linked. `code` is real
/// AArch64 machine code that writes `message` to stdout and exits
/// `exit_code`, so the artifact is a genuine (if minimal) aarch64 guest, the
/// same construction the neighbouring release suites use for their
/// deterministic stand-ins for the release binaries.
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

/// AArch64: write(1, message, len); exit(exit_code), built from movz/movk/svc.
fn elf_aarch64(message: &[u8], exit_code: u8) -> Vec<u8> {
    let msg_addr = 0x4000_0000u64 + 0x78 + 40; // code is 10 instructions
    let movz = |imm: u32, rd: u32| 0xd280_0000 | (imm << 5) | rd;
    let movk = |imm: u32, shift: u32, rd: u32| 0xf280_0000 | ((shift / 16) << 21) | (imm << 5) | rd;
    let words: Vec<u32> = vec![
        movz(1, 0),                                    // mov x0, #1 (stdout)
        movz(message.len() as u32, 2),                 // mov x2, #len
        movz(msg_addr as u32 & 0xffff, 1),             // mov x1, addr lo16
        movk((msg_addr >> 16) as u32 & 0xffff, 16, 1), //         mid16
        movk((msg_addr >> 32) as u32 & 0xffff, 32, 1), //         hi16
        movz(64, 8),                                   // mov x8, #64 (SYS_write)
        0xd400_0001,                                   // svc #0
        movz(exit_code as u32, 0),                     // mov x0, #exit_code
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

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
}

/// The workflow's workspace, shaped exactly as the release step sees it: the
/// validator at `scripts/verify-release-static.sh`, the fresh-built artifact
/// at `./cgov-linux-arm64`, an empty binfmt_misc dir (BINFMT_MISC_DIR is
/// always pinned so the outcome never depends on the host's real
/// registrations), and symlink-farm bin dirs standing in for the container's
/// PATH — host tools resolved via `command -v`, with and without the
/// `qemu-user-static` doubles, so a stripped PATH differs from the emulated
/// one only in the named omission.
struct WorkflowSandbox {
    dir: TempDir,
}

/// Tools the mirrored gate and the validator need from PATH. `file(1)` is
/// optional to the validator (it downgrades that cross-check loudly when the
/// host lacks it) and simply won't be symlinked if absent.
const TOOLS: &[&str] = &[
    "bash", "sh", "tee", "grep", "sed", "awk", "cut", "head", "cat", "env", "uname", "mktemp",
    "readlink", "readelf", "rm", "file",
];

impl WorkflowSandbox {
    fn new() -> Self {
        let dir = TempDir::new().expect("sandbox");
        fs::create_dir_all(dir.path().join("scripts")).expect("scripts dir");
        write_executable(
            &dir.path().join("scripts/verify-release-static.sh"),
            VERIFY_SH.as_bytes(),
        );
        fs::create_dir(dir.path().join("binfmt-empty")).expect("binfmt dir");
        fs::create_dir(dir.path().join("bin")).expect("bin dir");
        WorkflowSandbox { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn binfmt_empty(&self) -> PathBuf {
        self.dir.path().join("binfmt-empty")
    }

    /// A `qemu-aarch64-static` test double that records its argv. Invoked
    /// with only a flag (the workflow's `qemu-aarch64-static --version`
    /// emulator smoke check) it is a healthy emulator: banner, exit 0.
    /// Invoked with a guest binary it runs that guest as `exit_code`. The
    /// probe runs it under `env -i`, so it may use only shell builtins and
    /// an absolute log path. Deterministic stand-in for the
    /// `qemu-user-static` package the workflow installs.
    fn install_fake_emulator(&self, guest_exit_code: i32) -> PathBuf {
        let log = self.dir.path().join("fake-qemu.log");
        let log_display = log.to_str().expect("log path").to_string();
        write_executable(
            &self.dir.path().join("bin").join("qemu-aarch64-static"),
            &format!(
                r#"#!/bin/sh
# Test double for qemu-aarch64-static: record argv, report the outcome.
printf '%s\n' "$*" >> "{log_display}"
case "$1" in
  ''|-*)
    # The emulator itself, as the workflow's smoke check sees it: healthy.
    echo "fake-qemu-aarch64-static version 0.0-test"
    exit 0
    ;;
esac
if [ "{guest_exit_code}" -eq 0 ]; then
  echo "fake-qemu: executed $*"
else
  echo "fake-qemu: simulated failure of $*" >&2
fi
exit {guest_exit_code}
"#
            )
            .into_bytes(),
        );
        log
    }

    /// A PATH whose bin dir holds the host tools (resolved via `command -v`)
    /// plus the sandbox emulator double, minus anything named in `omit` —
    /// the lever that builds the container-with-qemu PATH and the
    /// qemu-user-static-did-not-install PATH.
    fn tools_bin(&self, dest_name: &str, omit: &[&str]) -> PathBuf {
        let bin = self.dir.path().join(dest_name);
        if bin.is_dir() {
            return bin; // already built for this sandbox — idempotent
        }
        fs::create_dir_all(&bin).expect("create tools bin dir");
        let sandbox_bin = self.dir.path().join("bin");
        for tool in TOOLS.iter().map(|s| (*s).to_string()).chain(
            [
                "qemu-aarch64-static".to_string(),
                "qemu-aarch64".to_string(),
            ]
            .into_iter()
            .filter(|t| !omit.contains(&t.as_str())),
        ) {
            let target = if tool.starts_with("qemu-") {
                sandbox_bin.join(&tool)
            } else {
                let found = Command::new("sh")
                    .args(["-c", &format!("command -v {tool}")])
                    .output()
                    .expect("probe the host for a gate tool");
                if !found.status.success() {
                    if tool == "readelf" {
                        panic!("readelf is required by the validator; this host lacks it");
                    }
                    continue; // optional to the contract on this host
                }
                PathBuf::from(String::from_utf8_lossy(&found.stdout).trim())
            };
            std::os::unix::fs::symlink(&target, bin.join(&tool))
                .expect("symlink a gate tool into the tools bin dir");
        }
        bin
    }

    /// The container-with-qemu PATH: host tools plus the emulator double.
    fn tools_with_qemu(&self) -> PathBuf {
        self.tools_bin("bin-with-qemu", &[])
    }

    /// The container whose qemu-user-static install did not take: host tools,
    /// no emulator of either naming the validator would look up.
    fn tools_without_qemu(&self) -> PathBuf {
        self.tools_bin("bin-no-qemu", &["qemu-aarch64-static", "qemu-aarch64"])
    }

    /// Run the mirrored workflow gate from the workspace root, exactly as the
    /// release step does: `scripts/verify-release-static.sh cgov-linux-arm64`
    /// relative to the checkout, PATH as given, binfmt pinned. Returns the
    /// exit code, the combined output, and whether the tee'd verify log
    /// exists (the gate refuses before creating one when the presence guard
    /// fires).
    fn run_gate(&self, path_bin: &Path) -> (i32, String, bool) {
        let log = self.dir.path().join("cgov-arm64-verify.log");
        let gate = self.dir.path().join("cgov-ci-arm64-gate.sh");
        write_executable(
            &gate,
            WORKFLOW_GATE_SH
                .replace("@LOG@", log.to_str().expect("log path"))
                .as_bytes(),
        );
        let out = Command::new("bash")
            .arg(&gate)
            .current_dir(self.dir.path())
            .env("PATH", path_bin)
            .env("BINFMT_MISC_DIR", self.binfmt_empty())
            .output()
            .expect("spawn bash on the mirrored workflow gate");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text, log.is_file())
    }

    /// Run the validator the way the gate pipes it, standalone, and return
    /// (exit code, combined output, tee-shaped log text).
    fn run_validator(&self, path_bin: &Path) -> (i32, String) {
        let out = Command::new("bash")
            .arg("scripts/verify-release-static.sh")
            .arg("cgov-linux-arm64")
            .current_dir(self.dir.path())
            .env("PATH", path_bin)
            .env("BINFMT_MISC_DIR", self.binfmt_empty())
            .output()
            .expect("spawn bash on the sandboxed validator");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }
}

/// A fresh build of a healthy `cgov-linux-arm64`, as the workflow's cp step
/// leaves it in the workspace root.
fn install_arm64_artifact(sb: &WorkflowSandbox, exit_code: u8) -> PathBuf {
    let artifact = sb.root().join("cgov-linux-arm64");
    write_executable(
        &artifact,
        &elf_aarch64(b"cgov 0.1.x-arm64-contract\n", exit_code),
    );
    artifact
}

#[test]
fn both_probes_passing_under_qemu_satisfy_the_cgov_ci_gate() {
    if !require_x86_64_host("both_probes_passing_under_qemu_satisfy_the_cgov_ci_gate") {
        return;
    }
    let sb = WorkflowSandbox::new();
    let artifact = install_arm64_artifact(&sb, 0);
    let qemu_argv = sb.install_fake_emulator(0);

    let (code, out, log_exists) = sb.run_gate(&sb.tools_with_qemu());
    assert_eq!(
        code, 0,
        "both emulated PASS verdicts must let the release proceed:\n{out}"
    );
    assert!(
        log_exists,
        "the gate must have tee'd the validator output it greps:\n{out}"
    );

    // The right artifact class went through the validator: aarch64, linkage
    // checks included, never skipped.
    assert!(
        out.contains("ELF EXEC for aarch64"),
        "the gate must validate the arm64 artifact itself:\n{out}"
    );
    assert!(
        !out.contains("EXECUTION PROBE SKIPPED"),
        "with qemu installed the probe must run, not skip:\n{out}"
    );

    let log = fs::read_to_string(sb.root().join("cgov-arm64-verify.log"))
        .expect("read the tee'd verify log");
    for probe in ["--version", "--help"] {
        assert!(
            workflow_probe_passed(&log, probe),
            "the workflow grep must match the validator's {probe} PASS line:\n{log}"
        );
        // And the PASS verdict must be for a run in the documented
        // dependency-free environment, on the same line the gate consumes.
        let line = log
            .lines()
            .find(|l| workflow_probe_passed(l, probe))
            .expect("the matching PASS line");
        assert!(
            line.contains("in env -i, empty cwd, empty PATH"),
            "the gated {probe} PASS must name the env -i, empty-cwd, empty-PATH \
             probe environment:\n{line}"
        );
    }

    // Both probes genuinely went through the emulator invocation — the gate
    // is not satisfied by linkage evidence alone.
    let argv = fs::read_to_string(&qemu_argv).expect("read the fake qemu argv log");
    for probe in ["--version", "--help"] {
        assert!(
            argv.contains(&format!("{} {probe}", artifact.display())),
            "the {probe} probe must have been invoked via qemu-aarch64-static:\n{argv}"
        );
    }
}

#[test]
fn cgov_ci_gate_refuses_an_arm64_artifact_that_was_never_executed() {
    if !require_x86_64_host("cgov_ci_gate_refuses_an_arm64_artifact_that_was_never_executed") {
        return;
    }
    let sb = WorkflowSandbox::new();
    install_arm64_artifact(&sb, 0);

    // Layer 1 — the container's qemu install did not take: the presence
    // guard refuses before the artifact is ever offered to the validator,
    // so no verify log is even produced.
    let (code, out, log_exists) = sb.run_gate(&sb.tools_without_qemu());
    assert_eq!(
        code, 1,
        "a container without qemu-aarch64-static must refuse:\n{out}"
    );
    assert!(
        out.contains(
            "FAIL: qemu-user-static did not provide qemu-aarch64-static — the arm64 execution probe cannot run"
        ),
        "the refusal must be the presence guard's own message:\n{out}"
    );
    assert!(
        !log_exists,
        "the presence guard fires before the validator runs, so nothing was tee'd"
    );

    // Layer 2 — defense in depth: even if the presence guard were removed,
    // the grep loop must still refuse. The validator itself exits 0 here —
    // the linkage-only downgrade is loud but not a failure — so the
    // refusal is entirely the workflow's gate, which is exactly why the
    // gate's shape is worth pinning.
    let (vcode, vout) = sb.run_validator(&sb.tools_without_qemu());
    assert_eq!(
        vcode, 0,
        "the validator downgrades to linkage-only evidence with exit 0:\n{vout}"
    );
    assert!(
        vout.contains("EXECUTION PROBE SKIPPED"),
        "the skip must be loud in the validator output:\n{vout}"
    );
    for probe in ["--version", "--help"] {
        assert!(
            !workflow_probe_passed(&vout, probe),
            "a skipped {probe} probe must produce no PASS verdict for the gate \
             to match:\n{vout}"
        );
        assert!(
            !naive_probe_mention(&vout, probe),
            "even a bare {probe} mention must be absent from skip output:\n{vout}"
        );
    }
}

#[test]
fn cgov_ci_gate_refuses_a_probe_that_ran_and_failed() {
    if !require_x86_64_host("cgov_ci_gate_refuses_a_probe_that_ran_and_failed") {
        return;
    }
    let sb = WorkflowSandbox::new();
    // The artifact executes — and exits 3: the runtime-defect class the
    // linkage checks cannot see, which is the entire reason the workflow
    // demands executed artifacts instead of merely validated ones.
    install_arm64_artifact(&sb, 3);
    sb.install_fake_emulator(3);

    let (code, out, log_exists) = sb.run_gate(&sb.tools_with_qemu());
    assert_eq!(
        code, 1,
        "a failing emulated probe must refuse the release:\n{out}"
    );
    assert!(
        log_exists,
        "the validator ran, so its output must have been tee'd for the gate:\n{out}"
    );

    // PASS-specificity, made load-bearing: the failure lines name the very
    // probe text the gate greps for, so a gate weakened to a bare match
    // would accept this run. The `^PASS: ` anchor is what refuses it.
    let log = fs::read_to_string(sb.root().join("cgov-arm64-verify.log"))
        .expect("read the tee'd verify log");
    assert!(
        log.contains("exit=3"),
        "the guest's own exit code must surface in the verify log:\n{log}"
    );
    for probe in ["--version", "--help"] {
        assert!(
            !workflow_probe_passed(&log, probe),
            "a failed {probe} probe must produce no PASS verdict:\n{log}"
        );
        assert!(
            naive_probe_mention(&log, probe),
            "the {probe} FAIL line must name the probe text — that collision is \
             exactly what the PASS anchor exists to defeat:\n{log}"
        );
    }
}
