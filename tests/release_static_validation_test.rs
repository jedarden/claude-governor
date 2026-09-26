//! Release validation for the README's zero-runtime-dependency promise
//! ("Zero runtime dependencies — Single statically-linked binary"),
//! claudego-6326b06e.
//!
//! Every supported release artifact that exists on disk is passed through
//! `scripts/verify-release-static.sh`, which fails unless the binary:
//!   1. is an ELF executable for x86-64 or AArch64;
//!   2. has no PT_INTERP segment (no dynamic loader);
//!   3. has no NEEDED shared-library entries;
//!   4. is never reported dynamically linked by file(1) (when available);
//!   5. runs `--version` and `--help` with output under `env -i` from an
//!      empty working directory with PATH pointed at an empty directory.
//!
//! Check 5 is pinned by the architecture-contract tests below
//! (claudego-8af2d72b): the host architecture executes its probe natively;
//! a foreign architecture executes it under a user-mode emulator or a
//! binfmt_misc registration when either exists, and with neither keeps the
//! full linkage checks while skipping execution only — loudly, because an
//! artifact that ships linkage-only evidence must say so.
//!
//! When no release artifact is built, the on-disk validation SKIPS with a
//! loud note so plain `cargo test` stays green in clean extractions;
//! `make verify-release` is the authoritative end-to-end invocation because
//! it builds the artifact first. The script is the single source of truth
//! for what "static" means here — these tests only wire it into `cargo test`.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Repo root, resolved at run time (cargo sets it per invocation — never
/// baked into the test binary, which may be reused from a shared cache).
fn manifest_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("cargo always sets CARGO_MANIFEST_DIR for tests"),
    )
}

/// Cargo binaries to consult for the effective target directory, most
/// specific first. `~/.cargo/bin/cargo` is the real rustup binary; the
/// codinghome/lab `cargo` wrapper may export a redirected CARGO_TARGET_DIR
/// for the commands it limits (needle-d6b685b4), which is where builds of
/// THIS repo land under `cargo test` but not where plain `cargo build`
/// artifacts go (those follow the configured target-dir). Probing both
/// covers both layouts.
fn cargo_binaries() -> Vec<PathBuf> {
    let mut bins = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let real = PathBuf::from(&home).join(".cargo").join("bin").join("cargo");
        if real.is_file() {
            bins.push(real);
        }
    }
    bins.push(PathBuf::from("cargo"));
    bins
}

/// Target directories cargo would use for this workspace, deduped, in probe
/// order. A failed `cargo metadata` simply contributes nothing. The real
/// binary is also consulted with CARGO_TARGET_DIR removed, which surfaces a
/// configured target-dir (fleet boxes set one off-repo) that an inherited
/// wrapper export would otherwise shadow.
fn cargo_target_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut probe = |bin: &PathBuf, strip_target_dir: bool| {
        let mut cmd = Command::new(bin);
        if strip_target_dir {
            cmd.env_remove("CARGO_TARGET_DIR");
        }
        let out = match cmd
            .args(["metadata", "--no-deps", "--offline", "--format-version", "1"])
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return,
        };
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            if let Some(s) = v["target_directory"].as_str() {
                let p = PathBuf::from(s);
                if !dirs.contains(&p) {
                    dirs.push(p);
                }
            }
        }
    };
    for bin in cargo_binaries() {
        probe(&bin, false);
    }
    if let Some(real) = cargo_binaries().first() {
        probe(real, true);
    }
    dirs
}

/// Existing release artifacts the validation applies to: the musl static
/// binaries cargo produces and the cgov-ci release artifacts.
fn candidate_artifacts() -> Vec<PathBuf> {
    if let Ok(explicit) = std::env::var("CGOV_RELEASE_BIN") {
        let p = PathBuf::from(explicit);
        return if p.is_file() { vec![p] } else { vec![] };
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut target_dirs: Vec<PathBuf> = cargo_target_dirs();
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        target_dirs.push(PathBuf::from(td));
    }
    target_dirs.push(manifest_dir().join("target"));

    for td in &target_dirs {
        for triple in [
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
        ] {
            candidates.push(td.join(triple).join("release").join("cgov"));
        }
    }
    for name in ["cgov-linux-amd64", "cgov-linux-arm64"] {
        candidates.push(manifest_dir().join(name));
    }

    let mut found: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if c.is_file() && !found.iter().any(|f| f == &c) {
            found.push(c);
        }
    }
    found
}

