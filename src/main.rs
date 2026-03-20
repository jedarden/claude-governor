//! Claude Governor - Automated capacity governor for Claude Code subscription usage
//!
//! Usage Poller: Phase 1 foundation
//! - Reads OAuth credentials from ~/.claude/.credentials.json
//! - Refreshes tokens when near expiry
//! - Polls usage data from the Anthropic API
//! - Outputs usage data in human or machine-readable formats

use anyhow::Result;
use clap::{Parser, Subcommand};
use log::LevelFilter;

use claude_governor::poller::{Poller, UsageData};

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
