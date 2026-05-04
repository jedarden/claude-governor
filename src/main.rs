//! Claude Governor - Automated capacity governor for Claude Code subscription usage
//!
//! Usage Poller: Phase 1 foundation
//! - Reads OAuth credentials from ~/.claude/.credentials.json
//! - Refreshes tokens when near expiry
//! - Polls usage data from the Anthropic API
//! - Outputs usage data in human or machine-readable formats
//!
//! CLI Subcommands:
//! - poll: Poll usage data from API
//! - forecast: Show window capacity forecasts
//! - workers: Show worker count and targets
//! - scale: Manually set target worker count
//! - logs: Tail governor.log
//! - config: Print or edit active configuration
//! - explain: Show recent governor scaling decisions
//! - version: Print version and component status

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use log::LevelFilter;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, IsTerminal};
use std::path::PathBuf;
use std::process::Command;

use claude_governor::capacity_summary::{generate_capacity_summary, StatusExitCode};
use claude_governor::collector;
use claude_governor::config::GovernorConfig;
use claude_governor::db;
use claude_governor::doctor;
use claude_governor::governor;
use claude_governor::narrator;
use claude_governor::poller::{Poller, UsageData};
use claude_governor::schedule;
use claude_governor::simulator::{self, SimConfig};
use claude_governor::state::{self, GovernorState};
use claude_governor::status_display::{format_status_dashboard, format_status_json};

/// Default state file path
fn default_state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("claude-governor")
        .join("governor-state.json")
}

/// Default log file path
fn default_log_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("claude-governor")
        .join("governor.log")
}

