# Bead bf-2ssban: Rust Source Files Inventory

## Task
List all Rust source files (.rs) in the src/ directory tree.

## Results
Found 18 Rust source files in src/:

1. `src/alerts.rs` - Alert management and notification logic
2. `src/burn_rate.rs` - Burn rate calculations and forecasting
3. `src/calibrator.rs` - Calibration and tuning logic for capacity forecasts
4. `src/capacity_summary.rs` - Capacity summary reporting
5. `src/collector.rs` - Token usage collection and aggregation
6. `src/config.rs` - Configuration file parsing and management
7. `src/db.rs` - Database access layer
8. `src/doctor.rs` - Health check and diagnostic commands
9. `src/governor.rs` - Core governor daemon (worker pool orchestration)
10. `src/lib.rs` - Library exports and common types
11. `src/main.rs` - CLI entry point
12. `src/narrator.rs` - Human-readable output formatting
13. `src/poller.rs` - API polling (subscription usage, limits)
14. `src/pricing.rs` - Pricing model and cost calculations
15. `src/schedule.rs` - Scheduling and periodic tasks
16. `src/simulator.rs` - Forecasting simulation
17. `src/snapshot_fixtures.rs` - Test fixtures for snapshots
18. `src/state.rs` - Persistent state management

## Verification
- All files are relative to `src/` root
- No subdirectories exist (flat structure)
- File count: 18

## Method
```bash
find src -name "*.rs" -type f | sort
```
