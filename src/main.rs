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
//! - version: Print version and component status

use anyhow::Result;
use chrono::Utc;
use clap::{Parser, Subcommand};
use log::LevelFilter;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;

use claude_governor::capacity_summary::generate_capacity_summary;
use claude_governor::collector;
use claude_governor::config::GovernorConfig;
use claude_governor::db;
use claude_governor::governor;
use claude_governor::poller::{Poller, UsageData};
use claude_governor::schedule;
use claude_governor::simulator::{self, SimConfig};
use claude_governor::state::{self, GovernorState};

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
        #[arg(short, long, default_value = "24")]
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
        if data.stale { "STALE (auth errors)" } else { "OK" }
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
        let cutoff = if win.cutoff_risk { " ⚠️ CUTOFF RISK" } else { "" };
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
        log::warn!("Scale command issued during safe mode - this may be overridden by emergency brake");
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
            println!(
                "  Agent {}: {} -> {}",
                agent_id, worker.target, count
            );
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
        let status = Command::new(&editor)
            .arg(&config_path)
            .status()?;

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

/// Check if daemon is running (placeholder - would check process or socket)
fn is_daemon_running() -> bool {
    // Check if state file has been updated recently
    let state_path = default_state_path();
    if let Ok(state) = state::load_state(&state_path) {
        let age = (Utc::now() - state.updated_at).num_seconds();
        age < 300 // Updated within last 5 minutes
    } else {
        false
    }
}

/// Check if collector is running (placeholder - would check last fleet aggregate)
fn is_collector_running() -> bool {
    let state_path = default_state_path();
    if let Ok(state) = state::load_state(&state_path) {
        let age = (Utc::now() - state.last_fleet_aggregate.t1).num_seconds();
        age < 300 // Updated within last 5 minutes
    } else {
        false
    }
}

fn run_version_command() -> Result<()> {
    println!("cgov v{}", env!("CARGO_PKG_VERSION"));
    println!("Claude Governor - automated capacity governor for Claude Code");
    println!();
    println!("Component Status:");
    println!("  Daemon:    {}", if is_daemon_running() { "✓ running" } else { "✗ stopped" });
    println!("  Collector: {}", if is_collector_running() { "✓ running" } else { "✗ stopped" });
    println!();
    println!("Build Info:");
    println!("  Target:    {}", option_env!("TARGET").unwrap_or("unknown"));
    println!("  Profile:   {}", option_env!("PROFILE").unwrap_or("unknown"));
    println!("  Rust:      {}", option_env!("RUSTC_VERSION").unwrap_or("unknown"));

    Ok(())
}

fn default_promotions_path() -> PathBuf {
    PathBuf::from("config/promotions.json")
}

fn run_simulate_command(workers: &str, hours: f64, resolution: i64, json: bool) -> Result<()> {
    let state_path = default_state_path();
    let state = state::load_state(&state_path)?;

    // Parse worker schedule
    let mut config = SimConfig::parse_workers(workers)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    config.hours = hours;
    config.resolution_minutes = resolution;

    // Load promotions
    let promo_path = default_promotions_path();
    let promotions = schedule::load_promotions(&promo_path);

    // Run simulation (read-only)
    let trajectory = simulator::simulate(&state, &config, promotions)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

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
            println!("{:<30} {:<30} {:>10} {:>10} {:>10}", "session", "model", "total_usd", "usd/hr", "usd/%7ds");
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

    // Handle --last (default)
    let n = last.unwrap_or(count);
    let results = db::query_last_windows(&conn, n)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        for r in &results {
            let win = r.get("win").and_then(|v| v.as_str()).unwrap_or("?");
            let snap = r.get("snap").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let remain = r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let exh = r.get("exh_hrs").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let cutoff = r.get("cutoff_risk").and_then(|v| v.as_u64()).unwrap_or(0);
            let bind = r.get("bind").and_then(|v| v.as_u64()).unwrap_or(0);
            let safe_w = r.get("safe_w").and_then(|v| v.as_u64());
            let binding = if bind == 1 { " [BINDING]" } else { "" };
            let cutoff_str = if cutoff == 1 { " CUTOFF_RISK" } else { "" };
            let safe_str = match safe_w {
                Some(w) => format!(" safe_w={}", w),
                None => String::new(),
            };
            println!(
                "{}: snap={:.1}% remain={:.1}% exh={:.1}h{}{}{}",
                win, snap, remain, exh, binding, cutoff_str, safe_str,
            );
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
    env_logger::Builder::new()
        .filter_level(log_level)
        .init();

    match cli.command {
        Commands::Poll { format, fail_on_stale } => {
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
        Commands::Status { json: _, summary } => {
            let state_path = default_state_path();
            let state = state::load_state(&state_path)?;
            let output = generate_capacity_summary(&state);
            if summary {
                print!("{}", output);
            } else {
                println!("{}", output);
            }
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

    let state_path = default_state_path();

    governor::run_daemon(
        &state_path,
        dry_run,
        loop_interval,
        hysteresis_band,
        daemon.max_scale_up_per_cycle,
        daemon.max_scale_down_per_cycle,
        target_ceiling,
    )
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
