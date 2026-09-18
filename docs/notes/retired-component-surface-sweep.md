# Retired-component surface sweep

**Bead:** claudego-8e03c5b4 (split child of claudego-ff2b3462, under umbrella
claudego-96e3b6a2) · **Verified:** 2026-09-18

The standalone polish queue, its timer and seeder, and the subscription
generator pool were retired on 2026-09-16 (CLAUDE.md, "Retired 2026-09-16").
The rule is enforced mechanically — `RETIRED_REFERENCE_MARKERS` rejection at
config load (`src/config.rs`), doctor's `retired_component_refs` /
`retired_component_units` checks (`src/doctor.rs`), and the init unit sweep
(`src/main.rs`) — but enforcement alone does not prove the *surfaces* an
operator actually sees stay clean. This note documents the end-to-end sweep
that does.

## The automated sweep

`tests/retired_component_surface_sweep.rs` runs the real `cgov` binary and
asserts all three surfaces at once:

```bash
cargo test --test retired_component_surface_sweep
```

1. **Fresh install** — `cgov init --no-systemd` under a temp
   `XDG_CONFIG_HOME`/`HOME` writes config with zero retired references,
   prints none, and creates no systemd unit directory; `cgov doctor --json
   --skip-live` on that fresh install passes both retired-component checks,
   carries no remediation for them, and its entire JSON report contains zero
   markers.
2. **Failure directions** — doctor run against a poisoned config (an
   `agents:` pool named after the retired queue) and against planted
   `claude-polish-seeder.service`/`.timer` files must fail with a remediation
   that demands removal and bars recreation. A word-boundary, negation-aware
   detector over the remediation text asserts no install/recreate/re-enable
   offer; its own semantics are pinned by a self-test.
3. **File sweep** — every template init embeds (`config/`) and every script
   under `deploy/` is compiled into the test via `include_str!` and scanned
   for `RETIRED_REFERENCE_MARKERS`. Compiled-in bytes, not runtime reads of
   the source tree, so the sweep is valid from a shared-cache test binary in
   a clean extraction. When a template or deploy script is added, add it to
   `SWEPT_FILES` in the test.

## Manual reproduction

```bash
# 1. Fresh init + doctor under a throwaway XDG root
TMP=$(mktemp -d)
env HOME="$TMP" XDG_CONFIG_HOME="$TMP/config" XDG_DATA_HOME="$TMP/data" \
    XDG_STATE_HOME="$TMP/state" XDG_CACHE_HOME="$TMP/cache" \
    cgov init --no-systemd
# expect: exit 0, no retired-component setup in "Actions taken"
env HOME="$TMP" XDG_CONFIG_HOME="$TMP/config" XDG_DATA_HOME="$TMP/data" \
    XDG_STATE_HOME="$TMP/state" XDG_CACHE_HOME="$TMP/cache" \
    cgov doctor --json --skip-live | python3 -m json.tool
# expect: retired_component_refs -> pass, retired_component_units -> pass,
#         zero occurrences of polish / generator-pool / generator_pool
rm -rf "$TMP"

# 2. Grep sweep over the embedded templates and deploy scripts
grep -rniE 'polish|generator[-_]pool|seeder' config/ deploy/
# expect: no matches (exit 1)
```

## Where retired references are still allowed to live

A raw grep over `src/` hits files the sweep deliberately does not treat as
violations — the enforcement machinery must name what it enforces against:

| File | Why it mentions retired components |
|---|---|
| `src/config.rs` | `RETIRED_REFERENCE_MARKERS`, `find_retired_references`, `reject_retired_references` + their tests |
| `src/doctor.rs` | `retired_component_refs` / `retired_component_units` checks + their negative tests |
| `src/main.rs` | `RETIRED_POLISH_UNITS` sweep in init, plus tests pinning template/sweep behaviour |
| `src/governor.rs` | test fixtures only |

Any marker occurrence outside those files — or reaching init output, doctor
output, or a template — is a regression the automated sweep fails on.

## Recorded evidence (2026-09-18)

- `cargo test --test retired_component_surface_sweep` — all six tests pass
  from a clean extraction of the committed tree (also re-verified by
  NEEDLE's close gate).
- Manual dry runs of the same commands: init exit 0 with only
  `governor.yaml` written; doctor fresh run `passed: 14, warned: 9,
  failed: 4` — all four failures environmental (no OAuth credentials, API
  429, no state file) — with both retired checks passing and zero markers in
  the report; poisoned-config and planted-unit runs flag with the
  removal-only remediations quoted in the tests.
- `install.sh`, `Makefile`, `README.md` were additionally grepped by hand:
  zero retired-component references.
