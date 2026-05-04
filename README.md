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

### Option 1: Pre-built binary (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/jedarden/claude-governor/main/install.sh | bash
```

### Option 2: Build from source

```bash
cargo build --release
cp target/release/cgov ~/.local/bin/
chmod +x ~/.local/bin/cgov
```

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

# Manually set target worker count
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

# Tail governor logs
cgov logs --follow

# Print or edit configuration
cgov config
cgov config --edit
```

## Daemon Management

```bash
# Initialize (create config, directories, install systemd units)
cgov init

# Enable services (install + start systemd/tmux)
cgov enable

# Start services
cgov start

# Stop services
cgov stop

# Restart services
cgov restart

# Disable services (stop + remove systemd units)
cgov disable
cgov disable --purge
```

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
