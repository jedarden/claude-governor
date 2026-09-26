# Claude Governor — Operating Guide

Claude Governor (`cgov`) is a capacity governor: it polls Claude subscription usage,
forecasts whether each window will exhaust before it resets, and scales a NEEDLE
worker fleet to fit.

> **Retired 2026-09-16:** the standalone polish queue, timer, seeder, and
> subscription generator pool were removed. NEEDLE's native Weave/Explore strands
> now rebalance organically: a worker that cannot find actionable work may create
> scoped work in a configured repository. Do not recreate `cgov-polish-queue` or
> install the former `claude-polish-seeder` units.

---

## 1. Components & where they live

| Component | Location | Notes |
|---|---|---|
| `cgov` binary | `~/.local/bin/cgov` | built from this repo (`cargo build --release`, target redirects to `~/target/release/cgov`) |
| Governor config | `~/.config/claude-governor/governor.yaml` | agents, daemon, pricing; **not** in the repo (machine-specific). The repo's `config/governor.yaml` is only the checked-in **seed template** — `src/config.rs` bakes it in with `include_str!` and copies it to the live path on first run (`GovernorConfig::config_paths`/`create_default_config`). Edit live values on the machine path and verify with `cgov config`; never cite the repo copy as running configuration (claudego-3648483e) |
| `claude-print` binary | `~/.local/bin/claude-print` | PTY wrapper that keeps sessions on the subscription pool. **Establish this path with `deploy/install-claude-print-adapters.sh`** — the adapters call it by absolute path and nothing else creates it (claudego-49195ba4) |
| NEEDLE adapters | `~/.config/needle/adapters/claude-print-{opus,fable}.yaml` | copies committed under `deploy/needle-adapters/` |

> ⚠️ **The live NEEDLE adapters directory is `~/.config/needle/adapters/`, NOT
> `~/.needle/agents/`.** The latter is a stale staging path (`claude-print`'s
> installer writes there) that the current `needle` binary does **not** read.

---

## 2. The claude-print adapters (`deploy/needle-adapters/`)

Install with:

```bash
./deploy/install-claude-print-adapters.sh
```

It copies the adapters to `~/.config/needle/adapters/`, reads the absolute
binary path back out of the installed templates, links it to whichever real
`claude-print` it can find, and verifies each path runs `--version`.

> ⚠️ **`needle test-agent claude-print-opus` is not sufficient proof.** It
> resolves `agent_cli` through PATH, while dispatch runs `invoke_template`. When
> the template names a binary that does not exist, test-agent still reports
> `Status: READY` and every dispatch dies at exit 127 (NEEDLE bead
> `needle-adef2ccd`). The authoritative checks are automated in two places
> (claudego-b03e5c39): the installer and `cgov doctor` (check
> `claude_print_adapters`) both statically verify each template still unsets
> the rule-3 and rule-5 variable sets, and both run the `invoke_template`
> verbatim with a trivial prompt against a poisoned rule-3/rule-5
> environment, requiring exit 0 with output (one trivial subscription call
> per adapter; `--skip-live` on the installer, `cgov doctor --skip-live` on
> the doctor, skip the static-only case). The logic lives in
> `src/adapter_verify.rs`, mirrored in bash inside the installer; the mirror
> is enforced by two gates, not maintained manually. The cargo test
> `installer_bash_variable_lists_match_the_rust_constants` in
> `src/adapter_verify.rs` and installer section 4's `check_var_list_sync` in
> `deploy/install-claude-print-adapters.sh` both compare the rule-3 pair
> (`IDE_ENV_VARS` ↔ `RULE3_IDE_VARS`) and the rule-5 pair
> (`API_ROUTING_ENV_VARS` ↔ `RULE5_API_VARS`) in both directions, catching
> variables missing from either copy or extra in either copy. Add new variables
> to **both** copies; both gates fail until you do.

Three rules make these work under NEEDLE dispatch (all learned the hard way):

1. **Deliver the prompt with `< {prompt_file}`.** Without it, claude-print launches
   with no prompt, produces nothing, exits instantly, and NEEDLE's re-dispatch loop
   churns real beads into stuck `in_progress`. NEEDLE does **not** pipe the prompt on
   stdin for you — the template must redirect it.
2. **Call the binary by absolute path** (`/home/coding/.local/bin/claude-print`).
   NEEDLE's dispatch shell PATH is not the interactive shell's; a bare `claude-print`
   is "command not found" (silent empty output).
