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

 polish-seeder (cron/timer) ──► tops the QUEUE up with new "Polish-gen: <repo>"
   meta-beads, but only when a repo's own ready-bead backlog is low (two-tank:
   generation follows consumption, so it converges instead of over-polishing).
```

Two tiers, both billed to the **subscription** (`cc_entrypoint=cli`) via `claude-print`:
generation (this loop) and execution (a normal NEEDLE fleet that works the beads).

---

## 2. Components & where they live

| Component | Location | Notes |
|---|---|---|
| `cgov` binary | `~/.local/bin/cgov` | built from this repo (`cargo build --release`, target redirects to `~/target/release/cgov`) |
| Governor config | `~/.config/claude-governor/governor.yaml` | agents, daemon, pricing; **not** in the repo (machine-specific) |
| `claude-print` binary | `~/.local/bin/claude-print` | PTY wrapper that keeps sessions on the subscription pool; install per host |
| NEEDLE adapters | `~/.config/needle/adapters/claude-print-{opus,fable}.yaml` | copies committed under `deploy/needle-adapters/` |
| Polish queue | `~/cgov-polish-queue/` | dedicated git repo + `.beads`; **only meta-beads live here** |
| Seeder | `scripts/polish-seeder.sh` (repo) → runs anywhere | reads `~/.config/claude-governor/polish-targets.txt` |

> ⚠️ **The live NEEDLE adapters directory is `~/.config/needle/adapters/`, NOT
> `~/.needle/agents/`.** The latter is a stale staging path (`claude-print`'s
> installer writes there) that the current `needle` binary does **not** read.

---

## 3. The claude-print adapters (`deploy/needle-adapters/`)

Install by copying to `~/.config/needle/adapters/`, then `needle test-agent claude-print-opus`.

Two rules make these work under NEEDLE dispatch (both learned the hard way):

1. **Deliver the prompt with `< {prompt_file}`.** Without it, claude-print launches
   with no prompt, produces nothing, exits instantly, and NEEDLE's re-dispatch loop
   churns real beads into stuck `in_progress`. NEEDLE does **not** pipe the prompt on
   stdin for you — the template must redirect it.
2. **Call the binary by absolute path** (`/home/coding/.local/bin/claude-print`).
   NEEDLE's dispatch shell PATH is not the interactive shell's; a bare `claude-print`
   is "command not found" (silent empty output).

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
bf init && git init -q && echo ".beads/*.db" > .gitignore
git add -A && git -c user.email=github@jedarden.com -c user.name=jedarden commit -qm "polish queue"
```

A meta-bead's **description is the generator prompt** (self-contained — the lab has
no skills to lean on). It tells the strand to `cd` into the target repo, audit
plan.md for verifiable polish within scope, create ≤5 beads in the target, and close
itself in the queue. The seeder writes these; see `scripts/polish-seeder.sh`.

---

## 5. The generator pool (add to `governor.yaml`)

See `deploy/polish-opus-agent.yaml`. **This flavor governs subscription-billed
*generator* pools only** — cgov-driven claude-print runners that **produce beads**
(never change code); the beads are worked by a separate, normal NEEDLE fleet.

```yaml
  polish-opus:
    launch_cmd: "needle run --agent claude-print-opus --workspace /home/coding/cgov-polish-queue --identifier cgov-polish"
    session_pattern: "needle-claude-print-opus-cgov-polish-*"
    heartbeat_dir: "~/.needle/state/heartbeats"
    min_workers: 0        # genuinely allowed to idle at 0
    max_workers: 4        # headroom to scale up and burn spare capacity when windows have room
    subscription: true    # billed against the subscription pool, not the SDK credit pool
```

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

Keeps the queue fed. For each target repo it creates a `Polish-gen: <repo>` meta-bead
**only if** (a) no such meta-bead is already pending in the queue, and (b) the repo's
own ready-bead backlog is below `LOW_WATER` — so generation follows consumption and
converges instead of endlessly re-polishing.

```bash
# 1. list target repos (one absolute path per line)
cp scripts/polish-targets.example.txt ~/.config/claude-governor/polish-targets.txt
$EDITOR ~/.config/claude-governor/polish-targets.txt

# 2. one pass (safe, idempotent)
scripts/polish-seeder.sh

# 3. on a cadence — cron every 30 min:
#    */30 * * * * /home/coding/claude-governor/scripts/polish-seeder.sh >> ~/.local/share/claude-governor/seeder.log 2>&1
#    or:  scripts/polish-seeder.sh --loop 1800
```

Env overrides: `CGOV_POLISH_QUEUE`, `CGOV_POLISH_LOW_WATER`, `CGOV_POLISH_TARGETS`.

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

---

## 9. Known warts

- Token collector cursor file can corrupt (`collector pass failed: Failed to load
  cursors`) — a non-fatal WARN; scaling is unaffected.
- `cargo test` offloads to iad-ci when the tree is clean; runs locally (cgroup-limited)
  with uncommitted changes. `cargo build` always runs locally.
