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
//!   5. on the host architecture, runs `--version` and `--help` with output
//!      under `env -i` from an empty working directory with PATH pointed at
//!      an empty directory.
//!
//! When no release artifact is built, the test SKIPS with a loud note so
//! plain `cargo test` stays green in clean extractions; `make verify-release`
//! is the authoritative end-to-end invocation because it builds the artifact
//! first. The script is the single source of truth for what "static" means
//! here — this test only wires it into `cargo test`.

use std::path::PathBuf;
use std::process::Command;

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