3. **Scrub the IDE environment** — `unset CLAUDECODE CLAUDE_CODE_SSE_PORT VSCODE_*`
   before the binary. `CLAUDECODE=1` tells the dispatched agent it is nested inside
   Claude Code; combined with an inherited `VSCODE_IPC_HOOK_CLI` and a live
   `~/.claude/ide/<port>.lock`, it connects to the VS Code extension host over
   loopback **instead of ever reaching the API** and blocks there until
   `timeout_secs` kills it. Observed at 46% of dispatches (26/57) on a worker
   launched from an interactive Claude Code shell, including six consecutive
   20-minute Opus timeouts on one bead. The symptom is a hung dispatch whose only
   socket is `127.0.0.1 -> 127.0.0.1:<ide-lock-port>` with no outbound connection to
   the API. The glm adapter has always carried `unset CLAUDECODE`, which is why it
   was immune. Workers launched by cgov via systemd get a clean environment and never
   see this — the adapter must be immune either way.

4. **Pass `--pretrust-cwd`.** In a workspace that has never been trusted, claude
   2.1.263 renders its trust dialog with the *refusing* option selected by
   default:

   ```
   Quick safety check: Is this a project you created or one you trust?
   ❯ No, exit
     Yes, I trust this folder
   ```

   claude-print confirms whatever is highlighted, so it picks "No, exit" and the
   session dies before doing any work. The failure surfaces misleadingly as
   `"claude exited before Stop hook fired"` (`internal_error`, exit 2), because
   the phase machine records the dismissal as a *successful* `trust-dismissed`
   transition. `--pretrust-cwd` writes `hasTrustDialogAccepted: true` for the cwd
   before launch and removes the dialog entirely.

   This is invisible in any directory already carrying
   `hasTrustDialogAccepted: true` in `~/.claude.json` — which is why it only
   appears when dispatching somewhere new. Upstream fix: claude-print bead
   `claudepr-fe3d3160`; keep the flag regardless, since it also removes the
   one-time dialog stall.

5. **Scrub the API-routing environment** — `unset ANTHROPIC_API_KEY
   ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL ANTHROPIC_MODEL
   ANTHROPIC_SMALL_FAST_MODEL ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL
   CLAUDE_CODE_SUBAGENT_MODEL` before the binary, same shape as
   rule 3 but for the vars that decide *where the API call goes* rather than
   whether it goes anywhere. claude-print scrubs `CLAUDECODE` and forces
   `CLAUDE_CODE_ENTRYPOINT=cli` but deliberately passes every other var through
   (`src/pty.rs` `SCRUBBED_ENV`), so a worker launched from a shell carrying the
   glm proxy's `ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN` hands them to the
   child: the session hangs at init until claude-print's watchdog SIGTERMs it
   (exit 124, `stream_json_first_output_timeout`) — and had it answered, it
   would have billed the proxy pool while wearing the subscription adapter.
   The model-name overrides belong in the same breath: `ANTHROPIC_MODEL` and
   `CLAUDE_CODE_SUBAGENT_MODEL` are both set to `glm-5.3-flash` in every
   proxy-routed worker shell (visible in `ps` for any live dispatch), and an
   inherited `CLAUDE_CODE_SUBAGENT_MODEL` would point the strand's
   subagents at a model the subscription endpoint has never heard of.
   Verified 2026-09-07 (bead `claudego-ab0f6871`): the installed template run
   with the proxy vars present dies at 124 with no assistant event; the same
   run with them unset exits 0 with a real `claude-opus-5` reply. cgov/systemd
   workers get a clean env and never see this; workers launched from an
   interactive Claude Code shell — the 46% population of rule 3 — always do.

Also: `--output-format stream-json` (what `needle-transform-claude` expects) and
`--no-inherit-hooks` (isolation; claude-print still installs its own Stop hook).

`timeout_secs` is a **hard backstop**: claude-print buffers stream-json until the
end, so NEEDLE's idle stuck-detection is blind during a run. A hung strand is only
killed by this wall-clock timeout — keep it tight (opus 1200s, fable 600s).

---

## 3. Running & verifying

```bash
cgov doctor                 # health check (claude_print + subscription checks included)
cgov config                 # confirm configured pools are parsed
cgov restart                # daemon reloads config ONLY on restart (agents load once at start)
cgov workers                # per-agent current/target
journalctl --user -u claude-governor -n 30 | grep reconcile   # watch it launch the strand
tmux ls                     # inspect active worker sessions
```

Expected on a healthy start: the daemon reports a capacity decision, reconciles
each configured pool toward its target, and starts or stops workers as needed.

**Lab note:** the second host is reachable at the Tailscale IP `100.81.129.38`; the
hostname times out. Each host needs its own `claude-print` binary + adapters + creds.

---

