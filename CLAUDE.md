# Claude Governor — Operating Guide

Claude Governor (`cgov`) is a capacity governor: it polls Claude subscription usage,
forecasts whether each window will exhaust before it resets, and scales a NEEDLE
worker fleet to fit. This file documents how to **run the fleet**, including the
**polish loop** — a self-refilling pipeline that keeps "finished" repos improving
by generating and working small, verifiable polish beads on the subscription pool.

---

## 1. Architecture of the polish loop

```
cgov daemon ──(reconcile per-agent min)──► launches the polish-opus strand (max 1)
      │                                           │
      │ governs capacity (window budget)          │ NEEDLE worker, claude-print (Opus, subscription billing)
      ▼                                           ▼
 keeps ≤ safe_worker_count total          claims a meta-bead from the polish QUEUE
                                                  │
                    ┌─────────────────────────────┘
                    ▼
   reads TARGET repo's docs/plan/plan.md, audits for real, verifiable polish
   within existing scope, creates ≤5 polish beads IN THE TARGET repo, then
   closes the meta-bead. Idles (no cost) when the queue is empty.

 polish-seeder (cron/timer) ──► discovers jedarden's current active public
   GitHub repos and tops the QUEUE up with new "Polish-gen: <repo>" meta-beads,
   but only when a repo's own ready-bead backlog is low and its last completed
   pass is outside the cooldown (two-tank: generation follows consumption).
```

Two tiers, both billed to the **subscription** (`cc_entrypoint=cli`) via `claude-print`:
generation (this loop) and execution (a normal NEEDLE fleet that works the beads).

---

## 2. Components & where they live

| Component | Location | Notes |
|---|---|---|
| `cgov` binary | `~/.local/bin/cgov` | built from this repo (`cargo build --release`, target redirects to `~/target/release/cgov`) |
| Governor config | `~/.config/claude-governor/governor.yaml` | agents, daemon, pricing; **not** in the repo (machine-specific) |
| `claude-print` binary | `~/.local/bin/claude-print` | PTY wrapper that keeps sessions on the subscription pool. **Establish this path with `deploy/install-claude-print-adapters.sh`** — the adapters call it by absolute path and nothing else creates it (claudego-49195ba4) |
| NEEDLE adapters | `~/.config/needle/adapters/claude-print-{opus,fable}.yaml` | copies committed under `deploy/needle-adapters/` |
| Polish queue | `~/cgov-polish-queue/` | dedicated git repo + `.beads`; **only meta-beads live here** |
| Seeder | `scripts/polish-seeder.sh` (repo) → runs anywhere | deployed mode discovers GitHub owner `jedarden`; static target file remains available as fallback |

> ⚠️ **The live NEEDLE adapters directory is `~/.config/needle/adapters/`, NOT
> `~/.needle/agents/`.** The latter is a stale staging path (`claude-print`'s
> installer writes there) that the current `needle` binary does **not** read.

---

## 3. The claude-print adapters (`deploy/needle-adapters/`)

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
> `needle-adef2ccd`). The installer's own check is the authoritative one; to
> confirm by hand, run the `invoke_template` verbatim and require exit 0.

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
   appears when dispatching somewhere new. `~/cgov-polish-queue` will be exactly
   that on first creation. Upstream fix: claude-print bead `claudepr-fe3d3160`;
   keep the flag regardless, since it also removes the one-time dialog stall.

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

**Use Opus for generation.** Fable looped/hung on control-flow tasks in testing;
reserve it for genuinely mechanical sweeps and watch it.

---

## 4. The polish queue & meta-beads

The queue (`~/cgov-polish-queue`) is a git repo with its own `.beads` that contains
**only** generation meta-beads. This is a load-bearing safety property: a worker
pointed here can never churn a real repo's beads — worst case it finds nothing and
idles. Create it once:

```bash
mkdir -p ~/cgov-polish-queue && cd ~/cgov-polish-queue
git init -q && git branch -m main
printf '.beads/*.db\n.beads/*.db-*\n.needle-predispatch-sha\n' > .gitignore
printf 'bead_cli:\n  backend: bead-rs\n' > .needle.yaml
bead init --prefix polishq --skip-foreign-workspace
git add .gitignore .needle.yaml .beads/config.json .beads/.gitignore .beads/checkpoint
git -c user.email=github@jedarden.com -c user.name=jedarden commit -qm "polish queue"
```