#[derive(Parser)]
#[command(name = "cgov")]
#[command(about = "Claude Governor - automated capacity governor for Claude Code", long_about = None)]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Poll usage data from the Anthropic API
    Poll {
        /// Output format: human, json
        #[arg(short, long, default_value = "human")]
        format: String,

        /// Exit with non-zero status if data is stale
        #[arg(long, default_value = "false")]
        fail_on_stale: bool,
    },

    /// Show window capacity forecasts
    Forecast {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Show worker count, targets, and heartbeat status per agent
    Workers {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Manually override target worker count for one cycle
    Scale {
        /// Target worker count
        count: u32,

        /// Show what would happen without acting
        #[arg(long)]
        dry_run: bool,
    },

    /// Show capacity status with exit codes for script integration
    Status {
        /// Output in JSON format
        #[arg(long)]
        json: bool,

        /// Output capacity summary for NEEDLE prompt injection
        #[arg(long)]
        summary: bool,
    },

    /// Tail governor.log
    Logs {
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,

        /// Number of lines to show (default: 50)
        #[arg(short, long, default_value = "50")]
        lines: usize,
    },

    /// Print active configuration or open in editor
    Config {
        /// Open configuration file in $EDITOR
        #[arg(long)]
        edit: bool,
    },

    /// Simulate future capacity trajectory under configurable scenarios
    Simulate {
        /// Worker count or schedule (e.g., "4" or "4:6h,2:6h")
        #[arg(short, long, default_value = "2")]
        workers: String,

        /// Hours to simulate
        #[arg(long, default_value = "24")]
        hours: f64,

        /// Output resolution in minutes
        #[arg(short = 'r', long, default_value = "15")]
        resolution: i64,

        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Run one token collection pass (or start daemon)
    Collect {
        /// Run in continuous daemon mode
        #[arg(long)]
        daemon: bool,

        /// Collection interval in seconds (daemon mode, default: 120)
        #[arg(short, long, default_value = "120")]
        interval: u64,
    },

    /// Query token history from SQLite mirror
    TokenHistory {
        /// Show last N window records
        #[arg(long)]
        last: Option<usize>,

        /// Show instance comparison view
        #[arg(long)]
        compare: bool,

        /// Show last N fleet records
        #[arg(long, name = "fleet")]
        fleet: bool,

        /// Rebuild SQLite from JSONL source
        #[arg(long)]
        rebuild_db: bool,

        /// Number of records for --last and --fleet (default: 10)
        #[arg(short = 'n', long, default_value = "10")]
        count: usize,

        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Print version, build info, and component status
    Version,

    /// Run the governor daemon (main capacity management loop)
    Daemon {
        /// Show what would happen without actually scaling workers
        #[arg(long)]
        dry_run: bool,

        /// Loop interval in seconds (overrides config)
        #[arg(short = 'i', long)]
        interval: Option<u64>,

        /// Hysteresis band for scaling decisions (overrides config)
        #[arg(long)]
        hysteresis: Option<f64>,

        /// Target utilization ceiling percentage (overrides config)
        #[arg(short = 'c', long)]
        ceiling: Option<f64>,
    },

    /// Initialize claude-governor configuration and systemd service
    Init {
        /// Force overwrite existing configuration
        #[arg(long)]
        force: bool,

        /// Skip systemd service installation
        #[arg(long)]
        no_systemd: bool,
    },

    /// Enable claude-governor services (install systemd units and start)
    Enable {
        /// Force overwrite existing service files
        #[arg(long)]
        force: bool,
    },

    /// Disable claude-governor services (stop and remove systemd units)
    Disable {
        /// Also remove service files
        #[arg(long)]
        purge: bool,
    },

    /// Start claude-governor daemon services
    Start {
        /// Service to start: governor, collector, or all (default: all)
        #[arg(long, default_value = "all")]
        service: String,
    },

    /// Stop claude-governor daemon services
    Stop {
        /// Service to stop: governor, collector, or all (default: all)
        #[arg(long, default_value = "all")]
        service: String,
    },

    /// Restart claude-governor daemon services
    Restart {
        /// Service to restart: governor, collector, or all (default: all)
        #[arg(long, default_value = "all")]
        service: String,
    },

    /// Show recent governor scaling decisions from the audit log
    Explain {
        /// Number of recent decisions to show (default: 5)
        #[arg(short = 'n', long, default_value = "5")]
        last: usize,

        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Run health diagnostic checks
    Doctor {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Internal: Run the governor daemon (called by systemd)
    #[command(hide = true, name = "_daemon")]
    _Daemon {
        /// Show what would happen without actually scaling workers
        #[arg(long)]
        dry_run: bool,

        /// Loop interval in seconds (overrides config)
        #[arg(short = 'i', long)]
        interval: Option<u64>,

        /// Hysteresis band for scaling decisions (overrides config)
        #[arg(long)]
        hysteresis: Option<f64>,

        /// Target utilization ceiling percentage (overrides config)
        #[arg(short = 'c', long)]
        ceiling: Option<f64>,
    },

    /// Internal: Run the token collector daemon (called by systemd)
    #[command(hide = true, name = "_token-collector")]
    _TokenCollector {
        /// Collection interval in seconds (default: 120)
        #[arg(short, long, default_value = "120")]
        interval: u64,
    },
}

/// Format usage data for human consumption
fn format_human(data: &UsageData) -> String {
    format!(
        "Claude Usage Report (as of {})\n\
         ===================================\n\
         \n\
         Five Hour Window:\n\
         - Utilization: {:.1}%\n\
         - Resets At: {}\n\
         - Hours Remaining: {:.2}\n\
         \n\
         Seven Day Window:\n\
         - Utilization: {:.1}%\n\
         - Resets At: {}\n\
         - Hours Remaining: {:.2}\n\
         \n\
         Seven Day Sonnet Window:\n\
         - Utilization: {:.1}%\n\
         - Resets At: {}\n\
         - Hours Remaining: {:.2}\n\
         \n\
         Status: {}",
        data.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
        data.five_hour_utilization,
        data.five_hour_resets_at,
        data.five_hour_hours_remaining,
        data.seven_day_utilization,
        data.seven_day_resets_at,
        data.seven_day_hours_remaining,
        data.seven_day_sonnet_utilization,
        data.seven_day_sonnet_resets_at,
        data.seven_day_sonnet_hours_remaining,
        if data.stale {
            "STALE (auth errors)"
        } else {
            "OK"
        }
    )
}

/// Format usage data as JSON
fn format_json(data: &UsageData) -> String {
    serde_json::json!({
        "timestamp": data.timestamp.to_rfc3339(),
        "five_hour": {
            "utilization": data.five_hour_utilization,
            "resets_at": data.five_hour_resets_at,
            "hours_remaining": data.five_hour_hours_remaining,
        },
        "seven_day": {
            "utilization": data.seven_day_utilization,
            "resets_at": data.seven_day_resets_at,
            "hours_remaining": data.seven_day_hours_remaining,
        },
        "seven_day_sonnet": {
            "utilization": data.seven_day_sonnet_utilization,
            "resets_at": data.seven_day_sonnet_resets_at,
            "hours_remaining": data.seven_day_sonnet_hours_remaining,
        },
        "stale": data.stale,
    })
    .to_string()
}

fn run_poll_command(format: &str, fail_on_stale: bool) -> Result<()> {
    let mut poller = Poller::new()?;

    log::debug!("Polling usage data from API...");
    let data = poller.poll()?;

    // Check if we should alert about refresh failures
    if poller.should_alert() {
        eprintln!("WARNING: OAuth token refresh failing - run: claude login");
    }

    let output = match format.to_lowercase().as_str() {
        "json" => format_json(&data),
        _ => format_human(&data),
    };

    println!("{}", output);

    // Exit with non-zero if data is stale and fail_on_stale is set
    if data.stale && fail_on_stale {
        anyhow::bail!("Usage data is stale due to authentication errors");
    }

    Ok(())
}

/// Format forecast for human consumption
fn format_forecast_human(state: &GovernorState) -> String {
    let forecast = &state.capacity_forecast;
    let mut output = String::new();

    output.push_str("Claude Capacity Forecast\n");
    output.push_str("========================\n\n");

    let windows = [
        ("Five Hour", &forecast.five_hour),
        ("Seven Day", &forecast.seven_day),
        ("Seven Day Sonnet", &forecast.seven_day_sonnet),
    ];

    for (name, win) in windows {
        let binding = if win.binding { " [BINDING]" } else { "" };
        let cutoff = if win.cutoff_risk {
            " ⚠️ CUTOFF RISK"
        } else {
            ""
        };
        output.push_str(&format!(
            "{}{}\n  Utilization: {:.1}% / {:.0}% ceiling\n  Remaining: {:.1}% ({:.1}h)\n  Burn Rate: {:.2}%/hr\n  Exhaustion: {:.1}h {}\n  Margin: {:.1}h\n\n",
            name,
            binding,
            win.current_utilization,
            win.target_ceiling,
            win.remaining_pct,
            win.hours_remaining,
            win.fleet_pct_per_hour,
            win.predicted_exhaustion_hours,
            cutoff,
            win.margin_hrs
        ));
    }

    output.push_str(&format!(
        "Binding Window: {}\n",
        if forecast.binding_window.is_empty() {
            "none"
        } else {
            &forecast.binding_window
        }
    ));
    output.push_str(&format!(
        "Remaining Budget: ~${:.2}\n",
        forecast.estimated_remaining_dollars
    ));

    output
}

/// Format forecast as JSON
fn format_forecast_json(state: &GovernorState) -> String {
    serde_json::to_string_pretty(&state.capacity_forecast).unwrap_or_else(|e| {
        serde_json::json!({"error": format!("Serialization error: {}", e)}).to_string()
    })
}

fn run_forecast_command(json: bool) -> Result<()> {
    let state_path = default_state_path();
    let state = state::load_state(&state_path)?;

    let output = if json {
        format_forecast_json(&state)
    } else {
        format_forecast_human(&state)
    };

    println!("{}", output);
    Ok(())
}

/// Format workers for human consumption
fn format_workers_human(state: &GovernorState) -> String {
    let mut output = String::new();

    output.push_str("Worker Status\n");
    output.push_str("=============\n\n");

    if state.workers.is_empty() {
        output.push_str("No workers configured.\n");
        return output;
    }

    for (agent_id, worker) in &state.workers {
        output.push_str(&format!(
            "Agent: {}\n  Current: {} workers\n  Target: {} workers\n  Range: {} - {}\n\n",
            agent_id, worker.current, worker.target, worker.min, worker.max
        ));
    }

    // Fleet aggregate info
    let fleet = &state.last_fleet_aggregate;
    output.push_str(&format!(
        "Fleet: {} workers, ${:.2}/hr total\n",
        fleet.sonnet_workers, fleet.sonnet_usd_total
    ));

    output
}

/// Format workers as JSON
fn format_workers_json(state: &GovernorState) -> String {
    let workers_data = serde_json::json!({
        "workers": state.workers,
        "fleet": {
            "sonnet_workers": state.last_fleet_aggregate.sonnet_workers,
            "usd_total": state.last_fleet_aggregate.sonnet_usd_total,
            "p75_usd_hr": state.last_fleet_aggregate.sonnet_p75_usd_hr,
        }
    });
    serde_json::to_string_pretty(&workers_data).unwrap_or_else(|e| {
        serde_json::json!({"error": format!("Serialization error: {}", e)}).to_string()
    })
}

fn run_workers_command(json: bool) -> Result<()> {
    let state_path = default_state_path();
    let state = state::load_state(&state_path)?;

    let output = if json {
        format_workers_json(&state)
    } else {
        format_workers_human(&state)
    };

    println!("{}", output);
    Ok(())
}

fn run_scale_command(count: u32, dry_run: bool) -> Result<()> {
    let state_path = default_state_path();
    let mut state = state::load_state(&state_path)?;

    // Check if safe mode is active
    if state.safe_mode.active {
        log::warn!(
            "Scale command issued during safe mode - this may be overridden by emergency brake"
        );
    }

    // Validate count against worker limits
    for (agent_id, worker) in &state.workers {
        if count < worker.min || count > worker.max {
            anyhow::bail!(
                "Worker count {} is outside allowed range for agent {} ({} - {})",
                count,
                agent_id,
                worker.min,
                worker.max
            );
        }
    }

    if dry_run {
        println!("DRY RUN: Would set target worker count to {}", count);
        for (agent_id, worker) in &state.workers {
            println!("  Agent {}: {} -> {}", agent_id, worker.target, count);
        }
        return Ok(());
    }

    // Apply the scale command
    for worker in state.workers.values_mut() {
        worker.target = count;
    }

    state.updated_at = Utc::now();
    state::save_state(&state, &state_path)?;

    println!("Target worker count set to {} for all agents", count);
    Ok(())
}

fn run_logs_command(follow: bool, lines: usize) -> Result<()> {
    let log_path = default_log_path();

    if !log_path.exists() {
        anyhow::bail!("Log file not found: {}", log_path.display());
    }

    if follow {
        // Use tail -f for following
        let status = Command::new("tail")
            .arg("-f")
            .arg("-n")
            .arg(lines.to_string())
            .arg(&log_path)
            .status()?;

        if !status.success() {
            anyhow::bail!("tail command failed");
        }
    } else {
        // Read and print last N lines
        let file = fs::File::open(&log_path)?;
        let reader = BufReader::new(file);
        let all_lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

        let start = if all_lines.len() > lines {
            all_lines.len() - lines
        } else {
            0
        };

        for line in &all_lines[start..] {
            println!("{}", line);
        }
    }

    Ok(())
}

fn run_config_command(edit: bool) -> Result<()> {
    // Find the config file path
    let config_path = if let Ok(xdg_config) = env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg_config).join("claude-governor/governor.yaml")
    } else if let Some(home) = dirs::home_dir() {
        home.join(".config/claude-governor/governor.yaml")
    } else {
        PathBuf::from("config/governor.yaml")
    };

    if edit {
        // Open in editor
        let editor = env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
        let status = Command::new(&editor).arg(&config_path).status()?;

        if !status.success() {
            anyhow::bail!("Editor exited with error");
        }
        Ok(())
    } else {
        // Print active config
        if !config_path.exists() {
            anyhow::bail!("Config file not found: {}", config_path.display());
        }

        let config = GovernorConfig::load_from_path(&config_path)?;
        let output = serde_yaml::to_string(&config)?;
        println!("Config file: {}\n", config_path.display());
        println!("{}", output);
        Ok(())
    }
}

/// Check if a systemd user service is active
fn systemd_service_is_active(service: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", service])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Get daemon status as a string with detection method
fn daemon_status_string() -> String {
    if systemd_user_available() && systemd_service_is_active(GOVERNOR_SERVICE) {
        return "✓ running (systemd)".to_string();
    }
    if tmux_available() && tmux_session_exists(GOVERNOR_SESSION) {
        return "✓ running (tmux)".to_string();
    }
    let state_path = default_state_path();
    if let Ok(state) = state::load_state(&state_path) {
        let age = (Utc::now() - state.updated_at).num_seconds();
        if age < 300 {
            return format!("✓ active (state {}s old)", age);
        }
    }
    "✗ stopped".to_string()
}

/// Get collector status as a string with detection method
fn collector_status_string() -> String {
    if systemd_user_available() && systemd_service_is_active(COLLECTOR_SERVICE) {
        return "✓ running (systemd)".to_string();
    }
    if tmux_available() && tmux_session_exists(COLLECTOR_SESSION) {
        return "✓ running (tmux)".to_string();
    }
    let state_path = default_state_path();
    if let Ok(state) = state::load_state(&state_path) {
        let age = (Utc::now() - state.last_fleet_aggregate.t1).num_seconds();
        if age < 300 {
            return format!("✓ active (fleet {}s old)", age);
        }
    }
    "✗ stopped".to_string()
}

/// Get the detected daemon mode as a string
fn detected_daemon_mode_string() -> String {
    if systemd_user_available() {
        "systemd".to_string()
    } else if tmux_available() {
        "tmux".to_string()
    } else {
        "standalone".to_string()
    }
}

fn run_version_command() -> Result<()> {
    println!("cgov v{}", env!("CARGO_PKG_VERSION"));
    println!("Claude Governor - automated capacity governor for Claude Code");
    println!();
    println!("Component Status:");
    println!("  Daemon:    {}", daemon_status_string());
    println!("  Collector: {}", collector_status_string());
    println!("  Mode:      {}", detected_daemon_mode_string());
    println!();
    println!("Build Info:");
    println!(
        "  Target:    {}",
        option_env!("TARGET").unwrap_or("unknown")
    );
    println!(
        "  Profile:   {}",
        option_env!("PROFILE").unwrap_or("unknown")
    );
    println!(
        "  Rust:      {}",
        option_env!("RUSTC_VERSION").unwrap_or("unknown")
    );

    Ok(())
}

fn run_explain_command(last: usize, json: bool) -> Result<()> {
    let decisions =
        narrator::read_last_decisions(last).with_context(|| "Failed to read decision log")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&decisions)?);
    } else {
        print!("{}", narrator::format_decisions_human(&decisions));
    }

    Ok(())
}

fn run_doctor_command(json: bool) -> Result<()> {
    let report = doctor::run_doctor();

    if json {
        println!("{}", doctor::format_doctor_json(&report));
    } else {
        print!("{}", doctor::format_doctor_human(&report));
    }

    // Exit with non-zero if any checks failed
    if report.failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

fn default_promotions_path() -> PathBuf {
    PathBuf::from("config/promotions.json")
}

fn run_simulate_command(workers: &str, hours: f64, resolution: i64, json: bool) -> Result<()> {
    let state_path = default_state_path();
    let state = state::load_state(&state_path)?;

    // Parse worker schedule
    let mut config = SimConfig::parse_workers(workers).map_err(|e| anyhow::anyhow!("{}", e))?;
    config.hours = hours;
    config.resolution_minutes = resolution;

    // Load promotions
    let promo_path = default_promotions_path();
    let promotions = schedule::load_promotions(&promo_path);

    // Run simulation (read-only)
    let trajectory =
        simulator::simulate(&state, &config, promotions).map_err(|e| anyhow::anyhow!("{}", e))?;

    let output = if json {
        serde_json::to_string_pretty(&trajectory).map_err(|e| anyhow::anyhow!("{}", e))?
    } else {
        simulator::format_ascii_table(&trajectory)
    };

    println!("{}", output);
    Ok(())
}

fn run_token_history_command(
    last: Option<usize>,
    compare: bool,
    fleet: bool,
    rebuild_db: bool,
    count: usize,
    json: bool,
) -> Result<()> {
    let history_path = collector::default_history_path();
    let db_path = collector::default_db_path();

    // Handle --rebuild-db
    if rebuild_db {
        let n = db::rebuild_from_jsonl(&history_path, &db_path)?;
        println!("Rebuilt SQLite from JSONL: {} records", n);
        return Ok(());
    }

    let conn = db::open_db(&db_path)?;
    db::create_schema(&conn)?;

    // Handle --compare
    if compare {
        let results = db::query_instance_compare(&conn, count)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else {
            println!(
                "{:<30} {:<30} {:>10} {:>10} {:>10}",
                "session", "model", "total_usd", "usd/hr", "usd/%7ds"
            );
            println!("{}", "-".repeat(95));
            for r in &results {
                let usd_pct = match r.get("usd_per_pct_7ds") {
                    Some(v) if !v.is_null() => format!("{:>10.4}", v.as_f64().unwrap_or(0.0)),
                    _ => "       N/A".to_string(),
                };
                println!(
                    "{:<30} {:<30} {:>10.4} {:>10.4} {}",
                    r["sess"].as_str().unwrap_or("?"),
                    r["model"].as_str().unwrap_or("?"),
                    r["total_usd"].as_f64().unwrap_or(0.0),
                    r["usd_per_hour"].as_f64().unwrap_or(0.0),
                    usd_pct,
                );
            }
        }
        return Ok(());
    }

    // Handle --fleet
    if fleet {
        let results = db::query_last_fleets(&conn, count)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else {
            for r in &results {
                let ts = r.get("ts").and_then(|v| v.as_str()).unwrap_or("?");
                let workers = r.get("workers").and_then(|v| v.as_u64()).unwrap_or(0);
                let total = r.get("total-usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let p75 = r.get("p75-usd-hr").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let std = r.get("std-usd-hr").and_then(|v| v.as_f64()).unwrap_or(0.0);
                println!(
                    "{} | workers={} total=${:.4} p75=${:.2}/hr std=${:.2}/hr",
                    ts, workers, total, p75, std,
                );
            }
        }
        return Ok(());
    }

    // Handle --last (default): show recent instance records with USD costs
    let n = last.unwrap_or(count);
    let results = db::query_last_instances(&conn, n)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        if results.is_empty() {
            println!("No instance records found. Run `cgov collect` to populate.");
        } else {
            println!(
                "{:<32} {:<30} {:>10} {:>8} {:>8}",
                "ts", "model", "total_usd", "in_tok", "out_tok"
            );
            println!("{}", "-".repeat(94));
            for r in &results {
                let ts = r.get("ts").and_then(|v| v.as_str()).unwrap_or("?");
                let model = r.get("model").and_then(|v| v.as_str()).unwrap_or("?");
                let usd = r.get("total-usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let in_n = r.get("input-n").and_then(|v| v.as_i64()).unwrap_or(0);
                let out_n = r.get("output-n").and_then(|v| v.as_i64()).unwrap_or(0);
                println!(
                    "{:<32} {:<30} {:>10.4} {:>8} {:>8}",
                    ts, model, usd, in_n, out_n
                );
            }
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logger
    let log_level = if cli.verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    env_logger::Builder::new().filter_level(log_level).init();

    match cli.command {
        Commands::Poll {
            format,
            fail_on_stale,
        } => {
            run_poll_command(&format, fail_on_stale)?;
        }
        Commands::Forecast { json } => {
            run_forecast_command(json)?;
        }
        Commands::Workers { json } => {
            run_workers_command(json)?;
        }
        Commands::Scale { count, dry_run } => {
            run_scale_command(count, dry_run)?;
        }
        Commands::Logs { follow, lines } => {
            run_logs_command(follow, lines)?;
        }
        Commands::Config { edit } => {
            run_config_command(edit)?;
        }
        Commands::Simulate {
            workers,
            hours,
            resolution,
            json,
        } => {
            run_simulate_command(&workers, hours, resolution, json)?;
        }
        Commands::Status { json, summary } => {
            let state_path = default_state_path();
            let state = state::load_state(&state_path)?;
            let exit_code = StatusExitCode::from_state(&state);

            // --summary: NEEDLE prompt injection format
            if summary {
                print!("{}", generate_capacity_summary(&state));
                std::process::exit(exit_code.as_exit_code());
            }

            // --json or non-TTY: raw state JSON
            if json || !std::io::stdout().is_terminal() {
                println!("{}", format_status_json(&state));
                std::process::exit(exit_code.as_exit_code());
            }

            // Default: rich human-readable dashboard
            print!("{}", format_status_dashboard(&state, chrono::Utc::now()));
            std::process::exit(exit_code.as_exit_code());
        }
        Commands::Version => {
            run_version_command()?;
        }
        Commands::Collect { daemon, interval } => {
            if daemon {
                collector::run_daemon(interval)?;
            } else {
                let result = collector::run_collection_pass()?;
                println!(
                    "Collection complete: {} lines, {} instances, {} fleet records, ${:.4} total",
                    result.lines_processed,
                    result.instance_records,
                    result.fleet_records,
                    result.total_usd,
                );
            }
        }
        Commands::TokenHistory {
            last,
            compare,
            fleet,
            rebuild_db,
            count,
            json,
        } => {
            run_token_history_command(last, compare, fleet, rebuild_db, count, json)?;
        }
        Commands::Daemon {
            dry_run,
            interval,
            hysteresis,
            ceiling,
        } => {
            run_daemon_command(dry_run, interval, hysteresis, ceiling)?;
        }
        Commands::Init { force, no_systemd } => {
            run_init_command(force, no_systemd)?;
        }
        Commands::Enable { force } => {
            run_enable_command(force)?;
        }
        Commands::Disable { purge } => {
            run_disable_command(purge)?;
        }
        Commands::Start { service } => {
            run_start_command(&service)?;
        }
        Commands::Stop { service } => {
            run_stop_command(&service)?;
        }
        Commands::Restart { service } => {
            run_restart_command(&service)?;
        }
        Commands::Explain { last, json } => {
            run_explain_command(last, json)?;
        }
        Commands::Doctor { json } => {
            run_doctor_command(json)?;
        }
        Commands::_Daemon {
            dry_run,
            interval,
            hysteresis,
            ceiling,
        } => {
            run_internal_daemon_command(dry_run, interval, hysteresis, ceiling)?;
        }
        Commands::_TokenCollector { interval } => {
            run_internal_token_collector_command(interval)?;
        }
    }

    Ok(())
}

fn run_daemon_command(
    dry_run: bool,
    interval: Option<u64>,
    hysteresis: Option<f64>,
    ceiling: Option<f64>,
) -> Result<()> {
    let config = GovernorConfig::load()?;
    let daemon = &config.daemon;

    let loop_interval = interval.unwrap_or(daemon.loop_interval_secs);
    let hysteresis_band = hysteresis.unwrap_or(daemon.hysteresis_band);
    let target_ceiling = ceiling.unwrap_or(daemon.target_ceiling);

    // Load promotions from config file
    let promo_path = default_promotions_path();
    let promotions = schedule::load_promotions(&promo_path);

    let state_path = default_state_path();

    governor::run_daemon(
        &state_path,
        dry_run,
        loop_interval,
        hysteresis_band,
        daemon.max_scale_up_per_cycle,
        daemon.max_scale_down_per_cycle,
        target_ceiling,
        &config.alerts,
        &config.agents,
        daemon.pre_scale_minutes,
        &promotions,
        &config.composite_risk,
        &config.cone_scaling,
    )
}

// --- Systemd helpers ---

/// Check if systemd user sessions are available
fn systemd_user_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "status"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if tmux is available
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get the systemd user unit directory
fn systemd_user_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("systemd/user"))
}

/// Run a systemctl --user command and return success/failure
fn systemctl_user(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(["--user"])
        .args(args)
        .status()
        .with_context(|| format!("Failed to run systemctl --user {}", args.join(" ")))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "systemctl --user {} failed with exit code {:?}",
            args.join(" "),
            status.code()
        )
    }
}

/// Service names for the two-unit split
const GOVERNOR_SERVICE: &str = "claude-governor.service";
const COLLECTOR_SERVICE: &str = "claude-token-collector.service";

/// Resolve --service arg to a list of unit names
fn resolve_service_names(service: &str) -> Vec<&'static str> {
    match service {
        "governor" => vec![GOVERNOR_SERVICE],
        "collector" => vec![COLLECTOR_SERVICE],
        _ => vec![GOVERNOR_SERVICE, COLLECTOR_SERVICE],
    }
}

