# Claude Governor

Automated capacity governor for Claude Code subscription usage.

## Overview

Claude Governor monitors Claude Code subscription usage in real time and predicts whether running worker processes will be stopped by hitting a usage window limit before that window resets. When the forecast shows workers will exhaust a window early, the governor scales down the fleet to a safe level; when capacity remains, it allows or adds workers.

This system replaces the fragile `capacity-governor.sh` (TUI screen-scraping, stateless, incomplete off-peak logic) with a reliable, accurate, and extensible Rust daemon.

## Key Features

- **Direct API polling** — Uses `/api/oauth/usage` endpoint instead of screen-scraping
- **Exhaustion prediction** — Forecasts whether each usage window will hit 100% before reset
- **Off-peak awareness** — Accounts for 2x promotion windows when forecasting capacity
- **Adaptive burn rate** — Learns actual per-worker consumption empirically (p75 EMA)
- **Graceful scaling** — Never kills workers mid-task; only scales down idle workers
- **Multi-agent support** — Supports Sonnet, Opus, and pay-per-token providers
- **Zero runtime dependencies** — Single statically-linked binary

## Installation

**Provenance.** The source of truth for this repo is Forgejo
(`git.ardenone.com/jedarden/claude-governor`); GitHub is a read-only mirror and
can lag it. Release binaries are built by the `cgov-ci` Argo Workflow in the
`iad-ci` cluster from a fresh Forgejo clone — the `v<VERSION>` tag is pushed to
Forgejo before the release is cut — and published to GitHub Releases with one
`sha256` sidecar per binary. Forgejo itself is private, so `install.sh` fetches
the artifact from the public GitHub release and **verifies the sidecar digest
before anything is installed**; a given `vX.Y.Z` asset and its digest are
immutable once published, so mirror lag can never swap contents under a pinned
version. Nothing is written to the install dir until verification passes.

### Option 1: Pre-built binary (recommended)

```bash
curl -fsSL https://git.ardenone.com/jedarden/claude-governor/raw/branch/main/install.sh | bash
```

(Forgejo requires authentication; on a box with Forgejo credentials in the git
credential store, the above just works. The GitHub-mirror equivalent is
`https://raw.githubusercontent.com/jedarden/claude-governor/main/install.sh` —
same script, possibly lagging `main`; the *binary* digest check below is what
guards integrity either way.)

The installer downloads the binary plus its published `.sha256` sidecar and
refuses to install on any mismatch.

### Option 1a: Pinned version

Reproducible installs should pin the release tag:

```bash
curl -fsSL https://git.ardenone.com/jedarden/claude-governor/raw/branch/main/install.sh \
  | CGOV_VERSION=v0.1.1 bash
```

`CGOV_VERSION` (or `--version v0.1.1`) selects the release; a bare `0.1.1` is
normalized to `v0.1.1`.

### Option 1b: Digest-pinned (strongest)

Pin the exact binary digest so even a compromised release page cannot serve
you different bytes:

```bash
# Obtain the published digest for your platform and release, e.g.:
curl -fsSL "https://github.com/jedarden/claude-governor/releases/download/v0.1.1/cgov-linux-amd64.sha256"

curl -fsSL https://git.ardenone.com/jedarden/claude-governor/raw/branch/main/install.sh \
  | CGOV_VERSION=v0.1.1 CGOV_SHA256=<64-hex-digest> bash
```

A non-matching `CGOV_SHA256` aborts the install before anything is written.

### Option 1c: Manual, no pipe-to-bash

```bash
cd "$(mktemp -d)"
curl -fsSLO "https://github.com/jedarden/claude-governor/releases/latest/download/cgov-linux-amd64"
curl -fsSLO "https://github.com/jedarden/claude-governor/releases/latest/download/cgov-linux-amd64.sha256"
sha256sum -c cgov-linux-amd64.sha256        # must print: cgov-linux-amd64: OK
install -m 0755 cgov-linux-amd64 ~/.local/bin/cgov
```

### Option 2: Build from source (Forgejo)

Requires a current stable Rust toolchain (`rustup` is the easiest source); the
clone needs Forgejo credentials. To build a known-good release instead of the
tip of `main`, check out the tag first (`git checkout v0.1.1`).

```bash
git clone https://git.ardenone.com/jedarden/claude-governor.git
cd claude-governor
cargo build --release
install -m 0755 target/release/cgov ~/.local/bin/cgov
```

