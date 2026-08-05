# bf-18y8i — Fix minor issues in plan.md

## Findings

Both issues named in the bead were already resolved before this bead ran, by
commit `5e6fdcc` ("Update plan.md to align with implemented artifacts"):

1. **Duplicate risk-item numbering.** The duplicate `9.` was introduced in
   `0bbc0f5` and removed in `5e6fdcc`. `docs/plan/plan.md:2054` onward now
   numbers Risk Considerations sequentially `1.`–`10.` with no repeats.
2. **Doctor health check count.** The `12 health checks` wording was introduced
   in `26f1292` and replaced in `5e6fdcc`. `docs/plan/plan.md:1234` now reads
   `~20 health checks`.

Verified `~20` is accurate against the implementation: `src/doctor.rs` defines
20 `check_*` functions (`daemon_running`, `collector_running`,
`state_file_freshness`, `oauth_token_validity`, `heartbeat_consistency`,
`sqlite_integrity`, `config_parseable`, `tmux_available`, `burn_rate_samples`,
`alert_cooldown`, `prediction_accuracy`, `disk_space`, `api_reachability`,
`model_generation`, `promotion_dates`, `jsonl_db_sync`, `log_file`,
`claude_binary_installed`, `claude_print_installed`,
`subscription_session_presence`).

## Change made

Verifying the count turned up a real remaining defect in the same section: the
sample `cgov doctor` output at `docs/plan/plan.md:1204`–`1229` lists 20 `✓`
lines and 1 `⚠` line, but its tally line claimed `19 passed · 2 warnings · 0
failed`. Corrected to `20 passed · 1 warning · 0 failed`, which also matches the
format string the implementation actually prints
(`src/doctor.rs:1687` — `"{} passed · {} warning · {} failed\n"`, singular
"warning").

## Not changed

The checks table at `docs/plan/plan.md:1177`–`1200` has 21 rows because it lists
`Pricing config` separately, while the implementation folds that validation into
`check_config_parseable` (`src/doctor.rs:731`). That is a table/implementation
mismatch, not a numbering or count error, and is outside this bead's scope.