// --- Daemon mode resolution ---

/// Resolve the effective daemon mode from config, falling back to auto-detection
fn resolve_daemon_mode(config: &GovernorConfig) -> &'static str {
    match &config.daemon.mode {
        claude_governor::config::DaemonMode::Systemd => "systemd",
        claude_governor::config::DaemonMode::Tmux => "tmux",
        claude_governor::config::DaemonMode::Auto => {
            if systemd_user_available() {
                "systemd"
            } else if tmux_available() {
                "tmux"
            } else {
                "none"
            }
        }
    }
}

// --- tmux session management ---

const GOVERNOR_SESSION: &str = "cgov-governor";
const COLLECTOR_SESSION: &str = "cgov-collector";

fn tmux_session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tmux_kill_session(session: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status()
        .with_context(|| format!("Failed to kill tmux session {}", session))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("tmux kill-session -t {} failed", session);
    }
}

fn tmux_start_session(session: &str, command: &str) -> Result<()> {
    if tmux_session_exists(session) {
        println!("  - {} is already running", session);
        return Ok(());
    }

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session, command])
        .status()
        .with_context(|| format!("Failed to start tmux session {}", session))?;
    if status.success() {
        println!("  ✓ Started {} (tmux session)", session);
        Ok(())
    } else {
        anyhow::bail!("tmux new-session -d -s {} {} failed", session, command);
    }
}

