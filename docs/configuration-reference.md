# Governor Configuration Reference

The complete, contract-checked reference for `governor.yaml` — the file that
configures every `cgov` component (daemon, observe/act halves, token
collector, doctor). The schema it documents lives in `src/config.rs`
(`GovernorConfig` and children); the test that keeps this document honest is
`tests/config_docs_contract_test.rs`.

Two blocks in this file are **machine-checked** against the binary:

- [Complete key inventory](#complete-key-inventory) — the exact set of keys
  the binary accepts. A key added to or removed from `src/config.rs` without
  updating this list fails the contract test, and so does a list entry the
  binary does not actually support.
- [Defaults snapshot](#defaults-snapshot) — the value the binary resolves for
  every key when it is absent from the file. A default change without a doc
  update fails the contract test.

Everything else (behaviour, validation, migration) is prose; where a claim
rests on code, the code is named.

---

## 1. Where configuration lives

`GovernorConfig::load_with_path` (`src/config.rs`) tries, in order:

1. `$XDG_CONFIG_HOME/claude-governor/governor.yaml` — only when
   `XDG_CONFIG_HOME` is set in the environment
2. `~/.config/claude-governor/governor.yaml` — the normal live path
3. `./config/governor.yaml` — the repo working tree, for development

The **first path that exists wins**; the others are never read.

### Live configuration vs the checked-in seed template

`config/governor.yaml` in this repository is only a **seed template**, not
running configuration. It is baked into the binary with `include_str!` at
build time (`create_default_config`) and copied verbatim to the
highest-precedence path **on first run only** — when none of the three paths
above exists. After that the live file is the running configuration and is
never rewritten automatically: upgrading `cgov` does not update it, and
editing the repo template has no effect on any machine that already has a
live file.

`cgov init` creates the directory and copies the template the same way;
`cgov init --force` is the one command that **overwrites an existing live
config with the current template** — it discards every local edit, so treat
it as a reset, not an upgrade path.

To see the configuration a machine is actually running, use `cgov config`:
its first line names the file that was loaded (the live path, never the
template) and the body is the fully resolved configuration. `cgov config
--edit` opens that same file in `$EDITOR` (default `nano`).

### When changes take effect

The daemon reads `governor.yaml` **once at startup**. Editing the file does
nothing to a running daemon — `cgov restart` to apply changes. CLI flag
overrides (below) apply to the process they are passed to, once, at the same
moment the file is read.

---

## 2. Runtime overrides

### CLI flags

Four entry points accept flag overrides at startup (`src/main.rs`): `cgov
daemon`, and the internal `cgov _daemon`, `cgov _act`, `cgov _observe` (the
forms the systemd units call).

| Flag | Overrides | Applies to |
|---|---|---|
| `-i`, `--interval` | `daemon.loop_interval_secs` | `daemon`, `_daemon`, `_act`, `_observe` |
| `--hysteresis` | `daemon.hysteresis_band` | `daemon`, `_daemon`, `_act` |
| `-c`, `--ceiling` | the global `daemon.target_ceiling` fallback | `daemon`, `_daemon`, `_act` |

`--ceiling` replaces only the **global fallback** ceiling. A per-window
override under `daemon.windows` that resolves to a positive value still takes
precedence over both the config's global and the flag (see
`get_target_ceiling_for_window` and its use in `run_act_cycle`). This is the
same precedence the config itself follows; the flag never weakens a
deliberate per-window reserve.

### Environment variables

Environment variables are process-level overrides, never YAML. Nothing in
`governor.yaml` can set them and no config key duplicates them.

| Variable | Effect | Defined in |
|---|---|---|
| `XDG_CONFIG_HOME` | Prepends `<XDG>/claude-governor/governor.yaml` to the config search path | `src/config.rs` `config_paths` |
| `CGOV_LEDGER_LOGS_DIR` | Logs directory for ledger-yield computation | `src/ledger_yield.rs` (`ENV_LOGS_DIR`) |
| `CGOV_LEDGER_WINDOW_HOURS` | Window length (hours) the ledger yield is measured over | `src/ledger_yield.rs` (`ENV_WINDOW_HOURS`) |
| `CGOV_DECISIONS_PATH` | Output path for the decision audit log | `src/narrator.rs` |
| `NO_COLOR` | Any value: disable colour in `cgov status` output | `src/status_display.rs` |
| `EDITOR` | Editor used by `cgov config --edit` (default `nano`) | `src/main.rs` |

---

## 3. Schema

Every key, its type and its default. "Required" means the load fails without
it; everything else fills from its default when absent. Sections themselves
(`sprint`, `daemon`, `alerts`, `composite_risk`, `cone_scaling`, `agents`)
are optional — an absent section is the same as an empty one.

### Top level

| Key | Type | Default | Description |
|---|---|---|---|
| `pricing` | map | — (required) | Model pricing table |
| `pricing.models` | map | — (required) | Model id → per-model rates |
| `credentials_path` | string | `null` | OAuth credentials file, `~` expanded. Default is `~/.claude/.credentials.json` (applied by the consumer, not here); set it for multi-account setups |

### `pricing.models.<model-id>` — all five fields required

Rates in USD per million tokens. No defaults: a model entry missing any
field fails to load. Model ids are whatever appears in collector JSONL
records — aliases and legacy ids are just more entries. `cgov doctor`'s
`pricing_coverage` check fails when recent records reference a model with no
entry here.

| Key | Type |
|---|---|
| `input_per_mtok` | number |
| `output_per_mtok` | number |
| `cache_write_5m_per_mtok` | number |
| `cache_write_1h_per_mtok` | number |
| `cache_read_per_mtok` | number |

### `agents.<pool-name>` — worker pools the governor may scale

| Key | Type | Default | Description |
|---|---|---|---|
| `launch_cmd` | string | — (required) | Command that launches one worker. `{id}` expands to cgov's unique per-worker token (timestamp-index) keeping tmux session names and heartbeat files distinct |
| `session_pattern` | string | — (required) | Glob matching the pool's tmux sessions; the prefix (pattern minus trailing `*`/`-`) identifies sessions to count and kill |
| `heartbeat_dir` | string | — (required) | Directory of heartbeat JSON files, `~` expanded |
| `min_workers` | integer | `0` | Floor the allocator guarantees before cost-distributing the remainder |
| `max_workers` | integer | `8` | Hard ceiling for the pool |
| `subscription` | bool | `false` | `true` = subscription-billed (cli-entrypoint sessions); `false` = sdk-cli credits billing |
| `baseline_burn_rate` | map | `null` | Fallback burn model when the collector is offline or the EMA is not ready |
| `baseline_burn_rate.pct_per_worker_per_hour` | number | `1.5` | Percentage-point burn per worker per hour |
| `baseline_burn_rate.dollars_per_worker_per_hour` | number | `5.0` | Dollar burn per worker per hour |
| `windows` | list | `null` | Which usage windows this pool's consumption draws down: any of `five_hour`, `seven_day`, `weekly_scoped`. See below |

`windows` semantics (claudego-ec6d3ae3): `null`/absent means **all windows**
— the conservative default, so an undeclared pool stays bounded by every
window including the premium one. Declared names are normalized to canonical
order; unknown names are dropped; a list that normalizes to empty (empty
list, or only typos) is treated as absent — i.e. all windows. A config
mistake can therefore never leave a pool unconstrained. See
`docs/notes/human-reserve-policy.md` for why this fail-safe direction was
chosen.

### `sprint` — underutilization and end-of-window sprints

| Key | Type | Default | Description |
|---|---|---|---|
| `underutilization_threshold_pct` | number | `50.0` | Utilization below which the sprint trigger arms |
| `underutilization_hours_remaining` | number | `2.0` | Hours-remaining below which the sprint trigger arms |
| `horizon_minutes` | number | `90.0` | End-of-window sprint only fires when the window resets within this horizon |
| `min_headroom_pct` | number | `15.0` | End-of-window sprint needs remaining headroom above this |
| `max_workers_boost` | integer | `3` | Temporary `max_workers` raise while a sprint runs |
| `max_cone_ratio` | number | `2.0` | Sprint blocked when prediction cone ratio exceeds this (forecast too uncertain) |
| `sprint_end_headroom_pct` | number | `5.0` | Sprint ends when headroom drops below this |
| `pace_blocks` | integer | `4` | Pace-block sprint: the weekly window cut into N equal-quota blocks; a pool underspending its block is authorised to run so the shortfall is measurable. `0` disables the pace-block sprint |

### `daemon` — loop, scaling, logging

| Key | Type | Default | Description |
|---|---|---|---|
| `loop_interval_secs` | integer | `300` | Cycle length in seconds. 300 is the *minimum useful* value, not an enforced floor: the usage API percentages don't move visibly faster, and shorter intervals zero out the deltas the burn-rate EMA learns from |
| `hysteresis_band` | number | `1.0` | Scale-down damping band (workers). Deficits of any size close immediately; see `docs/hysteresis-and-smooth-scaling.md` |
| `max_scale_up_per_cycle` | integer | `1` | Max workers added per cycle |
| `max_scale_down_per_cycle` | integer | `1` | Max workers removed per cycle |
| `progressive_scaling` | bool | `false` | Widen the per-cycle caps with the remaining gap: 3x when gap > 5, 2x when gap > 3, always clamped to the gap |
| `exponential_decay_scaling` | bool | `false` | Close 30% of the remaining gap per cycle (rounded up), bounded by the configured cap and the gap; takes precedence over `progressive_scaling` |
| `min_scale_interval_secs` | integer | `60` | Minimum seconds between scale operations |
| `target_ceiling` | number | `90.0` | Global target utilization ceiling, percent (0–100). The fallback for any window without an override |
| `mode` | `auto` \| `systemd` \| `tmux` | `auto` | Worker-launch mode; `auto` picks systemd when available |
| `pre_scale_minutes` | integer | `30` | Look-ahead for peak/off-peak transitions; pre-scales **down** before losing a bonus, never up. `0` disables |
| `log_max_bytes` | integer | `104857600` | Log size before rotation (100 MB) |
| `log_backup_count` | integer | `3` | Rotated logs kept (`governor.log.1` …) |
| `windows` | map | `{}` | Per-window ceiling overrides, keyed by window name |

`daemon.windows.<window>.target_utilization` — fraction 0.0–1.0 (0.85 =
85%), default `null` (inherit the global `target_ceiling`). Resolution order
for any window: a positive override here → else global `target_ceiling` →
(and a `--ceiling` CLI flag replaces only that global fallback). Safe mode
additionally subtracts a fixed reduction while active. Unknown window names
never match and simply fall back to the global value.

### `alerts` — episode-tracked alerting

Alerts follow an **episode lifecycle** (tracked in governor-state.json under
`open_alert_beads`): one continuous stretch of a true condition creates one
bead, refreshes it at most once per cooldown, and auto-closes it when the
condition clears. Bead volume is bounded by distinct incidents, not elapsed
time.

| Key | Type | Default | Description |
|---|---|---|---|
| `command` | string list | `[bf, create, --json, --type, human, --title]` | Episode-open command. The alert message is appended as the **last** argument; stdout is parsed for the created bead id |
| `close_command` | string list | `[bf, close]` | Invoked as `<close_command> <bead_id> --reason <reason>` when an episode clears |
| `update_command` | string list | `[bf, update]` | Invoked as `<update_command> <bead_id> --notes <notes>` to refresh an open episode (throttled by `cooldown_minutes`) |
| `cooldown_minutes` | integer | `60` | Anti-flap floor: minimum gap after a resolved episode before the same condition may open a new one; also throttles refreshes. **Not** a repeat interval — a condition that stays true never mints a second bead |
| `enabled` | bool | `true` | Master switch for alerting |
| `min_severity` | string | `warning` | One of `info`, `warning`, `critical`; any other value **behaves as `warning`** (`meets_severity_threshold`, `src/alerts.rs`) |
| `low_cache_eff_threshold` | number | `0.30` | Fleet cache-efficiency fraction below which the LowCacheEfficiency alert fires |
| `low_cache_eff_intervals` | integer | `5` | Consecutive intervals below threshold before it fires |
| `auto_bead` | bool | `false` | Execute `command` when an episode opens. `false` = episodes are tracked and logged, no external command runs. Gate for enabling: alert FP rate < 5% over a rolling 100-alert window (`alert_fp_telemetry` in governor-state.json) |

The defaults name the deprecated `bf` CLI. Override all three commands
together with the spelling your fleet actually uses — episodes open beads
that `close_command` must be able to close.

### `composite_risk` — cross-window scaling

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | `false` = strict binding-window-only ceiling; `true` = weighted composite risk across all windows |
| `cost_threshold` | number | `0.0` | `0.0` keeps strict binding-window behaviour; higher allows more workers when non-binding windows have ample capacity |
| `binding_weight` | number | `2.0` | Extra weight on the binding window so it stays the primary constraint |

### `cone_scaling` — prediction-cone aggressiveness

| Key | Type | Default | Description |
|---|---|---|---|
| `narrow_threshold` | number | `1.5` | `cone_ratio` below this → narrow cone → act on the p50 safe worker count; at or above → wide cone → act on the p75 (conservative) |

---

## 4. Complete key inventory

`*` marks the wildcard level of a keyed map: one `<model-id>` under
`pricing.models`, one `<pool-name>` under `agents`, one `<window>` under
`daemon.windows`. Every path below is accepted by the binary, and the binary
accepts nothing else — `tests/config_docs_contract_test.rs` derives the
supported set from a full round-trip of a maximal config through
`GovernorConfig` and asserts it equals this list exactly, in both
directions.

<!-- key-inventory:begin -->
```yaml
agents
agents.*
agents.*.baseline_burn_rate
agents.*.baseline_burn_rate.dollars_per_worker_per_hour
agents.*.baseline_burn_rate.pct_per_worker_per_hour
agents.*.heartbeat_dir
agents.*.launch_cmd
agents.*.max_workers
agents.*.min_workers
agents.*.session_pattern
agents.*.subscription
agents.*.windows
alerts
alerts.auto_bead
alerts.close_command
alerts.cooldown_minutes
alerts.command
alerts.enabled
alerts.low_cache_eff_intervals
alerts.low_cache_eff_threshold
alerts.min_severity
alerts.update_command
composite_risk
composite_risk.binding_weight
composite_risk.cost_threshold
composite_risk.enabled
cone_scaling
cone_scaling.narrow_threshold
credentials_path
daemon
daemon.hysteresis_band
daemon.log_backup_count
daemon.log_max_bytes
daemon.loop_interval_secs
daemon.max_scale_down_per_cycle
daemon.max_scale_up_per_cycle
daemon.exponential_decay_scaling
daemon.min_scale_interval_secs
daemon.mode
daemon.pre_scale_minutes
daemon.progressive_scaling
daemon.target_ceiling
daemon.windows
daemon.windows.*
daemon.windows.*.target_utilization
pricing
pricing.models
pricing.models.*
pricing.models.*.cache_read_per_mtok
pricing.models.*.cache_write_1h_per_mtok
pricing.models.*.cache_write_5m_per_mtok
pricing.models.*.input_per_mtok
pricing.models.*.output_per_mtok
sprint
sprint.horizon_minutes
sprint.max_cone_ratio
sprint.max_workers_boost
sprint.min_headroom_pct
sprint.pace_blocks
sprint.sprint_end_headroom_pct
sprint.underutilization_hours_remaining
sprint.underutilization_threshold_pct
```
<!-- key-inventory:end -->

---

## 5. Defaults snapshot

What the binary resolves when a key is absent — serialized from a minimal
`pricing: {models: {}}` config through the same structs the daemon runs on.
The contract test parses this block and asserts value-equality against the
binary's own serialization. `pricing` and `pricing.models` are the only
required keys; they have no defaults.

<!-- defaults-snapshot:begin -->
```yaml
pricing:
  models: {}
sprint:
  underutilization_threshold_pct: 50.0
  underutilization_hours_remaining: 2.0
  horizon_minutes: 90.0
  min_headroom_pct: 15.0
  max_workers_boost: 3
  max_cone_ratio: 2.0
  sprint_end_headroom_pct: 5.0
  pace_blocks: 4
agents: {}
daemon:
  loop_interval_secs: 300
  hysteresis_band: 1.0
  max_scale_up_per_cycle: 1
  max_scale_down_per_cycle: 1
  progressive_scaling: false
  exponential_decay_scaling: false
  min_scale_interval_secs: 60
  target_ceiling: 90.0
  mode: auto
  pre_scale_minutes: 30
  log_max_bytes: 104857600
  log_backup_count: 3
  windows: {}
alerts:
  command: [bf, create, --json, --type, human, --title]
  close_command: [bf, close]
  update_command: [bf, update]
  cooldown_minutes: 60
  enabled: true
  min_severity: warning
  low_cache_eff_threshold: 0.30
  low_cache_eff_intervals: 5
  auto_bead: false
composite_risk:
  enabled: false
  cost_threshold: 0.0
  binding_weight: 2.0
cone_scaling:
  narrow_threshold: 1.5
credentials_path: null
```
<!-- defaults-snapshot:end -->

---

## 6. Validation rules

### Hard load errors (`GovernorConfig::parse_and_validate`)

The daemon, every config-reading CLI command, and `cgov doctor`'s
`config_parseable` check all fail on:

- invalid YAML, or a value of the wrong type anywhere in the tree;
- missing `pricing` or `pricing.models`;
- a model entry missing any of the five rate fields;
- an agent pool missing `launch_cmd`, `session_pattern`, or `heartbeat_dir`;
- **references to retired components** — see below.

### Retired-component references (rejected, not ignored)

A config naming a component retired on 2026-09-16 (the standalone polish
queue, its timer and seeder, and the subscription generator pool) fails to
load with remediation text. Markers (`RETIRED_REFERENCE_MARKERS`,
`src/config.rs`): `polish`, `generator-pool`, `generator_pool` — matched
**case-insensitively as substrings** against top-level key names, agent pool
names, and each pool's `launch_cmd`, `session_pattern`, and
`heartbeat_dir`. There is no allowlist: a pool whose name merely contains a
marker is rejected too and must be renamed.

This guard exists *because* unknown keys are otherwise silently dropped:
serde never sees a stale `polish_queue:` block, so only an explicit raw-YAML
scan can reject it. `cgov doctor` surfaces the same condition as its
`retired_component_refs` check.

### Silent tolerance (documented, deliberate)

- **Unknown keys are ignored.** A typo (`daemon.poll_interval_secs`,
  `min_worker`) parses fine, takes effect never, and is dropped when the
  config is re-serialized. Nothing warns. The contract test pins the seed
  template and this document against that hazard; a live file is the
  operator's responsibility — `cgov config` shows what actually resolved.
- **`agents.<pool>.windows`** — unknown names dropped, empty-or-all-unknown
  list falls back to all windows (fail-safe toward *more* constraint).
- **`daemon.windows` unknown window names** — never match; global fallback.
  The live window set is `five_hour`, `seven_day`, `weekly_scoped`; there is
  no `seven_day_sonnet` window.
- **`alerts.min_severity` unknown value** — behaves as `warning`.

### `cgov doctor` checks beyond parsing

`config_parseable` (the file parses and passes the retired-reference guard),
`pricing_coverage` (recent collector records reference models with no
pricing entry), `retired_component_refs`, and `retired_component_units`
(stale systemd units). See `cgov doctor --json` for the live verdicts.

---

## 7. Migration behaviour

- **Adding keys to newer cgov:** an older live config keeps working — every
  new key fills from its default (that is what the defaults snapshot
  documents). No config edit is required across an upgrade.
- **Removing or renaming keys:** the old key becomes *unknown* and is
  **silently ignored**, while the replacement (if any) takes its default.
  This is the one silent behaviour-change hazard of a config upgrade; the
  contract test guards the seed template and this document, and `cgov
  config` shows the resolved values after an upgrade.
- **Retired components:** the loud exception — a retired marker fails the
  load outright instead of being ignored (see §6). Prune the entries;
  NEEDLE's native Weave/Explore strands replaced the retired queue.
- **Template changes:** the template is baked in at build time and applied
  on first run only. Upgrading cgov never touches an existing live file;
  reconcile deliberately by editing the live path, using `cgov config` to
  compare, or resetting with `cgov init --force` (discards local edits).
- **Restart to apply:** the daemon loads the file once at startup; config
  edits require `cgov restart`.