#[test]
fn release_binaries_hold_the_zero_runtime_dependency_promise() {
    println!("\n=== Release static-linkage validation ===\n");

    let script = manifest_dir()
        .join("scripts")
        .join("verify-release-static.sh");
    assert!(
        script.is_file(),
        "validation script missing: {}",
        script.display()
    );

    let artifacts = candidate_artifacts();
    if artifacts.is_empty() {
        println!(
            "SKIP: no release artifact found (musl release build or cgov-linux-* \
             artifact). Run `make verify-release` to build one and validate it; \
             this test only validates artifacts that already exist."
        );
        println!(
            "  probed target dirs: {:?}; override with CGOV_RELEASE_BIN to point \
             at an artifact elsewhere",
            {
                let mut dirs = cargo_target_dirs();
                if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
                    dirs.push(PathBuf::from(td));
                }
                dirs.push(manifest_dir().join("target"));
                dirs
            }
        );
        return;
    }

    for artifact in &artifacts {
        println!("--- validating {} ---", artifact.display());
        let out = Command::new("bash")
            .arg(&script)
            .arg(artifact)
            .output()
            .expect("failed to spawn the validation script");
        print!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(
            out.status.success(),
            "release artifact {} does not hold the zero-runtime-dependency \
             promise (script exited {})",
            artifact.display(),
            out.status.code().unwrap_or(-1)
        );
    }
    assert!(!artifacts.is_empty());
}

