# Release static-linkage validation

Bead: claudego-6326b06e (2026-09-24). Enforces the README's
"Zero runtime dependencies — Single statically-linked binary" claim on real
release artifacts instead of trusting it.

## What is checked, and why these checks

`scripts/verify-release-static.sh` is the single source of truth. For each
binary it requires:

1. **ELF executable, x86-64 or AArch64** — the platforms `install.sh`
   supports. Anything else fails rather than silently passing.
2. **No `PT_INTERP` program header.** This is the check that matters: a
   `PT_INTERP` segment is the binary naming a dynamic loader
   (`/lib64/ld-linux-x86-64.so.2`). No segment means nothing ever loads
   another file to run the program.
3. **No `NEEDED` entries in `.dynamic`.** No shared library is requested at
   startup. Checks 2+3 together are the precise definition of "statically
   linked"; `file(1)`'s wording is a convenience summary, not a contract.
4. **`file(1)` cross-check** (optional): fails if file(1) ever says
   "dynamically linked", but its absence is only a NOTE — some hosts (this
   NixOS box, the debian build container) have no `file` package, and the
   readelf checks carry the guarantee.
5. **Execution probe** (host architecture only): `--version` and `--help`
   must exit 0 with output when run as
   `env -i PATH=<empty-dir> HOME=<empty-dir> <binary>` from an empty working
   directory. That is a genuinely minimal environment: no environment
   variables, no PATH to resolve helpers through, no project files to read.
   clap's `--version`/`--help` paths are config-free, so nothing but process
   startup, the runtime, and argv parsing is exercised. Foreign-architecture
   binaries skip the probe (readelf/file are cross-arch; the linkage
   guarantee still fully applies) — this is what lets `cgov-ci` validate
   `cgov-linux-arm64` from its amd64 container.

A gnu-target release binary fails checks 2 and 3 (it carries `PT_INTERP` and
NEEDED entries for `libgcc_s`/`libm`/`libc`) — verified as a negative control.
The musl target is load-bearing for the promise; a plain
`cargo build --release` does NOT satisfy it.

## Where it runs

- **`make verify-release`** — builds
  `--target $MUSL_TARGET` (default `x86_64-unknown-linux-musl`) then
  auto-discovers the artifact (via `cargo metadata`, so a redirected target
  dir is honored) and validates it.
- **`cgov-ci`** (`declarative-config/k8s/iad-ci/argo-workflows/cgov-ci.yaml`)
  — runs the script over `cgov-linux-amd64` and `cgov-linux-arm64` as a
  release gate before the tag is pushed or anything is published.
- **`cargo test`** — `tests/release_static_validation_test.rs` validates any
  release artifact already on disk, and SKIPS (loudly) when none exists so
  clean extractions stay green. Discovery probes `cargo metadata` three ways:
  each cargo on PATH (which may be the fleet wrapper), the real
  `~/.cargo/bin/cargo`, and the real cargo with `CARGO_TARGET_DIR` removed —
  the last matters because the wrapper redirects `cargo test` builds to
  `/build/<repo>` (needle-d6b685b4) while plain `cargo build` artifacts follow
  the configured target-dir. `CGOV_RELEASE_BIN` overrides discovery entirely.
  `make verify-release` remains the authoritative end-to-end path because it
  builds first.

## Building the musl binary on codinghome

The box has no `musl-gcc`, and `libsqlite3-sys` (bundled SQLite) needs one to
compile C for the musl target. `sudo` is unavailable, but nix can provide the
compiler — pass it to cc-rs by absolute path and run cargo in the **normal**
shell:

```bash
export CC_x86_64_unknown_linux_musl="$(nix-shell -p musl --run 'command -v musl-gcc' 2>/dev/null)"
export AR_x86_64_unknown_linux_musl=ar
cargo build --release --target x86_64-unknown-linux-musl
```

Do **not** run the whole build inside `nix-shell -p musl`: the stdenv setup
poisons host-side linking, and every build script (proc-macros, etc.) fails
with undefined `open64`/`stat64`/`mmap64` symbols because `-lc` resolves
against musl. `cargo` itself must stay outside; only the C compiler for the
musl target points in.

(`cgov-ci` has none of this: its debian container installs `musl-tools`,
which puts `musl-gcc` on PATH.)