fn resolve_tmux_sessions(service: &str) -> Vec<&'static str> {
    match service {
        "governor" => vec![GOVERNOR_SESSION],
        "collector" => vec![COLLECTOR_SESSION],
        _ => vec![GOVERNOR_SESSION, COLLECTOR_SESSION],
    }
}

// --- Lifecycle commands ---

fn run_enable_command(force: bool) -> Result<()> {
    let config = GovernorConfig::load()?;
    let mode = resolve_daemon_mode(&config);

    if mode == "tmux" {
        // tmux mode: just start the sessions
        println!("Detected tmux mode — starting daemon sessions");
        println!("=============================================\n");
        tmux_start_session(GOVERNOR_SESSION, "cgov _daemon")?;
        tmux_start_session(COLLECTOR_SESSION, "cgov _token-collector")?;
        println!("\nAttach to sessions:");
        println!("  tmux attach -t {}", GOVERNOR_SESSION);
        println!("  tmux attach -t {}", COLLECTOR_SESSION);
        return Ok(());
    }

    if mode == "none" {
        anyhow::bail!(
            "No daemon backend available.\n\
             Ensure systemd --user is enabled (loginctl enable-linger $USER) or tmux is installed."
        );
    }

    // systemd mode
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

    let user_dir = systemd_user_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine systemd user directory"))?;
    if !user_dir.exists() {
        fs::create_dir_all(&user_dir)
            .with_context(|| format!("Failed to create {}", user_dir.display()))?;
    }

    let mut actions = Vec::new();

    // Install both unit files
    for (name, content) in [
        (
            GOVERNOR_SERVICE,
            include_str!("../config/claude-governor.service"),
        ),
        (
            COLLECTOR_SERVICE,
            include_str!("../config/claude-token-collector.service"),
        ),
    ] {
        let path = user_dir.join(name);
        let content = content.replace("%h", home.to_str().unwrap_or("~"));

        if path.exists() && !force {
            actions.push(format!(
                "  - {} already exists (use --force to overwrite)",
                name
            ));
        } else {
            fs::write(&path, &content)
                .with_context(|| format!("Failed to write {}", path.display()))?;
            actions.push(format!("  ✓ Installed {}", path.display()));
        }
    }

    // Daemon-reload
    systemctl_user(&["daemon-reload"])?;
    actions.push("  ✓ Ran daemon-reload".to_string());

    // Enable both services
    systemctl_user(&["enable", GOVERNOR_SERVICE])?;
    actions.push(format!("  ✓ Enabled {}", GOVERNOR_SERVICE));

    systemctl_user(&["enable", COLLECTOR_SERVICE])?;
    actions.push(format!("  ✓ Enabled {}", COLLECTOR_SERVICE));

    // Start both services
    systemctl_user(&["start", GOVERNOR_SERVICE])?;
    actions.push(format!("  ✓ Started {}", GOVERNOR_SERVICE));

    systemctl_user(&["start", COLLECTOR_SERVICE])?;
    actions.push(format!("  ✓ Started {}", COLLECTOR_SERVICE));

    println!("Claude Governor services enabled and started (systemd)");
    println!("======================================================\n");
    for action in &actions {
        println!("{}", action);
    }
    println!("\nView logs:");
    println!("  journalctl --user -u {} -f", GOVERNOR_SERVICE);
    println!("  journalctl --user -u {} -f", COLLECTOR_SERVICE);

    Ok(())
}