Three things this must get right, all of which bit on first creation
(2026-09-07):

- **`bead init`, not `bf init`.** bead-rs is the canonical CLI as of 2026-08-14.
  Running `bf` against a bead-rs store (or the reverse) does not fail cleanly —
  it reports a generic SQLite column error, and applying the other tool's
  recovery recipe silently reinitializes the store with the wrong schema.
- **`--skip-foreign-workspace` is required here.** `/home/coding/.beads` exists
  on codinghome but is a *log directory* (doctor logs), not a workspace.
  Workspace discovery walks up, stops at the first `.beads` it finds, and
  refuses to continue — so a plain `bead init` in a fresh directory under
  `/home/coding` fails with "No workspace found". The flag lets discovery past
  it; init still creates the workspace in the current directory.
- **Write `.needle.yaml` with `backend: bead-rs`** so NEEDLE workers dispatched
  here use the right CLI rather than guessing.

A meta-bead's **description is the generator prompt** (self-contained — the lab has
no skills to lean on). It tells the strand to `cd` into the target repo, read its
applicable AGENTS/README and authoritative plan when present, audit for verifiable
polish within documented scope, create ≤5 beads in the target, and close itself in
the queue. The seeder writes these; see `scripts/polish-seeder.sh`.

---

## 5. The generator pool (add to `governor.yaml`)

See `deploy/polish-opus-agent.yaml`. **This flavor governs subscription-billed
*generator* pools only** — cgov-driven claude-print runners that **produce beads**
(never change code); the beads are worked by a separate, normal NEEDLE fleet.

```yaml
  polish-opus:
    launch_cmd: "needle run --agent claude-print-opus --workspace /home/coding/cgov-polish-queue"
    session_pattern: "needle-claude-print-opus-*"
    heartbeat_dir: "~/.needle/state/heartbeats"
    min_workers: 0        # genuinely allowed to idle at 0
    max_workers: 4        # headroom to scale up and burn spare capacity when windows have room
    subscription: true    # billed against the subscription pool, not the SDK credit pool
```

> ⚠️ **No fixed `--identifier`**: cgov runs the launch command once per scale-up step,
> so a fixed identifier collides (`worker X already running`) and caps the pool at 1.
> Omit it — needle NATO-names each worker and the `*` glob tracks them all.

cgov flexes the runner count **`0 ↔ N`** to track subscription window utilisation:
`safe_worker_count` (from the binding window) drives it up when there's headroom and
**down to 0** when a window is tight — filling spare use-or-lose capacity with
productive bead-generation, never driving the subscription to a platform cutoff.
`Some(0) → 0` (see §8) makes the scale-to-0 real; the emergency brake (window ≥ 98%)
forces 0 regardless.

> ⚠️ **Do NOT also configure a non-subscription pool** (e.g. glm via a proxy) in this
> instance. It doesn't consume the subscription — so filling it does nothing for the
> goal — and, being cheaper, the cost-priority distribution hands it every slot,
> starving the generator. Disable such pools here (`max_workers: 0`) or govern them
> from a separate cgov instance.

---

## 6. The seeder (`scripts/polish-seeder.sh`)

Keeps the queue fed. The deployed service queries GitHub on every pass for current
public repositories owned by `jedarden`, excludes archived repositories and forks,
maps names to existing clones under `/home/coding`, and requires each clone to be a
bead-rs workspace. GitHub is used only for selection; Forgejo remains the configured
`origin` and the only push destination.

For each eligible repo it creates a `Polish-gen: <repo>` meta-bead **only if** (a) no
such meta-bead is already pending, (b) its last completed generation pass is outside
the seven-day cooldown, and (c) its ready-bead backlog is below `LOW_WATER`. A pass
creates at most eight meta-beads, so enabling a large public inventory cannot flood
the queue at once. API failure is fail-closed and never falls back to a stale list.