#[test]
fn non_executable_artifact_is_refused_until_the_exec_bit_is_restored() {
    // An artifact whose exec bit was stripped — the classic shape of an
    // archive extraction that dropped permissions — has byte-identical
    // content and therefore a valid digest, so every content check would
    // pass it. The validator must refuse it on the executable-regular-file
    // check alone, and accept the very same bytes again once the bit is
    // back — pinning the mode as the only defect (claudego-eb2394ce).
    let sb = ArchSandbox::new();
    let msg = b"cgov 0.1.2-mode-probe\n";
    let artifact = sb.dir.path().join("cgov-noexec");
    fs::write(&artifact, artifact_for(host_machine(), msg, 0)).expect("write artifact");
    fs::set_permissions(&artifact, fs::Permissions::from_mode(0o644)).expect("chmod 0644");

    let (ok, out) = sb.run(&artifact, &sb.binfmt_empty(), false);
    assert!(
        !ok,
        "a regular file without its exec bit must fail the validator:\n{out}"
    );
    assert!(
        out.contains("not an executable regular file"),
        "the refusal must name the executable-regular-file check:\n{out}"
    );
    assert!(
        out.contains("FAILED (1 check(s) failed)"),
        "exactly the executability check may have failed:\n{out}"
    );
    assert!(
        !out.contains("EXECUTION PROBE SKIPPED"),
        "a refused artifact must not be reported as a probe skip — \
         it was never eligible to run:\n{out}"
    );

    fs::set_permissions(&artifact, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
    let (ok, out) = sb.run(&artifact, &sb.binfmt_empty(), false);
    assert!(
        ok,
        "the same bytes with the exec bit restored must pass:\n{out}"
    );
    assert!(
        out.contains(&format!("ELF EXEC for {}", host_machine())),
        "the linkage checks must run once the artifact is executable:\n{out}"
    );
    assert!(
        out.contains("--version in env -i, empty cwd, empty PATH"),
        "restoring the bit must restore the execution probe:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// The architecture contract (claudego-8af2d72b). The README documents the
// script's per-architecture behavior; these tests pin it against hand-built
// ELFs so it cannot drift from the documentation in a clean extraction:
//   host arch     — the env -i execution probe always runs, and a runtime
//                   defect that passes every linkage check still fails;
//   foreign arch  — full linkage checks always run; execution runs under an
//                   emulator or binfmt_misc when either exists and is
//                   skipped (loudly) only with neither; cgov-ci makes the
//                   emulator path mandatory for the published arm64 asset.
// ---------------------------------------------------------------------------

/// The shipped validator, embedded at compile time — the tested text cannot
/// drift from the shipped one, and nothing is read from CARGO_MANIFEST_DIR
/// at run time (close-gate test binaries are reused from a shared cache
/// where the extraction may already be deleted).
const VERIFY_SH: &str = include_str!("../scripts/verify-release-static.sh");

/// This machine in the script's naming. The contract is defined for the two
/// platforms install.sh supports; anything else has nothing to pin.
fn host_machine() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => panic!("arch contract is pinned for x86_64/aarch64; host is {other}"),
    }
}

fn foreign_machine() -> &'static str {
    if host_machine() == "x86_64" {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// True when a user-mode emulator for `machine` is visible on this test
/// process's PATH — the same lookup the script does, so the tests branch on
/// host capability instead of assuming it.
fn host_has_emulator(machine: &str) -> bool {
    Command::new("bash")
        .args([
            "-c",
            &format!("command -v qemu-{machine}-static || command -v qemu-{machine}"),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// ELF64 header + single R+X PT_LOAD, no PT_INTERP, no dynamic section —
/// the exact shape the validator calls statically linked. `code` is real
/// machine code that writes `message` to stdout and exits `exit_code`, so
/// artifacts of the executing architecture genuinely run under the probe.
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

/// x86-64: write(1, message, len); exit(exit_code). Stable kernel ABI, no libc.
fn elf_x86_64(message: &[u8], exit_code: u8) -> Vec<u8> {
    let mut code: Vec<u8> = vec![
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1   (SYS_write)
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1   (stdout)
        0xbe, 0, 0, 0, 0, // mov esi, msg  (patched below)
        0xba, 0, 0, 0, 0, // mov edx, len  (patched below)
        0x0f, 0x05, // syscall
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, 60  (SYS_exit)
        0xbf, 0, 0, 0, 0, // mov edi, code (patched below)
        0x0f, 0x05, // syscall
    ];
    assert_eq!(code.len(), 34);
    let msg_addr = (0x4000_0000u64 + (0x78 + code.len()) as u64) as u32;
    code[11..15].copy_from_slice(&msg_addr.to_le_bytes());
    code[16..20].copy_from_slice(&(message.len() as u32).to_le_bytes());
    code[28] = exit_code;
    static_elf(0x3e, &code, message) // EM_X86_64
}

/// AArch64: write(1, message, len); exit(exit_code), built from movz/movk/svc.
fn elf_aarch64(message: &[u8], exit_code: u8) -> Vec<u8> {
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

fn artifact_for(machine: &str, message: &[u8], exit_code: u8) -> Vec<u8> {
    match machine {
        "x86_64" => elf_x86_64(message, exit_code),
        "aarch64" => elf_aarch64(message, exit_code),
        m => panic!("unsupported machine {m}"),
    }
}

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
}

/// The validator materialized under its own `scripts/` path shape, plus an
/// empty binfmt_misc dir and a bin/ dir for test doubles.
struct ArchSandbox {
    dir: TempDir,
}

impl ArchSandbox {
    fn new() -> Self {
        let dir = TempDir::new().expect("sandbox");
        fs::create_dir_all(dir.path().join("scripts")).expect("scripts dir");
        write_executable(
            &dir.path().join("scripts/verify-release-static.sh"),
            VERIFY_SH.as_bytes(),
        );
        fs::create_dir(dir.path().join("binfmt-empty")).expect("binfmt dir");
        fs::create_dir(dir.path().join("bin")).expect("bin dir");
        ArchSandbox { dir }
    }

    fn script(&self) -> PathBuf {
        self.dir.path().join("scripts/verify-release-static.sh")
    }

    fn binfmt_empty(&self) -> PathBuf {
        self.dir.path().join("binfmt-empty")
    }

    /// A `qemu-<machine>-static` test double that records its argv and exits
    /// `exit_code`. The probe runs it under `env -i`, so it may use only
    /// shell builtins and an absolute log path.
    fn install_fake_emulator(&self, machine: &str, exit_code: i32) -> PathBuf {
        let log = self.dir.path().join("fake-qemu.log");
        let log_display = log.to_str().expect("log path").to_string();
        write_executable(
            &self
                .dir
                .path()
                .join("bin")
                .join(format!("qemu-{machine}-static")),
            &format!(
                r#"#!/bin/sh
# Test double for qemu-{machine}-static: record argv, report the outcome.
printf '%s\n' "$*" >> "{log_display}"
if [ "{exit_code}" -eq 0 ]; then
  echo "fake-qemu: executed $*"
else
  echo "fake-qemu: simulated failure of $*" >&2
fi
exit {exit_code}
"#
            )
            .into_bytes(),
        );
        log
    }

    /// An enabled-looking binfmt_misc registration for `machine`, printed in
    /// the shape the qemu registrations use, to wire in via BINFMT_MISC_DIR.
    fn install_fake_binfmt(&self, machine: &str) -> PathBuf {
        let d = self.dir.path().join("binfmt-fake");
        fs::create_dir_all(&d).expect("fake binfmt dir");
        let em = match machine {
            "x86_64" => "02003e", // ET_EXEC(02 00) + EM_X86_64(3e 00), LE
            "aarch64" => "0200b7", // ET_EXEC(02 00) + EM_AARCH64(b7 00), LE
            m => panic!("unsupported machine {m}"),
        };
        fs::write(
            d.join(format!("qemu-{machine}")),
            format!(
                "enabled\ninterpreter /usr/bin/qemu-{machine}-static\nflags: OC\n\
                 offset 0\nmagic 7f454c46020101000000000000000000{em}00\n\
                 mask ffffffffffffff00fffffffffffffffffeffffff\n"
            ),
        )
        .expect("write fake binfmt registration");
        d
    }

    /// Run the materialized validator against `artifact`. BINFMT_MISC_DIR is
    /// always pinned (empty, or a fake registration dir) so the outcome never
    /// depends on the host's real registrations; `fake_bin_on_path` prepends
    /// the sandbox bin/ so the fake emulator is the one found.
    fn run(&self, artifact: &Path, binfmt_dir: &Path, fake_bin_on_path: bool) -> (bool, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(self.script())
            .arg(artifact)
            .env("BINFMT_MISC_DIR", binfmt_dir);
        if fake_bin_on_path {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.path().join("bin").display(),
                    path
                ),
            );
        }
        let out = cmd
            .output()
            .expect("spawn bash on the sandboxed validator");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }
}

#[test]
fn host_arch_artifact_is_executed_in_a_scrubbed_environment() {
    let sb = ArchSandbox::new();
    let msg = b"cgov 0.1.2-arch-contract\n";
    let artifact = sb.dir.path().join("cgov-host");
    write_executable(&artifact, &artifact_for(host_machine(), msg, 0));

    let (ok, out) = sb.run(&artifact, &sb.binfmt_empty(), false);
    assert!(ok, "a good host-arch artifact must pass:\n{out}");
    for probe in ["--version", "--help"] {
        assert!(
            out.contains(&format!("{probe} in env -i, empty cwd, empty PATH")),
            "host-arch {probe} must run under the env -i probe:\n{out}"
        );
    }
    assert!(
        out.contains(&format!("ELF EXEC for {}", host_machine())),
        "the linkage checks must still run for the host artifact:\n{out}"
    );
    assert!(
        !out.contains("EXECUTION PROBE SKIPPED"),
        "the host architecture must never skip the probe:\n{out}"
    );
}

#[test]
fn host_arch_runtime_defect_is_caught_by_the_probe() {
    // Writes output, then exits 1: every linkage check passes, and the
    // execution probe must still fail the script — the defect class the
    // linkage checks cannot see (the reason the probe exists at all).
    let sb = ArchSandbox::new();
    let msg = b"cgov broken-main\n";
    let artifact = sb.dir.path().join("cgov-host-broken");
    write_executable(&artifact, &artifact_for(host_machine(), msg, 1));

    let (ok, out) = sb.run(&artifact, &sb.binfmt_empty(), false);
    assert!(
        !ok,
        "a host-arch artifact whose main exits non-zero must fail the script:\n{out}"
    );
    assert!(
        out.contains("--version in env -i, empty cwd, empty PATH: exit=1"),
        "the failure must be attributed to the execution probe:\n{out}"
    );
}

#[test]
fn foreign_arch_artifact_runs_under_an_emulator_when_one_exists() {
    let sb = ArchSandbox::new();
    let m = foreign_machine();
    let msg = b"cgov 0.1.2-foreign\n";
    let artifact = sb.dir.path().join("cgov-foreign");
    write_executable(&artifact, &artifact_for(m, msg, 0));
    let log = sb.install_fake_emulator(m, 0);

    let (ok, out) = sb.run(&artifact, &sb.binfmt_empty(), true);
    assert!(ok, "a foreign artifact with an emulator must pass:\n{out}");
    for probe in ["--version", "--help"] {
        assert!(
            out.contains(&format!("{probe} under qemu-{m}-static in env -i, empty cwd, empty PATH")),
            "foreign {probe} must run under the emulator:\n{out}"
        );
    }
    assert!(
        !out.contains("EXECUTION PROBE SKIPPED"),
        "the probe must not be skipped when an emulator exists:\n{out}"
    );
    // The fake must have been invoked ON the guest, both probes, in order —
    // proving the script hands the artifact to the emulator rather than
    // passing on any emulator output.
    let abs = artifact.canonicalize().expect("canonical artifact path");
    let log_text = fs::read_to_string(&log).expect("fake emulator log");
    let calls: Vec<String> = log_text.lines().map(str::to_string).collect();
    assert_eq!(
        calls,
        vec![format!("{} --version", abs.display()), format!("{} --help", abs.display())],
        "the emulator must be invoked on the guest artifact, both probes, in order"
    );
}

#[test]
fn foreign_arch_without_a_way_to_run_keeps_linkage_and_skips_only_the_probe() {
    let sb = ArchSandbox::new();
    let m = foreign_machine();
    let artifact = sb.dir.path().join("cgov-foreign-noemu");
    write_executable(&artifact, &artifact_for(m, b"cgov unreachable\n", 0));

    let (ok, out) = sb.run(&artifact, &sb.binfmt_empty(), false);
    // Full linkage checks ran for the foreign artifact regardless:
    assert!(
        out.contains(&format!("ELF EXEC for {m}")),
        "foreign artifact keeps the ELF check:\n{out}"
    );
    assert!(
        out.contains("no PT_INTERP (no dynamic loader)"),
        "foreign artifact keeps the PT_INTERP check:\n{out}"
    );
    assert!(
        out.contains("no NEEDED entries (no shared libraries)"),
        "foreign artifact keeps the NEEDED check:\n{out}"
    );
    if host_has_emulator(m) {
        // This host can execute it after all: execution must happen, not skip.
        assert!(
            ok && !out.contains("EXECUTION PROBE SKIPPED"),
            "with an emulator on PATH the foreign artifact must be executed:\n{out}"
        );
    } else {
        assert!(
            ok,
            "linkage-only evidence must still pass the script:\n{out}"
        );
        assert!(
            out.contains("EXECUTION PROBE SKIPPED"),
            "the skip must be loud — a linkage-only artifact must say so:\n{out}"
        );
        assert!(
            !out.contains("--version in env -i") && !out.contains("--version under"),
            "no execution probe may run when there is no way to run it:\n{out}"
        );
    }
}

#[test]
fn binfmt_registration_makes_the_script_execute_rather_than_skip() {
    let sb = ArchSandbox::new();
    let m = foreign_machine();
    let artifact = sb.dir.path().join("cgov-foreign-binfmt");
    write_executable(&artifact, &artifact_for(m, b"cgov via-binfmt\n", 0));
    let fake_binfmt = sb.install_fake_binfmt(m);

    let (ok, out) = sb.run(&artifact, &fake_binfmt, false);
    assert!(
        out.contains("under binfmt_misc"),
        "a registration must make the script attempt execution, not skip:\n{out}"
    );
    assert!(
        !out.contains("EXECUTION PROBE SKIPPED"),
        "a binfmt registration is a way to run — never a skip:\n{out}"
    );
    // Whether the exec itself succeeds depends on whether the KERNEL really
    // has the registration (the fake dir only steers the script's check): on
    // a truly binfmt-enabled host the guest runs and the script passes;
    // elsewhere exec fails (126) and the script fails loudly. Both are the
    // contract — what is pinned here is that a registration is never a skip.
    if ok {
        assert!(out.contains("exit=0"), "executed guest must report exit 0:\n{out}");
    } else {
        assert!(
            out.contains("FAIL:"),
            "a failed direct execution must fail the script loudly:\n{out}"
        );
    }
}

#[test]
fn foreign_arch_probe_failure_fails_the_script() {
    // The cross-architecture twin of the broken-main test: with an emulator
    // in play, a failing guest fails the script — this is what makes cgov-ci's
    // emulated arm64 probe load-bearing rather than advisory.
    let sb = ArchSandbox::new();
    let m = foreign_machine();
    let artifact = sb.dir.path().join("cgov-foreign-broken");
    write_executable(&artifact, &artifact_for(m, b"cgov broken-foreign\n", 3));
    sb.install_fake_emulator(m, 3);

    let (ok, out) = sb.run(&artifact, &sb.binfmt_empty(), true);
    assert!(
        !ok,
        "an emulated probe exiting non-zero must fail the script:\n{out}"
    );
    assert!(
        out.contains(&format!("--version under qemu-{m}-static in env -i")),
        "the failure must be attributed to the emulated probe:\n{out}"
    );
    assert!(
        out.contains("exit=3"),
        "the guest's own exit code must surface:\n{out}"
    );
}