fn run_disable_command(purge: bool) -> Result<()> {
    let config = GovernorConfig::load()?;
    let mode = resolve_daemon_mode(&config);

    if mode == "tmux" || mode == "none" {
        // Kill tmux sessions if they exist
        let mut actions = Vec::new();
        for session in [GOVERNOR_SESSION, COLLECTOR_SESSION] {
            if tmux_session_exists(session) {
                tmux_kill_session(session)?;
                actions.push(format!("  ✓ Killed tmux session {}", session));
            } else {
                actions.push(format!("  - {} is not running", session));
            }
        }
        println!("Claude Governor sessions disabled (tmux)\n");
        for action in &actions {
            println!("{}", action);
        }
        return Ok(());
    }

    // systemd mode
    if !systemd_user_available() {
        anyhow::bail!("systemd user sessions not available");
    }

    let mut actions = Vec::new();

    // Stop both services (ignore errors if not running)
    for svc in [GOVERNOR_SERVICE, COLLECTOR_SERVICE] {
        if systemctl_user(&["is-active", svc]).is_ok() {
            systemctl_user(&["stop", svc])?;
            actions.push(format!("  ✓ Stopped {}", svc));
        }
    }

    // Disable both services
    for svc in [GOVERNOR_SERVICE, COLLECTOR_SERVICE] {
        if systemctl_user(&["is-enabled", svc]).is_ok() {
            systemctl_user(&["disable", svc])?;
            actions.push(format!("  ✓ Disabled {}", svc));
        }
    }

    // Optionally purge service files
    if purge {
        let user_dir = systemd_user_dir();
        if let Some(dir) = user_dir {
            for svc in [GOVERNOR_SERVICE, COLLECTOR_SERVICE] {
                let path = dir.join(svc);
                if path.exists() {
                    fs::remove_file(&path)
                        .with_context(|| format!("Failed to remove {}", path.display()))?;
                    actions.push(format!("  ✓ Removed {}", path.display()));
                }
            }
            systemctl_user(&["daemon-reload"])?;
            actions.push("  ✓ Ran daemon-reload".to_string());
        }
    }

    println!(
        "Claude Governor services disabled{}\n",
        if purge { " and purged" } else { "" }
    );
    for action in &actions {
        println!("{}", action);
    }

    Ok(())
}

