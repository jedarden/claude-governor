# Release publication gate

Bead: claudego-78c221be (2026-09-24). Closes the gap between the two earlier
release-integrity layers — `scripts/verify-release-static.sh`
(claudego-6326b06e, artifact linkage) and `install.sh`'s digest verification
(claudego-4531120c, consumer side) — by owning the publish decision itself.

## The problem

Before this gate, the release sequence lived inline in the `cgov-ci`
WorkflowTemplate: build, generate sidecars with `sha256sum >`, push the tag,
`gh release create`. Nothing verified the three claims the README's
provenance story depends on:

1. **Provenance** — that the published artifacts were built from the commit
   the `v<VERSION>` tag names *on Forgejo*. A stale or missing tag, or a
   checkout whose origin is the GitHub mirror, would have published anyway.
2. **Per-architecture static validity** — the static gate ran, but nothing
   enforced that BOTH architectures were present and valid before publishing;
   a future edit dropping the arm64 build would have produced a
   half-release.
3. **Sidecar integrity** — sidecars were generated, never validated. A
   truncated sidecar, a sidecar naming the wrong artifact, or a digest that
   doesn't match the artifact bytes would only surface at install time on
   some user's machine.

And crucially, nothing ordered these checks BEFORE the publish: a failure
anywhere had to be caught by hand instead of refusing the release.

## What `scripts/publish-release.sh` does

Fail-closed, in order; any failure exits 1 with nothing uploaded:

1. **Provenance.** The release dir must be a git checkout whose `origin`
   canonicalizes to the Forgejo source of truth (https/ssh/scp forms
   equivalent). The tag must exist, `^{commit}` must equal HEAD, and
   `git ls-remote` against the Forgejo URL must resolve the tag (peeling
   annotated tags via `^{}`) to the same commit. Forgejo is the source of
   truth; the GitHub mirror is never consulted for provenance.
2. **Artifacts.** `cgov-linux-amd64` and `cgov-linux-arm64` must both exist —
   the two architectures `install.sh` supports. A missing architecture is a
   failure, not a partial release.
3. **Static.** Each artifact goes through the real
   `scripts/verify-release-static.sh`. Since claudego-8af2d72b the
   foreign-arch artifact executes its probe too whenever the host has a way
   to run it (an emulator on PATH or a binfmt registration) — `cgov-ci`
   installs `qemu-user-static`, so there the arm64 artifact genuinely runs
   both smoke probes — and keeps the full linkage checks either way.
4. **Sidecars.** Each artifact needs `<artifact>.sha256`, exactly one line,
   `<64-hex>␠␠<artifact>` — the `sha256sum -c` shape `install.sh` consumes —
   with a digest equal to the artifact's actual sha256. Sidecars are
   validated, never generated: the build step writes them, the gate refuses
   to publish unless they are right. That is what "immutable" means here —
   a published digest is fixed once the release is cut, so the gate checks
   it instead of silently rewriting it.

Then, and only then: `gh release create` with BOTH binaries and BOTH
sidecars, followed by a post-publish pairing check — the release's own asset
list is re-fetched and every asset must pair with its `.sha256` (a lone
sidecar fails too). The release already exists at that point, so this cannot
un-publish it; it fails the CI run loudly instead, because a
live-but-incomplete release must never pass silently.

`--dry-run` runs phases 1–4 and stops before any gh call.

## CI wiring (owed follow-up)

`cargo test` — which `cgov-ci` already runs as its first release step — pins
the whole contract through `tests/release_publication_gate_test.rs`, so the
tests ARE release CI. The remaining wiring step lives in
`declarative-config/k8s/iad-ci/argo-workflows/cgov-ci.yaml`: replace the
template's inline sidecar-generation + `gh release create` block with a call
to `scripts/publish-release.sh --version "v${VERSION}"` after its existing
tag push (the sidecar generation itself stays in the template, before the
gate). That file is outside a claude-governor dispatch's write scope, so it
is recorded here and on the bead rather than edited alongside. Until that
call lands, the gate is exercised by `cargo test` and can be run manually:

```bash
scripts/publish-release.sh --version v0.1.x --dry-run   # rehearse, no publish
scripts/publish-release.sh --version v0.1.x             # validate + publish
```

## How the tests fake the world

`tests/release_publication_gate_test.rs` embeds BOTH scripts with
`include_str!` (the repo's gate pattern — the tested text cannot drift from
the shipped one, and nothing is read from `CARGO_MANIFEST_DIR` at run time,
which matters because close-gate extractions are deleted). Per test it
builds:

- a one-commit git checkout whose `origin` is a local bare repo standing in
  for Forgejo, tag pushed there;
- release artifacts that are **hand-assembled static ELF64 binaries** —
  x86-64 and AArch64, ~150 bytes, no PT_INTERP, no dynamic section, real
  `write`/`exit` syscalls — so the static-validation phase is the genuine
  validator, not a mock. The host-arch artifact actually executes under the
  script's `env -i` probe; the foreign one executes too when the host has an
  emulator for it (claudego-8af2d72b) and otherwise exercises the skip path;
- a recording `gh` fake on `PATH` that logs its argv (newlines collapsed —
  the gate's `--notes` span lines) and answers the post-publish asset query
  from a scenario file.

Every refusal test asserts the `gh release create` call was never logged —
the strongest available form of "publication fails". Covered: happy path
(exactly one create with all four assets, asset query after create), dry-run
(no gh call at all), missing sidecar, tampered digest, sidecar naming the
wrong artifact, malformed sidecar shape, foreign-arch artifact failing static
validation, missing foreign-arch artifact, tag behind HEAD, tag absent from
Forgejo, origin not Forgejo, post-publish sidecar gap, bare-version
normalization, and the Forgejo-default pin.