The install path is not a convention — **all three shipped systemd units exec
`%h/.local/bin/cgov`** (`claude-governor-observe.service`,
`claude-governor-act.service`, `claude-token-collector.service`). A binary left
in `target/release/` or installed elsewhere (e.g. `~/.cargo/bin/cgov` via
`cargo install --path .`) works as a CLI but starts nothing when the services
run. Keep `~/.local/bin` on your `PATH`.

Then initialize, configure, and start:

```bash
# 1. Initialize — seeds ~/.config/claude-governor/governor.yaml (shipped
#    default), creates the log/state directories, installs the three systemd
#    user units, and runs daemon-reload. --no-systemd skips the units
#    (tmux mode); --force overwrites existing files.
cgov init

# 2. Configure — the seeded config ships pricing but no agents; add one
#    `agents:` entry per subscription pool (see Configuration below).
cgov config --edit

# 3. Worker-dispatching installs only — install the claude-print NEEDLE
#    adapters. Only the source tree carries deploy/; install.sh does not.
#    The adapters invoke claude-print by absolute path and this script is
#    what establishes and verifies that path. --skip-live runs the static
#    env-scrub checks only (offline, or conserving subscription quota).
./deploy/install-claude-print-adapters.sh

# 4. Enable and start observe, act, and the token collector — via systemd
#    user services, or tmux sessions where systemd user sessions are
#    unavailable (systemd needs `loginctl enable-linger $USER` to keep the
#    services alive after logout). Also (re)installs the units if step 1
#    skipped them.
cgov enable
```

Verify the build the same way as a release install:

```bash
cgov version            # version, build info, component status
cgov doctor             # full health check; --skip-live for offline/quota-conserving
cgov status             # capacity view (--watch to keep it open)
cgov logs --follow      # or: journalctl --user -u claude-governor-observe -f
```

`cgov doctor`'s `claude_print_adapters` check runs the same verification as
step 3 (static env-scrub check plus a live invocation of each template), so a
dispatch-path failure the installer would catch surfaces there too.

### Installer smoke test