fn run_start_command(service: &str) -> Result<()> {
    let config = GovernorConfig::load()?;
    let mode = resolve_daemon_mode(&config);

    if mode == "tmux" || (!systemd_user_available() && tmux_available()) {
        let sessions = resolve_tmux_sessions(service);
        for session in &sessions {
            let cmd = match *session {
                GOVERNOR_SESSION => "cgov _daemon",
                COLLECTOR_SESSION => "cgov _token-collector",
                _ => unreachable!(),
            };
            tmux_start_session(session, cmd)?;
        }
        return Ok(());
    }

    if !systemd_user_available() {
        anyhow::bail!(
            "systemd user sessions not available and tmux not found.\n\
             Install tmux or enable systemd --user (loginctl enable-linger $USER)"
        );
    }

    let names = resolve_service_names(service);
    for name in &names {
        systemctl_user(&["start", name])?;
        println!("  ✓ Started {}", name);
    }

    Ok(())
}

fn run_stop_command(service: &str) -> Result<()> {
    let config = GovernorConfig::load()?;
    let mode = resolve_daemon_mode(&config);

    if mode == "tmux" || (!systemd_user_available() && tmux_available()) {
        let sessions = resolve_tmux_sessions(service);
        for session in &sessions {
            if tmux_session_exists(session) {
                tmux_kill_session(session)?;
                println!("  ✓ Stopped {}", session);
            } else {
                println!("  - {} is not running", session);
            }
        }
        return Ok(());
    }

    if !systemd_user_available() {
        anyhow::bail!("systemd user sessions not available");
    }

    let names = resolve_service_names(service);
    for name in &names {
        if systemctl_user(&["is-active", name]).is_ok() {
            systemctl_user(&["stop", name])?;
            println!("  ✓ Stopped {}", name);
        } else {
            println!("  - {} is not running", name);
        }
    }

    Ok(())
}