## 4. The cgov code fixes behind this (all in `src/`)

These fixes prevent cgov from choking on `null`/`Inf` from the API/state or
treating configured agents as fungible.

- **poller.rs** — `UsageResponse` windows are `Option`; a `null` window (the API
  legitimately returns one, e.g. no separate sonnet limit) no longer crashes the
  whole poll and starves the governor of capacity data.
- **state.rs** — null-tolerant deserialize for `hard_limit_margin_hrs`, `cone_ratio`,
  `risk_score` (an `Inf` serializes to `null`); the daemon no longer discards all
  learned calibration and "starts fresh" every cycle.
- **governor.rs `distribute_workers_by_cost_priority`** — guarantees each agent's
  `min_workers` floor before cost-distributing the remainder, so an expensive pool
  (Opus, max 1) actually wins a slot. Gentle scale-up/down behaviour preserved.
- **governor.rs `NoChange` arm** — reconciles the per-agent allocation even when the
  aggregate total is unchanged, so a pinned pool launches at a steady total instead of
  the daemon only ever acting on aggregate deltas.
- **governor.rs `safe_worker_count_or_max`** (renamed `safe_worker_count_or_hold`; pinned by `tests/governor_scaling_fixes.rs`) — `Some(0) → 0` (was `→ current_total`):
  when the binding window can't afford even one worker, cgov now actually scales to 0
  instead of holding capacity that would drive the shared window to a platform cutoff.
  This is what makes "allow scaling to 0" real for a use-or-lose utilisation governor.
- **governor.rs `apply_underutilization_sprint`** — wires the previously **dead** sprint
  (`check_underutilization_sprint` was defined but never called in the cycle) into the
  daemon: when a window is under-used and resets soon and nothing is at cutoff risk,
  cgov may boost an eligible subscription pool toward its maximum. A governor.yaml
  containing a retired pool or queue fails config validation at load
  (`RETIRED_REFERENCE_MARKERS` in `src/config.rs`), so a sprint target can no longer
  be a retired pool by construction.

---

## 5. Known warts

- Token collector cursor file corruption is auto-recovered (hardened 2026-09-16,
  claudego-dddaf7fb): a corrupt `collector-cursors.json` is quarantined to
  `.corrupt-<timestamp>`, the surviving per-file offsets are rebuilt so those
  files resume from their last good offset, the rebuilt store is persisted
  immediately, and a `cursor recovery` WARN is logged. The pass completes; only
  files whose cursor was lost re-read from byte 0 once (a re-count, not data
  loss). Scaling is unaffected. Cursor saves are atomic (unique temp file +
  rename), so new corruption should be rare.
- `cargo test` offloads to iad-ci when the tree is clean; runs locally (cgroup-limited)
  with uncommitted changes. `cargo build` always runs locally. Host note, updated
  2026-09-19: codinghome's `~/.local/bin/cargo` had been clobbered to a plain
  `~/.cargo/bin` symlink (2026-08-12, untracked), so `cargo test` there ran real cargo
  locally for five weeks; restored as of 2026-09-19 byte-identical to the tracked lab
  copy (verified by claudego-41ca0b41) — the wrapper intercepts on both hosts again.
  Both wrappers are now tracked (NEEDLE `fleet/lab/bin/{cargo,cargo-remote}`,
  needle-322a3953, 2026-09-24) and install via `fleet/lab/apply-lab-fleet.sh
  --install-cargo-wrapper`; `wrapper-drift.timer` re-compares the deployed bytes
  against origin/main every 30 min on both hosts (exit 1 = drift, 2 = blind
  detector). Both wrappers carry both scope hardenings on both hosts —
  `--slice="$(current_slice)"` and `-p RuntimeMaxSec=14400` (needle-3d5c65d8,
  reaps hung scopes after 4h). `cargo-remote` since the 2026-09-25 installer run,
  verified live that day (claudego-adfbc8e9): the dirty-tree fallback from a
  needle.slice caller produced a scope in needle.slice with RuntimeMaxUSec=4h, not
  an unbounded app.slice one. `bin/cargo`'s own `local_limited` fallback (every
  non-test command: `build`, `check`, `metadata`, …) closed the same gap
  2026-09-25 (claudego-4746d945, NEEDLE 65fc66ad; `fleet/lab/test.sh` now pins
  both hardenings on both wrappers) — verified live on both hosts the same way: a
  non-test command from a needle.slice caller produces a scope in needle.slice
  with RuntimeMaxUSec=4h, not an app.slice one. The adapter sync gates are
  unaffected — they run under any `cargo test`, including NEEDLE's close-gate
  re-extraction.
