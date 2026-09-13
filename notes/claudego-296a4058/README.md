# Preserved working-tree patches (triage claudego-296a4058)

On 2026-09-12 the working tree was reverted to HEAD so the trusted `cargo test`
gate could run against a known state. Everything reverted was first preserved
here. Apply with `git apply <patch>` from the repo root.

## claudego-1942b4ea-guard-work.patch

`src/governor.rs` + `src/burn_rate.rs` — a complete-shaped implementation of
bead **claudego-1942b4ea** ("Guard zero-worker burn so it cannot drive
safe_worker_count to 0 or raise fleet CUTOFF_RISK"): two new public helpers
(`effective_fleet_pct_rate`, `per_worker_pct_for_sizing`), the zero-worker
EMA-skip guard in `run_observe_cycle`, their call-site wiring, and six unit
tests (two in burn_rate, four in governor), all labeled with the bead ID.

Provenance: written by a timed-out dispatch of worker `glm-cgraph` on that
bead (attempt 01a09685, resolved `indeterminate/timeout` 2026-09-12T17:49Z,
never committed). This patch is the starting point for whoever claims
claudego-1942b4ea next; test results observed at write time are recorded on
that bead's successor triage record (claudego-296a4058 notes).

## rustfmt-only-reflow.patch

`src/db.rs`, `src/poller.rs`, `tests/apportioning_test.rs`,
`tests/pluck_filter_combinations_test.rs`,
`tests/pluck_workspace_mismatch_test.rs`,
`tests/test_workspace_path_formats.rs` — pure `rustfmt` output (line
rewrapping, import ordering, trailing-comma/comment alignment). No semantic
change; safe to drop or regenerate with `cargo fmt` at any time.

## pluck-debug-log-deletions.patch

Deletion of three stale pluck-debug logs committed in earlier sessions
(`docs/notes/pluck-starvation-pluck-output.log`, `notes/bf-56wnh-pluck-debug.log`,
`notes/pluck-debug-output-full.log`). The deletions were never committed by
whoever made them; the files remain in git history. Re-applying this patch
re-does that cleanup as a committable change.