fn run_restart_command(service: &str) -> Result<()> {
    let config = GovernorConfig::load()?;
    let mode = resolve_daemon_mode(&config);

    if mode == "tmux" || (!systemd_user_available() && tmux_available()) {
        let sessions = resolve_tmux_sessions(service);
        for session in &sessions {
            if tmux_session_exists(session) {
                tmux_kill_session(session)?;
            }
            let cmd = match *session {
                GOVERNOR_SESSION => "cgov _daemon",
                COLLECTOR_SESSION => "cgov _token-collector",
                _ => unreachable!(),
            };
            tmux_start_session(session, cmd)?;
        }
        return Ok(());
    }

    if !systemd_user_available() {
        anyhow::bail!("systemd user sessions not available");
    }

    let names = resolve_service_names(service);
    for name in &names {
        systemctl_user(&["restart", name])?;
        println!("  ✓ Restarted {}", name);
    }

    Ok(())
}

fn run_internal_daemon_command(
    dry_run: bool,
    interval: Option<u64>,
    hysteresis: Option<f64>,
    ceiling: Option<f64>,
) -> Result<()> {
    // Identical to run_daemon_command — called by systemd unit file
    run_daemon_command(dry_run, interval, hysteresis, ceiling)
}

fn run_internal_token_collector_command(interval: u64) -> Result<()> {
    // Identical to collect --daemon — called by systemd unit file
    collector::run_daemon(interval)
}

