# Bead bf-1fwer: Add aarch64 arm64 binary to release

## Task
Add aarch64/arm64 binary support to Argo cgov-ci workflow

## What was done

### 1. Updated cgov-ci workflow (declarative-config)
The workflow was already updated in commit 3840d7a on 2026-06-27 to include:
- Cross-compilation toolchain: gcc-aarch64-linux-gnu, binutils-aarch64-linux-gnu
- Cargo config for aarch64-unknown-linux-musl target
- Build steps for both x86_64-unknown-linux-musl and aarch64-unknown-linux-musl
- Upload of both cgov-linux-amd64 and cgov-linux-arm64 binaries to releases

This commit was pushed to declarative-config main branch.

### 2. Version bump
Bumped version from 0.1.0 → 0.1.1 in Cargo.toml to trigger a new release.

### 3. Next steps
- Commit and push this version bump
- The cgov-ci workflow will automatically create a new v0.1.1 release
- This release will include both amd64 and arm64 binaries
- The install.sh script already supports both architectures

## Verification
Once v0.1.1 is released, users can install on arm64 systems:
```bash
curl -fsSL https://raw.githubusercontent.com/jedarden/claude-governor/main/install.sh | bash
```

This will download cgov-linux-arm64 on aarch64 systems.

## Workflow changes
The cgov-ci workflow now builds both targets sequentially:
1. x86_64-unknown-linux-musl (amd64)
2. aarch64-unknown-linux-musl (arm64)

Both binaries are uploaded to the GitHub release with SHA256 checksums.