The installer itself is exercised end-to-end (latest via pipe, pinned version,
and a must-fail bad-digest run, all in a clean `env -i` environment) by the
`cgov-install-smoke` WorkflowTemplate in
[`declarative-config/k8s/iad-ci/argo-workflows/`](https://git.ardenone.com/jedarden/declarative-config)
(Argo Workflows in `iad-ci` — CI never runs on GitHub Actions).

## Quickstart

```bash
# Initialize configuration and directories
cgov init

# Edit configuration (set agents, pricing, etc.)
cgov config --edit

# Run health check
cgov doctor

# Enable and start daemon services (systemd or tmux)
cgov enable
```

## Directory Structure

The governor's state file lives under the config directory, **not** under
`~/.needle/state/` — that path holds only the collector and heartbeat state.
`cgov doctor`'s `state_file_location` check reports the live path and its age.

```
~/.config/claude-governor/
├── governor.yaml             # Main configuration file
├── governor-state.json       # Governor state (usage, forecasts, burn rates — written by the daemon)
└── governor-state.prev.json  # Previous-cycle snapshot (delta calculation)
~/.local/share/claude-governor/
├── governor.log              # Governor daemon logs
└── collector.log             # Token collector logs
~/.needle/state/
├── heartbeats/               # Worker heartbeat files (managed by NEEDLE)
├── governor-decisions.jsonl  # Scaling-decision audit log (read by `cgov explain`)
├── token-history.jsonl       # Append-only token delta records (collector)
├── token-history.db          # SQLite mirror of token-history.jsonl
└── collector-cursors.json    # Collector read-position bookkeeping
```

## Configuration

The governor reads configuration from `~/.config/claude-governor/governor.yaml`:

```yaml
agents:
  sonnet:
    launch_cmd: needle run --agent=claude-anthropic-sonnet --workspace={workspace} --force
    session_pattern: needle-claude-anthropic-sonnet-*
    heartbeat_dir: ~/.needle/state/heartbeats
    workspace: /path/to/project

polling:
  interval_seconds: 300
  usage_api_url: https://api.anthropic.com/api/oauth/usage

pricing:
  claude-sonnet-4-6:
    input_per_mtok: 3.0
    output_per_mtok: 15.0
    cache_write_5m_per_mtok: 3.75
    cache_write_1h_per_mtok: 6.0
    cache_read_per_mtok: 0.3
```

## Usage

```bash
# Poll usage data from API
cgov poll

# Show window capacity forecasts
cgov forecast

# Show worker count and targets
cgov workers

# Manually pin the fleet's worker target (see "Manual scale override" below)
cgov scale 3

# Show capacity status (with --watch for live updates)
cgov status
cgov status --watch

# Run health diagnostic checks
cgov doctor

# Simulate future capacity trajectory
cgov simulate --workers 4 --hours 24

# View recent scaling decisions
cgov explain
cgov explain --last 20          # more history
cgov explain --json             # machine-readable

# Decisions are appended by every act cycle (scale up/down, hysteresis holds,
# emergency brake) to ~/.needle/state/governor-decisions.jsonl — each entry
# carries the binding window, worker transition, trigger, and a computed-vs-
# actual context block. Set CGOV_DECISIONS_PATH to relocate the log.

# Tail governor logs
cgov logs --follow

# Print or edit configuration
cgov config
cgov config --edit

# Print version, build info, and component status
cgov version

# Run one token collection pass (or start daemon)
cgov collect
cgov collect --daemon

# Query token history from SQLite mirror
cgov token-history --last 5
cgov token-history --compare
cgov token-history --fleet

# Run the governor daemon (main capacity management loop)
cgov daemon
```

## Daemon Management

```bash
# Initialize (create config, directories, install systemd units)
cgov init

# Enable services (install + start systemd/tmux)
cgov enable

# Start services
cgov start
cgov start observe
cgov start act

# Stop all services, or pause only automated scaling and alerting
cgov stop
cgov stop act

# Restart services
cgov restart

# Disable services (stop + remove systemd units)
cgov disable
cgov disable --purge
```

The governor runs as two independently supervised loops. `_observe` polls usage,
updates burn rates and forecasts, and keeps state fresh without launching or
killing workers. `_act` reads that state and performs scaling and alerting. Stop
`act` when actions need to be paused while telemetry should continue; `doctor`
reports this as a warning rather than treating the intentional pause as a
telemetry failure.

## Manual scale override

`cgov scale N` pins the fleet's aggregate worker target by hand. This section is
the normative contract for that override.

- **Persistence.** The override lives in governor state —
  `GovernorState.manual_override` (the `ManualOverride` struct in `src/state.rs`,
  carrying `target`, `set_at`, `expires_at`, `source`) — persisted to
  `~/.config/claude-governor/governor-state.json`, so it survives a daemon
  restart. It is act-owned in the ADR-001 merge split (`docs/plan/plan.md`):
  `merge_act_owned` copies it and `merge_observe_owned` never touches it.
  `merge_act_owned` copies the field only when the act cycle itself changed it
  (the expiry drop — the same three-way rule `safe_mode` uses), so no loop-side
  save can revert a CLI write, including one that lands while an act cycle is
  in flight.
- **Expiry / clear.** TTL, not hold-forever: an override binds for a default of
  2 hours (`MANUAL_OVERRIDE_DEFAULT_TTL_HOURS` in `src/governor.rs`), and
  `cgov scale --ttl 0` extends it until an explicit `cgov scale --clear`. The
  choice is deliberate — long enough to serve as a real pin, short enough that a
  forgotten override cannot hold the fleet at a stale size indefinitely.
  `resolve_manual_override` (`src/governor.rs`) drops the field once
  `expires_at` passes, and computed targets resume. The `--ttl` and `--clear`
  flags are implemented in the CLI (`run_scale_command` in `src/main.rs`, which
  also supports `--dry-run`).
- **Clamping.** The requested count is stored raw (`ManualOverride.target` is
  pre-clamp) and clamped to the fleet's aggregate `[min, max]` on every
  reconcile — `aggregate_worker_bounds` inside `resolve_manual_override`. The
  aggregate bound is the envelope (min of mins, max of maxes) that
  `compute_target_workers` has always clamped the fleet total against — not the
  sum of per-agent bounds — so set-time validation (`validate_scale_count`,
  which rejects a count outside the same envelope up front) and reconcile-time
  clamping name and use the identical range, and a count that binds at all
  binds at the same bound the computed target does. Because the raw count is
  stored, raising an agent's `max_workers` later un-clamps a stored pin without
  re-running `cgov scale`. Per-agent bounds keep working underneath the total:
  allocation within the manual total respects each agent's own floor
  (`distribute_workers_by_cost_priority`) and max.
- **Precedence.** While applied, the manual total replaces the computed target
  as the act cycle's aggregate goal. It deliberately does not enter
  `compute_target_workers`; the cycle applies it after that function returns,
  so per-agent distribution (cost priority, floors, caps) is unchanged within
  the manual total. An active pin suspends `apply_underutilization_sprint` and
  the pre-scale adjustment for that cycle; neither may move an explicit
  operator target.
- **Emergency brake wins; hysteresis never blocks.** Any usage window at or
  above 98% utilization (`EMERGENCY_BRAKE_THRESHOLD`, found by
  `first_brake_window`) suspends the override: the fleet brakes to 0, the
  override stays stored, and it resumes when the brake clears
  (`ManualOverrideResolution::SuspendedByBrake`). Conversely, the scale-down
  hysteresis band (`daemon.hysteresis_band` in `apply_scaling`) never suppresses
  an explicit manual change — a deliberate pin takes effect immediately.
  Manual changes still respect the configured per-cycle movement cap; only the
  forecast hysteresis is bypassed. A manually requested zero is a normal
  scale-down, while zero caused by the emergency brake is recorded as a brake.
- **Decision source.** Every recorded act-cycle capacity decision records
  whether it acted on the manual override or on a computed target, stamped in
  the entry's context (`narrate_decision` / the `context` block built in
  `run_act_cycle`) in the decisions audit log
  `~/.needle/state/governor-decisions.jsonl` (read by `cgov explain`). The
  context uses `decision_source` values `manual_override`, `computed_target`,
  or `emergency_brake`, and keeps both `computed_target` and
  `effective_target` for comparison. Computed at-target holds remain omitted
  to keep the log useful; an active manual pin is recorded even when it holds
  the fleet at its current total.