```bash
# 1. inspect current GitHub-derived targets without creating beads
CGOV_POLISH_GITHUB_OWNER=jedarden CGOV_POLISH_REPO_ROOT=/home/coding \
  scripts/polish-seeder.sh --list-targets

# 2. one pass (safe, idempotent)
CGOV_POLISH_GITHUB_OWNER=jedarden CGOV_POLISH_REPO_ROOT=/home/coding \
  scripts/polish-seeder.sh

# 3. on a cadence — systemd user timer (NixOS has no crontab):
cp deploy/claude-polish-seeder.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now claude-polish-seeder.timer   # every 30 min
#    or, if you prefer a foreground loop:  scripts/polish-seeder.sh --loop 1800
```

Env overrides: `CGOV_POLISH_QUEUE`, `CGOV_POLISH_LOW_WATER`, `CGOV_POLISH_TARGETS`,
`CGOV_POLISH_GITHUB_OWNER`, `CGOV_POLISH_REPO_ROOT`, `CGOV_POLISH_COOLDOWN_HOURS`,
`CGOV_POLISH_MAX_SEED_PER_PASS`, and `CGOV_POLISH_PUSH` (1 = also `git push` bead
commits, best-effort; default 0 = local commit only). If no GitHub owner is set, the
static target file remains the fallback. When an owner is set, the static file is
ignored deliberately so a newly-private repo cannot remain targeted through stale data.

**Bead git durability:** `bead create` writes to the live SQLite store
(`.beads/beads.db`, gitignored); `bead sync flush-only` updates the committed
`.beads/checkpoint/`. So each seeder pass **flushes and commits** every eligible
target repo's (and the queue's) beads — scoped to the `.beads/` pathspec so a dirty
tree is untouched — so runner-produced beads survive a fresh clone / db rebuild and
are visible to other hosts.

**Runners can view deployed artifacts via ADB:** the generator prompt reminds runners
that for a repo with a deployed web frontend they have ADB access to a Pixel 6 over
Tailscale (`adb-check`, then open the URL in Chrome and `screencap`) to audit the real
deployed UI/UX, not just the source.

---

## 7. Running & verifying

```bash
cgov doctor                 # health check (claude_print + subscription checks included)
cgov config                 # confirm polish-opus is parsed
cgov restart                # daemon reloads config ONLY on restart (agents load once at start)
cgov workers                # per-agent current/target
journalctl --user -u claude-governor -n 30 | grep reconcile   # watch it launch the strand
tmux ls | grep polish       # the strand's session
```

Expected on a healthy start: `reconcile: needle-sonnet 1 -> 0` → `launched worker` →
`reconcile: polish-opus 0 -> 1`, then the strand claims a queue meta-bead and runs.

**Lab note:** the second host is reachable at the Tailscale IP `100.81.129.38`; the
hostname times out. Each host needs its own `claude-print` binary + adapters + creds.

---

## 8. The cgov code fixes behind this (all in `src/`)

cgov could not launch a dedicated pool until these landed — all one class: cgov
choking on `null`/`Inf` from the API/state, or treating agents as fungible.

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
- **governor.rs `safe_worker_count_or_max`** — `Some(0) → 0` (was `→ current_total`):
  when the binding window can't afford even one worker, cgov now actually scales to 0
  instead of holding capacity that would drive the shared window to a platform cutoff.
  This is what makes "allow scaling to 0" real for a use-or-lose utilisation governor.
- **governor.rs `apply_underutilization_sprint`** — wires the previously **dead** sprint
  (`check_underutilization_sprint` was defined but never called in the cycle) into the
  daemon: when a window is under-used and resets soon and nothing is at cutoff risk,
  cgov boosts the subscription generator toward its max to burn the spare use-or-lose
  capacity before it resets — **gated on real backlog** (`bf ready` depth in the pool's
  workspace > running workers) so the boost is productive, not idle runners.

---

## 9. Known warts

- Token collector cursor file can corrupt (`collector pass failed: Failed to load
  cursors`) — a non-fatal WARN; scaling is unaffected.
- `cargo test` offloads to iad-ci when the tree is clean; runs locally (cgroup-limited)
  with uncommitted changes. `cargo build` always runs locally.