fn run_init_command(force: bool, no_systemd: bool) -> Result<()> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

    let mut actions_taken = Vec::new();
    let mut actions_skipped = Vec::new();

    // 1. Create ~/.config/claude-governor/ directory
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
        .join("claude-governor");

    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).with_context(|| {
            format!(
                "Failed to create config directory: {}",
                config_dir.display()
            )
        })?;
        actions_taken.push(format!(
            "Created config directory: {}",
            config_dir.display()
        ));
    } else {
        actions_skipped.push(format!("Config directory exists: {}", config_dir.display()));
    }

    // 2. Copy default governor.yaml (skip if exists and not force)
    let config_path = config_dir.join("governor.yaml");
    if config_path.exists() && !force {
        actions_skipped.push(format!(
            "Config file exists (use --force to overwrite): {}",
            config_path.display()
        ));
    } else {
        let default_yaml = include_str!("../config/governor.yaml");
        fs::write(&config_path, default_yaml)
            .with_context(|| format!("Failed to write config file: {}", config_path.display()))?;
        if force && config_path.exists() {
            actions_taken.push(format!("Overwrote config file: {}", config_path.display()));
        } else {
            actions_taken.push(format!("Created config file: {}", config_path.display()));
        }
    }

    // 3. Create ~/.local/share/claude-governor/ (log directory) and ~/.needle/state/ directories
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| home.join(".local/share"))
        .join("claude-governor");
    let needle_dir = home.join(".needle");
    let state_dir = needle_dir.join("state");

    if !log_dir.exists() {
        fs::create_dir_all(&log_dir)
            .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;
        actions_taken.push(format!("Created log directory: {}", log_dir.display()));
    } else {
        actions_skipped.push(format!("Log directory exists: {}", log_dir.display()));
    }

    if !state_dir.exists() {
        fs::create_dir_all(&state_dir).with_context(|| {
            format!("Failed to create state directory: {}", state_dir.display())
        })?;
        actions_taken.push(format!("Created state directory: {}", state_dir.display()));
    } else {
        actions_skipped.push(format!("State directory exists: {}", state_dir.display()));
    }

    // 4. Detect systemd availability and install user service units
    let systemd_installed = if !no_systemd {
        let systemd_user_dir = dirs::config_dir()
            .map(|p| p.join("systemd/user"))
            .unwrap_or_else(|| home.join(".config/systemd/user"));

        // Check if systemd is available (user sessions)
        let systemd_available = Command::new("systemctl")
            .args(["--user", "status"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if systemd_available {
            if !systemd_user_dir.exists() {
                fs::create_dir_all(&systemd_user_dir).with_context(|| {
                    format!(
                        "Failed to create systemd directory: {}",
                        systemd_user_dir.display()
                    )
                })?;
            }

            // Install both unit files (governor + token-collector)
            for (name, content) in [
                (
                    GOVERNOR_SERVICE,
                    include_str!("../config/claude-governor.service"),
                ),
                (
                    COLLECTOR_SERVICE,
                    include_str!("../config/claude-token-collector.service"),
                ),
            ] {
                let service_path = systemd_user_dir.join(name);
                let service_content = content.replace("%h", home.to_str().unwrap_or("~"));

                if service_path.exists() && !force {
                    actions_skipped.push(format!(
                        "Systemd service exists (use --force to overwrite): {}",
                        service_path.display()
                    ));
                } else {
                    fs::write(&service_path, &service_content).with_context(|| {
                        format!(
                            "Failed to write systemd service: {}",
                            service_path.display()
                        )
                    })?;
                    actions_taken.push(format!(
                        "Installed systemd service: {}",
                        service_path.display()
                    ));
                }
            }

            // Run daemon-reload
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .output();
            actions_taken.push("Ran systemctl --user daemon-reload".to_string());

            true
        } else {
            actions_skipped.push("Systemd user sessions not available".to_string());
            false
        }
    } else {
        actions_skipped.push("Skipped systemd installation (--no-systemd)".to_string());
        false
    };

    // 5. Check for legacy capacity-governor.sh
    let legacy_script = home.join("capacity-governor.sh");
    let legacy_detected = legacy_script.exists();

    // Print summary
    println!("Claude Governor Initialization Complete");
    println!("========================================\n");

    if !actions_taken.is_empty() {
        println!("Actions taken:");
        for action in &actions_taken {
            println!("  ✓ {}", action);
        }
        println!();
    }

    if !actions_skipped.is_empty() {
        println!("Actions skipped:");
        for action in &actions_skipped {
            println!("  - {}", action);
        }
        println!();
    }

    if legacy_detected {
        println!("MIGRATION NOTE:");
        println!("  Legacy script detected: {}", legacy_script.display());
        println!("  Consider migrating your custom logic to governor.yaml and removing the legacy script.");
        println!();
    }

    // Quickstart message
    println!("Quickstart:");
    println!("  1. Edit configuration: cgov config --edit");
    println!("  2. Check status:       cgov status");
    println!("  3. View forecasts:     cgov forecast");
    if systemd_installed {
        println!("  4. Enable services:    cgov enable");
        println!("  5. View logs:          journalctl --user -u claude-governor -f");
    } else {
        println!("  4. Run daemon:         cgov daemon");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn create_test_usage_data() -> UsageData {
        UsageData {
            five_hour_utilization: 45.5,
            five_hour_resets_at: "2026-03-18T20:00:00Z".to_string(),
            five_hour_hours_remaining: 2.5,
            seven_day_utilization: 68.0,
            seven_day_resets_at: "2026-03-20T03:00:00Z".to_string(),
            seven_day_hours_remaining: 32.5,
            seven_day_sonnet_utilization: 72.0,
            seven_day_sonnet_resets_at: "2026-03-20T04:00:00Z".to_string(),
            seven_day_sonnet_hours_remaining: 33.5,
            timestamp: Utc::now(),
            stale: false,
        }
    }

    #[test]
    fn test_format_human() {
        let data = create_test_usage_data();
        let output = format_human(&data);

        assert!(output.contains("45.5%"));
        assert!(output.contains("68.0%"));
        assert!(output.contains("72.0%"));
        assert!(output.contains("OK"));
    }

    #[test]
    fn test_format_human_stale() {
        let mut data = create_test_usage_data();
        data.stale = true;
        let output = format_human(&data);

        assert!(output.contains("STALE"));
    }

    #[test]
    fn test_format_json() {
        let data = create_test_usage_data();
        let json = format_json(&data);

        // Verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["five_hour"]["utilization"], 45.5);
        assert_eq!(parsed["seven_day"]["utilization"], 68.0);
        assert_eq!(parsed["stale"], false);
    }
}
