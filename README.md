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

```bash
cargo build --release
sudo cp target/release/cgov /usr/local/bin/
```

## Configuration

The governor reads configuration from `~/.config/claude-governor/config.yaml`:

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

# Manually set target worker count
cgov scale 3

# Run governor daemon
cgov daemon

# Show recent scaling decisions
cgov explain
```

## Usage Windows

The governor tracks three parallel usage windows:

| Window | Reset | Purpose |
|--------|-------|---------|
| `five_hour` | Rolling 5-hour session | Burst rate limiting |
| `seven_day` | 7-day rolling window | Weekly quota (all models) |
| `seven_day_sonnet` | 7-day rolling window | Weekly Sonnet quota |

## Alerting

The governor creates HUMAN-type beads via NEEDLE when:
- Any window is at cutoff risk (< 5% margin)
- Cache efficiency drops below threshold
- Collector is offline

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

## License

MIT
