# Adapter variable-list sync gates — four-way drift drill

Recorded 2026-09-18 (claudego-6664cc27, parent claudego-efe591bd).

Two gates keep the installer's bash variable lists (`RULE3_IDE_VARS` /
`RULE5_API_VARS` in `deploy/install-claude-print-adapters.sh`) set-identical
to the canonical Rust constants (`IDE_ENV_VARS` / `API_ROUTING_ENV_VARS` in
`src/adapter_verify.rs`):

| Gate | Where it runs | Failure mode |
|---|---|---|
| **cargo-test parse gate** — `installer_bash_variable_lists_match_the_rust_constants` | every `cargo test` (the installer is `include_str!`ed into `src/adapter_verify.rs`) | panic, exit 101 |
| **bash gate** — `check_var_list_sync` (installer section 4) | every install run, before anything is trusted | `Install incomplete.`, exit 1 |

A third, passive layer — the pinning tests in `tests/adapter_var_sync.rs`
(claudego-385e691f) — extracts the bash gate into a sandbox and asserts it
still detects drift; it *reports* the same drift text as the bash gate.

This drill applied four single-sided mutations to a `git archive HEAD`
extraction in a fresh temp dir (never mutate-and-revert the shared checkout)
and captured what each gate printed. The installer runs below used a
sandboxed `$HOME` (a hardlinked `claude-print` under `$HOME/.cargo/bin`,
since the installer rejects symlinked candidates) so its adapter installs
landed in the sandbox, not in `~/.config/needle/adapters`. Cargo runs used
`~/.cargo/bin/cargo` with an isolated `CARGO_TARGET_DIR`. The scratch copy
was byte-compared against HEAD after the drill and deleted.

Drill variables: added `VSCODE_DRILL_PROBE` (rule-3 list), removed
`VSCODE_CWD` (rule-3 list), removed `ANTHROPIC_SMALL_FAST_MODEL` (rule-5
list).

---

## Mutation 1 — add a variable to the Rust constants

`IDE_ENV_VARS += "VSCODE_DRILL_PROBE"` in `src/adapter_verify.rs`.

**cargo-test gate — exit 101:**

```text
thread 'adapter_verify::tests::installer_bash_variable_lists_match_the_rust_constants' (3725234) panicked at src/adapter_verify.rs:798:13:
RULE3_IDE_VARS in deploy/install-claude-print-adapters.sh diverges from IDE_ENV_VARS in src/adapter_verify.rs — the bash list is missing ["VSCODE_DRILL_PROBE"] and carries unlisted []. Update both copies together; the installer's own sync check fails the install on the same drift.
```

(A Rust-side addition additionally fails `committed_adapter_templates_scrub_both_variable_sets`,
because no committed template unsets the new variable yet — the scrub check
treats an unscrubbed required var as a template regression. Same source line,
different complaint.)

**bash gate — `Install incomplete.`, exit 1:**

```text
  ✗ variable-list drift: RULE3_IDE_VARS (this script) vs IDE_ENV_VARS (src/adapter_verify.rs)
      4d3
      < VSCODE_DRILL_PROBE
```

**pinning test** `clean_tree_sync_gate_passes` — exit 101, panicking with the
same drift report as the bash gate.

## Mutation 2 — remove a variable from the Rust constants

`IDE_ENV_VARS -= "VSCODE_CWD"`.

**cargo-test gate — exit 101:**

```text
RULE3_IDE_VARS in deploy/install-claude-print-adapters.sh diverges from IDE_ENV_VARS in src/adapter_verify.rs — the bash list is missing [] and carries unlisted ["VSCODE_CWD"]. Update both copies together; the installer's own sync check fails the install on the same drift.
```

**bash gate — exit 1:**

```text
  ✗ variable-list drift: RULE3_IDE_VARS (this script) vs IDE_ENV_VARS (src/adapter_verify.rs)
      2a3
      > VSCODE_CWD
```

**pinning test** `clean_tree_sync_gate_passes` — exit 101, same drift text.

## Mutation 3 — add a variable to a bash array

`RULE3_IDE_VARS += VSCODE_DRILL_PROBE` in the installer.

**bash gate — exit 1:**

```text
  ✗ variable-list drift: RULE3_IDE_VARS (this script) vs IDE_ENV_VARS (src/adapter_verify.rs)
      3a4
      > VSCODE_DRILL_PROBE
```

**cargo-test gate — exit 101:**

```text
RULE3_IDE_VARS in deploy/install-claude-print-adapters.sh diverges from IDE_ENV_VARS in src/adapter_verify.rs — the bash list is missing [] and carries unlisted ["VSCODE_DRILL_PROBE"]. Update both copies together; the installer's own sync check fails the install on the same drift.
```

**pinning test** `clean_tree_sync_gate_passes` — exit 101, same drift text.

## Mutation 4 — remove a variable from a bash array

`RULE5_API_VARS -= ANTHROPIC_SMALL_FAST_MODEL` in the installer.

**bash gate — exit 1:**

```text
  ✗ variable-list drift: RULE5_API_VARS (this script) vs API_ROUTING_ENV_VARS (src/adapter_verify.rs)
      8d7
      < ANTHROPIC_SMALL_FAST_MODEL
```

**cargo-test gate — exit 101:**

```text
RULE5_API_VARS in deploy/install-claude-print-adapters.sh diverges from API_ROUTING_ENV_VARS in src/adapter_verify.rs — the bash list is missing ["ANTHROPIC_SMALL_FAST_MODEL"] and carries unlisted []. Update both copies together; the installer's own sync check fails the install on the same drift.
```

**pinning tests — exit 101, two failures.** `clean_tree_sync_gate_passes`
with the same drift text, and `sync_gate_flags_a_removed_bash_variable`,
whose own mutation (removing the same variable) is now a no-op and trips its
"pick a variable the array actually declares" assertion. That second failure
is an artifact of the drill compounding onto an already-mutated list, not a
separate gate.

---

## How to read these signatures

- **The cargo-test gate's message is written from the bash list's
  perspective**, always relative to the Rust constants *as they now are*:
  "missing" = in the Rust constants, absent from the bash array; "carries
  unlisted" = in the bash array, absent from the Rust constants. It names
  both files and both list names, but it does **not** tell you which side
  was edited — a Rust-side addition (M1) and a bash-side removal (M4)
  produce the identical `missing` clause shape.
- **The bash gate's embedded `diff` does name the direction.** It diffs the
  sorted Rust list (file 1) against the sorted bash list (file 2), so:
  `<` = only in the Rust constants, `>` = only in the bash array — the same
  decoding for all four mutations. The `a`/`d` letter is mechanical
  (append/delete), not semantic.
- **Both gates are set comparisons** (`sort -u` / `BTreeSet`): element order
  and duplicates never trip them; only real membership differences do.
- **Exit codes are unambiguous**: 101 (panic) for anything under
  `cargo test`, 1 with `Install incomplete.` for the installer. A
  mutation that changes nothing (the drill's first M4 attempt — a sed that
  greedy-matched to a no-op) leaves every gate green, so a silent drill run
  means the mutation never landed, not that a gate is blind.
- **Cross-coverage is total**: each gate reads both files (the test via
  `include_str!`, the installer via `${REPO_DIR}/src/adapter_verify.rs`), so
  every one-sided edit is caught twice — by the commit-time gate and by the
  runtime gate — and the pinning tests exist to keep the runtime gate
  itself honest.
- The `\x1b[0;31m` escapes around `✗` in raw pinning output are the
  installer's `${RED}`; the text above is color-stripped.