### Status today

`cgov scale N` stores the persistent override described above. The act cycle
resolves it after computing the forecast target, applies the aggregate clamp,
skips sprint/pre-scale adjustments while it is active, bypasses scale-down
hysteresis, and records the decision source. Safe mode alone does not suspend a
pin; an engaged emergency brake does, and the stored pin resumes after the
brake clears.

## Usage Windows

The governor tracks three parallel usage windows:

| Window | Reset | Purpose |
|--------|-------|---------|
| `five_hour` | Rolling 5-hour session | Burst rate limiting |
| `seven_day` | 7-day rolling window | Weekly quota (all models) |
| `seven_day_sonnet` | 7-day rolling window | Weekly Sonnet quota |

## Alerting

The governor creates HUMAN-type beads via NEEDLE when specific conditions are detected.

See `docs/research/alerts.md` for complete alert documentation including:
- All alert types (cutoff_imminent, sonnet_cutoff_risk, session_cutoff_risk, collector_offline, etc.)
- Severity levels and thresholds
- Cooldown deduplication
- Troubleshooting steps

## Bead visibility

NEEDLE Pluck uses the resolved workspace's `.beads` store, the `bead-rs`
`--ready` frontier, and exact `exclude_labels` matching. For worker launches,
use an absolute `--workspace` path; an empty custom label list does not disable
the built-in exclusions, and wildcard-looking labels are literal strings.

Use [`docs/bead-visibility-quickref.md`](docs/bead-visibility-quickref.md) for
the commands and common mistakes, or
[`docs/bead-visibility-troubleshooting.md`](docs/bead-visibility-troubleshooting.md)
for the full starvation response procedure. The authoritative current filter
inventory is [`docs/plan/pluck-configuration.md`](docs/plan/pluck-configuration.md).

## Project Structure

```
src/
├── alerts.rs       # Alert conditions and bead creation
├── burn_rate.rs    # Exhaustion forecasting and safe worker calculation
├── collector.rs    # Token usage collection from Claude Code logs
├── governor.rs     # Main governor loop and scaling logic
├── poller.rs       # Usage API polling
├── worker.rs       # Worker discovery and scaling
└── ...
```

## Documentation

- `docs/plan/plan.md` — Complete system design plan
- `docs/research/` — Research on API pricing, usage tracking, off-peak promotions
- `docs/bead-visibility-troubleshooting.md` — Comprehensive troubleshooting guide for bead visibility issues, common pitfalls, and configuration best practices
- `docs/bead-visibility-quickref.md` — Quick reference for bead visibility configuration and common pitfalls
- `docs/filter-patterns-reference.md` — Historical Pluck filter patterns and query examples
- `docs/plan/pluck-configuration.md` — Authoritative current Pluck filter and label settings
- `docs/pluck-workspace-paths.md` — Workspace path configuration and discovery
- `docs/pluck-query-results.md` — Historical query patterns and filter syntax examples
- `docs/research/bead-visibility-configuration.md` — Historical six-layer configuration map for bead visibility

## License

Apache-2.0 — see [LICENSE](LICENSE).

---

Part of [jedarden.com](https://jedarden.com)

*This GitHub repo is a read-only mirror of git.ardenone.com/jedarden/claude-governor — issues and PRs are welcome here either way.*
