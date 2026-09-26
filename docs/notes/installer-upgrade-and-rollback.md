# Installer upgrade and rollback behavior

Bead: claudego-765c10d9 (2026-09-26). Specifies what re-running
`install.sh` does to an existing installation — what is preserved, what is
replaced, what an interruption or failed verification leaves behind, and how
to get back to a prior verified version. Every claim here is pinned by an
integration test; the test map is at the end.

## An upgrade is the same operation as an install

There is no upgrade mode. `install.sh` is idempotent: re-running it — for
the latest release or a pinned one — downloads, verifies, and installs the
artifact over whatever is currently at `${INSTALL_DIR}/cgov`. A "downgrade"
is the identical operation with `CGOV_VERSION` pointing at an older tag; the
installer never compares versions and never refuses to move backwards.

## What an upgrade replaces — exactly one file

The installer writes exactly one path: `${INSTALL_DIR}/cgov`
(default `~/.local/bin/cgov`, `--install-dir`/`CGOV_INSTALL_DIR`), created
with mode `0755` via `install -m 0755`. `mkdir -p` on the install dir runs
only after verification passes. Nothing else in the install dir is read,
written, or removed — unrelated files there survive every install, upgrade,
refusal, and interruption untouched.

## What an upgrade preserves — configuration, state, units, adapters

The installer does not read or write anything under
`~/.config/claude-governor/`. Specifically, all of this survives every
upgrade byte-for-byte, mode-for-mode:

| Preserved | Path | Why |
|---|---|---|
| Configuration | `~/.config/claude-governor/governor.yaml` | Only `cgov init` seeds it (`--force` overwrites); the installer never touches it |
| Daemon state | `~/.config/claude-governor/governor-state.json` | Written by the running daemon (learned calibration, forecasts); an upgrade must not discard it |
| systemd user units | `~/.config/systemd/user/claude-governor-*` | Installed by `cgov init`/`cgov enable`; they exec `%h/.local/bin/cgov`, so a replaced binary is picked up on the next service (re)start with no unit changes |
| NEEDLE adapters | `~/.config/needle/adapters/claude-print-*.yaml` | Installed by `deploy/install-claude-print-adapters.sh`, not by `install.sh` |
| Everything else in the install dir | `${INSTALL_DIR}/*` (except `cgov`) | The installer's only write target is the one file above |

Consequence: a running daemon keeps executing the old binary until restarted
(systemd restarts the exec'd path only on unit start), and a rollback needs
no config surgery — same config, older binary.

## Verification precedes every write

The download and its digest verification happen inside a `mktemp -d` work
dir; the install dir is first touched — `mkdir -p` included — only after
verification passes. Every refusal therefore leaves an existing installation
byte-identical and a fresh target not even created:

| Failure | Refusal message | Effect on disk |
|---|---|---|
| Artifact download fails (404, 500, unreachable server, **truncated/interrupted transfer**) | `Download failed: …` | Nothing written; work dir removed by the `EXIT` trap |
| Sidecar missing (404/500) | `Digest sidecar download failed: …` | Nothing written |
| Sidecar content fails `sha256sum -c` (tampered, garbage, names another artifact) | `Checksum verification FAILED … Nothing was written to ${INSTALL_DIR}.` | Nothing written |
| Caller digest pin mismatch | `Checksum MISMATCH … Refusing to install. Nothing was written to ${INSTALL_DIR}.` | Nothing written |
| Malformed digest pin | rejected before any download | Nothing written |

Because the refusal paths never touch the install dir, a refused upgrade
leaves the **prior version installed and still executable** — the safe
state to investigate from.

## Interruption semantics

Phase by phase, what an upgrade interrupted at each point leaves:

1. **During resolution/download (the long phase).** The transfer targets the
   temp work dir. A kill, a dropped connection, or a truncated body aborts
   the run; the partial file never leaves the work dir (removed by the
   `EXIT` trap when the script exits, leaked as an orphan `mktemp` dir only
   if the process is killed outright — `/tmp` litter, never install damage).
   The existing installation is untouched and the prior version keeps
   running.
2. **During verification.** Refusal table above; nothing written.
3. **During the final copy.** `install -m 0755` writes the destination in
   place rather than staging a same-directory rename, so there is a
   millisecond-scale window in which a `SIGKILL` mid-copy can truncate
   `cgov` itself. The window is the only non-atomic step in the script, it
   is bounded by the artifact size (a few MiB), and recovery is the same
   operation as everything else here: re-run the installer (any version,
   pinned or latest). It overwrites the broken file unconditionally.

## Rollback: re-run the installer pinned to a prior verified version

Rollback and repair are the same operation — the installer overwrites
whatever is installed, including a binary that will not execute:

```bash
# 1. Find the digest of the version you trust (each release publishes one
#    sidecar per artifact):
curl -fsSL "https://github.com/jedarden/claude-governor/releases/download/v0.1.1/cgov-linux-amd64.sha256"

# 2. Reinstall the prior release, tag- and digest-pinned:
curl --netrc -fsSL https://git.ardenone.com/jedarden/claude-governor/raw/branch/main/install.sh \
  | CGOV_VERSION=v0.1.1 CGOV_SHA256=<64-hex-digest> bash
```

The digest pin is what makes it a rollback to a *verified* version: with
`CGOV_SHA256` set, the sidecar is never consulted (a compromised or lagging
mirror cannot alter the bytes), and a mismatch aborts pre-write — a failed
rollback attempt leaves the currently-installed version running, so you are
never worse off than before the attempt. `CGOV_VERSION` alone also works
(sidecar-verified) when exact-byte pinning is not required.

Then verify by property, not by trust: `cgov version` (or
`~/.local/bin/cgov --version`) must report the rolled-back release, and
restart the units (`cgov enable` or `systemctl --user restart
claude-governor-observe`) so the daemon stops executing the newer binary.
A broken-but-digest-valid release (the artifact passed checksum yet will not
execute) is the case rollback exists for: the installer reports
`(version check failed)` in its post-install probe — best-effort,
non-fatal by design, since the bytes *are* the verified release — and the
rollback above replaces the dead binary without any manual deletion.

## Test map

| Guarantee | Pinned by |
|---|---|
| Upgrade over an existing install replaces only `cgov`; config, state, and unrelated files survive byte-for-byte | `tests/installer_upgrade_rollback_test.rs::upgrade_over_existing_install_replaces_only_the_binary` |
| Interrupted (truncated) transfer aborts pre-write and the prior version still executes | `tests/installer_upgrade_rollback_test.rs::interrupted_transfer_aborts_upgrade_and_prior_version_keeps_working` |
| Digest-valid but non-executable artifact installs and reports the failed probe without damaging the install dir | `tests/installer_upgrade_rollback_test.rs::verified_but_non_executable_artifact_installs_and_reports_the_failed_probe` |
| Rollback to a prior tag with a caller digest pin restores a working binary without consulting the sidecar | `tests/installer_upgrade_rollback_test.rs::rollback_to_a_prior_pinned_version_restores_the_working_binary` |
| Refusal paths (tampered/missing/garbage sidecar, wrong pin, network failures) preserve an existing install; fresh dir never created | `tests/install_sh_release_verification.rs`, `tests/release_install_integration_test.rs` |
| Mode-0755 install, bare-version normalization, pinned-digest install skips the sidecar | `tests/install_sh_release_verification.rs` |
| Post-install probe is informational only (`(version check failed)` does not fail the run) | `tests/installer_upgrade_rollback_test.rs::verified_but_non_executable_artifact_installs_and_reports_the_failed_probe` |
