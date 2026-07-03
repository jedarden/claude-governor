//! Test fixtures for governor cycle tests
//!
//! Provides minimal, reusable helper functions to create test configurations
//! and state files for governor cycle tests.

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, DaemonConfig,
    GovernorConfig, ModelPricing, PricingConfig, SprintConfig,
};
use claude_governor::state;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Create a minimal test AgentConfig
///
/// Returns an AgentConfig with safe defaults suitable for testing.
///
/// # Arguments
/// * `name` - Agent name (used for session_pattern and defaults)
///
/// # Example
/// ```no_run
/// use claude_governor::fixtures::test_agent_config;
///
/// let config = test_agent_config("test-agent");
/// assert_eq!(config.launch_cmd, "echo test-agent");
/// assert_eq!(config.min_workers, 0);
/// assert_eq!(config.max_workers, 8);
/// ```
pub fn test_agent_config(name: &str) -> AgentConfig {
    AgentConfig {
        launch_cmd: format!("echo {}", name),
        session_pattern: format!("{}-*", name),
        heartbeat_dir: format!("/tmp/test-heartbeats/{}", name),
        min_workers: 0,
        max_workers: 8,
        subscription: false,
    }
}

/// Create a minimal test AgentConfig with custom parameters
///
/// Allows overriding specific fields while providing sensible defaults for others.
///
/// # Arguments
/// * `name` - Agent name
/// * `min_workers` - Minimum worker count
/// * `max_workers` - Maximum worker count
///
/// # Example
/// ```no_run
/// use claude_governor::fixtures::test_agent_config_with_bounds;
///
/// let config = test_agent_config_with_bounds("custom-agent", 2, 10);
/// assert_eq!(config.min_workers, 2);
/// assert_eq!(config.max_workers, 10);
/// ```
pub fn test_agent_config_with_bounds(name: &str, min_workers: u32, max_workers: u32) -> AgentConfig {
    AgentConfig {
        launch_cmd: format!("echo {}", name),
        session_pattern: format!("{}-*", name),
        heartbeat_dir: format!("/tmp/test-heartbeats/{}", name),
        min_workers,
        max_workers,
        subscription: false,
    }
}

/// Create a minimal test AlertConfig with safe defaults
///
/// Returns an AlertConfig suitable for testing with:
/// - Alerts enabled
/// - 60-minute cooldown
/// - Warning-level severity threshold
/// - Cache efficiency threshold at 30%
/// - Auto-bead disabled (alerts only)
///
/// # Example
/// ```no_run
/// use claude_governor::fixtures::test_alert_config;
///
/// let config = test_alert_config();
/// assert!(config.enabled);
/// assert_eq!(config.cooldown_minutes, 60);
/// assert!(!config.auto_bead);
/// ```
pub fn test_alert_config() -> AlertConfig {
    AlertConfig::default()
}

/// Create a minimal test AlertConfig with custom cooldown
///
/// # Arguments
/// * `cooldown_minutes` - Cooldown period between repeated alerts
///
/// # Example
/// ```no_run
/// use claude_governor::fixtures::test_alert_config_with_cooldown;
///
/// let config = test_alert_config_with_cooldown(30);
/// assert_eq!(config.cooldown_minutes, 30);
/// ```
pub fn test_alert_config_with_cooldown(cooldown_minutes: i64) -> AlertConfig {
    AlertConfig {
        cooldown_minutes,
        ..Default::default()
    }
}

/// Create a minimal test GovernorConfig
///
/// Returns a GovernorConfig with minimal pricing data and safe defaults.
/// Suitable for basic governor cycle tests without requiring a full config file.
///
/// # Example
/// ```no_run
/// use claude_governor::fixtures::test_governor_config;
///
/// let config = test_governor_config();
/// assert_eq!(config.daemon.loop_interval_secs, 300);
/// assert_eq!(config.daemon.target_ceiling, 90.0);
/// assert!(config.alerts.enabled);
/// ```
pub fn test_governor_config() -> GovernorConfig {
    let mut models = HashMap::new();

    // Add minimal pricing for a common test model
    models.insert(
        "claude-sonnet-4-20250514".to_string(),
        ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_5m_per_mtok: 3.75,
            cache_write_1h_per_mtok: 6.0,
            cache_read_per_mtok: 0.30,
        },
    );

    GovernorConfig {
        pricing: PricingConfig { models },
        sprint: SprintConfig::default(),
        daemon: DaemonConfig::default(),
        alerts: AlertConfig::default(),
        composite_risk: CompositeRiskConfig::default(),
        cone_scaling: ConeScalingConfig::default(),
        agents: HashMap::new(),
        credentials_path: None,
    }
}

/// Create a minimal GovernorConfig with custom daemon settings
///
/// # Arguments
/// * `loop_interval_secs` - Governor polling interval
/// * `target_ceiling` - Target utilization ceiling percentage
///
/// # Example
/// ```no_run
/// use claude_governor::fixtures::test_governor_config_with_daemon;
///
/// let config = test_governor_config_with_daemon(120, 85.0);
/// assert_eq!(config.daemon.loop_interval_secs, 120);
/// assert_eq!(config.daemon.target_ceiling, 85.0);
/// ```
pub fn test_governor_config_with_daemon(
    loop_interval_secs: u64,
    target_ceiling: f64,
) -> GovernorConfig {
    let mut config = test_governor_config();
    config.daemon.loop_interval_secs = loop_interval_secs;
    config.daemon.target_ceiling = target_ceiling;
    config
}

/// Create a minimal GovernorConfig with test agents
///
/// # Arguments
/// * `agent_names` - List of agent names to create configs for
///
/// # Example
/// ```no_run
/// use claude_governor::fixtures::test_governor_config_with_agents;
///
/// let config = test_governor_config_with_agents(&["agent-1", "agent-2"]);
/// assert_eq!(config.agents.len(), 2);
/// assert!(config.agents.contains_key("agent-1"));
/// ```
pub fn test_governor_config_with_agents(agent_names: &[&str]) -> GovernorConfig {
    let mut config = test_governor_config();

    for name in agent_names {
        config
            .agents
            .insert(name.to_string(), test_agent_config(name));
    }

    config
}

/// Create a minimal state file in a temporary directory
///
/// Creates an empty governor-state.json file in the specified temp directory.
/// Returns the path to the created file for loading into tests.
///
/// # Arguments
/// * `temp_dir` - Path to temporary directory (e.g., from tempfile::TempDir)
///
/// # Returns
/// Path to the created state file
///
/// # Example
/// ```no_run
/// use tempfile::TempDir;
/// use claude_governor::fixtures::create_minimal_state_file;
///
/// let temp_dir = TempDir::new().unwrap();
/// let state_path = create_minimal_state_file(temp_dir.path());
///
/// // Load the state for testing
/// let state = claude_governor::state::load_state(&state_path).unwrap();
/// assert_eq!(state.usage.sonnet_pct, 0.0);
/// ```
pub fn create_minimal_state_file(temp_dir: &Path) -> std::path::PathBuf {
    let state_path = temp_dir.join("governor-state.json");

    // Write an empty state file (will load as defaults)
    let empty_state = state::GovernorState::new();
    let json = serde_json::to_string_pretty(&empty_state).unwrap();
    fs::write(&state_path, json).expect("Failed to write minimal state file");

    state_path
}

/// Create a state file with custom worker configuration
///
/// Creates a governor-state.json file with specified worker configurations.
/// Useful for testing scaling decisions with known worker states.
///
/// # Arguments
/// * `temp_dir` - Path to temporary directory
/// * `workers` - HashMap of agent names to (current, min, max) worker counts
///
/// # Returns
/// Path to the created state file
///
/// # Example
/// ```no_run
/// use std::collections::HashMap;
/// use tempfile::TempDir;
/// use claude_governor::fixtures::create_state_file_with_workers;
///
/// let temp_dir = TempDir::new().unwrap();
/// let mut workers = HashMap::new();
/// workers.insert("agent-1".to_string(), (5, 1, 10));
/// workers.insert("agent-2".to_string(), (3, 0, 8));
///
/// let state_path = create_state_file_with_workers(temp_dir.path(), &workers);
/// let state = claude_governor::state::load_state(&state_path).unwrap();
///
/// assert_eq!(state.workers["agent-1"].current, 5);
/// assert_eq!(state.workers["agent-2"].current, 3);
/// ```
pub fn create_state_file_with_workers(
    temp_dir: &Path,
    workers: &HashMap<String, (u32, u32, u32)>,
) -> std::path::PathBuf {
    let state_path = temp_dir.join("governor-state.json");

    let mut state = state::GovernorState::new();

    // Populate worker states
    for (agent_name, &(current, min, max)) in workers {
        state.workers.insert(
            agent_name.clone(),
            state::WorkerState {
                current,
                target: current,
                min,
                max,
            },
        );
    }

    let json = serde_json::to_string_pretty(&state).unwrap();
    fs::write(&state_path, json).expect("Failed to write state file with workers");

    state_path
}

/// Create a state file with utilization data for capacity forecast testing
///
/// Creates a governor-state.json file with current utilization percentages
/// for all three windows. Useful for testing capacity forecasts and scaling
/// decisions under specific utilization conditions.
///
/// # Arguments
/// * `temp_dir` - Path to temporary directory
/// * `five_hour_pct` - 5-hour window utilization percentage
/// * `seven_day_pct` - 7-day window utilization percentage
/// * `seven_day_sonnet_pct` - 7-day Sonnet window utilization percentage
///
/// # Returns
/// Path to the created state file
///
/// # Example
/// ```no_run
/// use tempfile::TempDir;
/// use claude_governor::fixtures::create_state_file_with_utilization;
///
/// let temp_dir = TempDir::new().unwrap();
/// let state_path = create_state_file_with_utilization(
///     temp_dir.path(),
///     50.0,  // 5-hour at 50%
///     40.0,  // 7-day at 40%
///     35.0,  // 7-day Sonnet at 35%
/// );
///
/// let state = claude_governor::state::load_state(&state_path).unwrap();
/// // Test capacity forecast and scaling with this utilization data
/// ```
pub fn create_state_file_with_utilization(
    temp_dir: &Path,
    five_hour_pct: f64,
    seven_day_pct: f64,
    seven_day_sonnet_pct: f64,
) -> std::path::PathBuf {
    let state_path = temp_dir.join("governor-state.json");

    let mut state = state::GovernorState::new();
    state.usage.five_hour_pct = five_hour_pct;
    state.usage.sonnet_pct = seven_day_sonnet_pct;
    state.usage.all_models_pct = seven_day_pct;

    let json = serde_json::to_string_pretty(&state).unwrap();
    fs::write(&state_path, json).expect("Failed to write state file with utilization");

    state_path
}

/// Create a fully-populated state file for comprehensive testing
///
/// Creates a governor-state.json file with workers, utilization, and capacity
/// forecast data. Suitable for end-to-end governor cycle tests.
///
/// # Arguments
/// * `temp_dir` - Path to temporary directory
/// * `workers` - HashMap of agent names to (current, min, max) worker counts
/// * `five_hour_pct` - 5-hour window utilization percentage
/// * `seven_day_pct` - 7-day window utilization percentage
/// * `seven_day_sonnet_pct` - 7-day Sonnet window utilization percentage
///
/// # Returns
/// Path to the created state file
///
/// # Example
/// ```no_run
/// use std::collections::HashMap;
/// use tempfile::TempDir;
/// use claude_governor::fixtures::create_full_state_file;
///
/// let temp_dir = TempDir::new().unwrap();
/// let mut workers = HashMap::new();
/// workers.insert("test-agent".to_string(), (5, 1, 10));
///
/// let state_path = create_full_state_file(
///     temp_dir.path(),
///     &workers,
///     50.0,  // moderate utilization
///     40.0,
///     35.0,
/// );
///
/// let state = claude_governor::state::load_state(&state_path).unwrap();
/// // State is fully populated for a complete governor cycle test
/// ```
pub fn create_full_state_file(
    temp_dir: &Path,
    workers: &HashMap<String, (u32, u32, u32)>,
    five_hour_pct: f64,
    seven_day_pct: f64,
    seven_day_sonnet_pct: f64,
) -> std::path::PathBuf {
    let state_path = temp_dir.join("governor-state.json");

    let mut state = state::GovernorState::new();

    // Populate worker states
    for (agent_name, &(current, min, max)) in workers {
        state.workers.insert(
            agent_name.clone(),
            state::WorkerState {
                current,
                target: current,
                min,
                max,
            },
        );
    }

    // Set utilization
    state.usage.five_hour_pct = five_hour_pct;
    state.usage.sonnet_pct = seven_day_sonnet_pct;
    state.usage.all_models_pct = seven_day_pct;

    // Set up capacity forecast with safe worker counts
    state.capacity_forecast = state::CapacityForecast {
        five_hour: state::WindowForecast {
            current_utilization: five_hour_pct,
            safe_worker_count: Some(5),
            safe_worker_count_p75: Some(4),
            ..Default::default()
        },
        seven_day: state::WindowForecast {
            current_utilization: seven_day_pct,
            safe_worker_count: Some(6),
            safe_worker_count_p75: Some(5),
            ..Default::default()
        },
        seven_day_sonnet: state::WindowForecast {
            current_utilization: seven_day_sonnet_pct,
            safe_worker_count: Some(7),
            safe_worker_count_p75: Some(6),
            ..Default::default()
        },
        binding_window: "seven_day_sonnet".to_string(),
        ..Default::default()
    };

    let json = serde_json::to_string_pretty(&state).unwrap();
    fs::write(&state_path, json).expect("Failed to write full state file");

    state_path
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_test_agent_config() {
        let config = test_agent_config("my-agent");
        assert_eq!(config.launch_cmd, "echo my-agent");
        assert_eq!(config.session_pattern, "my-agent-*");
        assert_eq!(config.heartbeat_dir, "/tmp/test-heartbeats/my-agent");
        assert_eq!(config.min_workers, 0);
        assert_eq!(config.max_workers, 8);
        assert_eq!(config.subscription, false);
    }

    #[test]
    fn test_test_agent_config_with_bounds() {
        let config = test_agent_config_with_bounds("custom", 2, 15);
        assert_eq!(config.min_workers, 2);
        assert_eq!(config.max_workers, 15);
    }

    #[test]
    fn test_test_alert_config() {
        let config = test_alert_config();
        assert!(config.enabled);
        assert_eq!(config.cooldown_minutes, 60);
        assert_eq!(config.min_severity, "warning");
        assert_eq!(config.low_cache_eff_threshold, 0.30);
        assert_eq!(config.low_cache_eff_intervals, 5);
        assert!(!config.auto_bead);
    }

    #[test]
    fn test_test_alert_config_with_cooldown() {
        let config = test_alert_config_with_cooldown(30);
        assert_eq!(config.cooldown_minutes, 30);
        assert!(config.enabled); // other defaults preserved
    }

    #[test]
    fn test_test_governor_config() {
        let config = test_governor_config();
        assert_eq!(config.daemon.loop_interval_secs, 300);
        assert_eq!(config.daemon.target_ceiling, 90.0);
        assert!(config.alerts.enabled);
        assert!(!config.composite_risk.enabled);
        assert_eq!(config.cone_scaling.narrow_threshold, 1.5);
        assert!(config.pricing.models.contains_key("claude-sonnet-4-20250514"));
    }

    #[test]
    fn test_test_governor_config_with_daemon() {
        let config = test_governor_config_with_daemon(120, 85.0);
        assert_eq!(config.daemon.loop_interval_secs, 120);
        assert_eq!(config.daemon.target_ceiling, 85.0);
    }

    #[test]
    fn test_test_governor_config_with_agents() {
        let config = test_governor_config_with_agents(&["agent-1", "agent-2"]);
        assert_eq!(config.agents.len(), 2);
        assert!(config.agents.contains_key("agent-1"));
        assert!(config.agents.contains_key("agent-2"));
        assert_eq!(config.agents["agent-1"].launch_cmd, "echo agent-1");
    }

    #[test]
    fn test_create_minimal_state_file() {
        let temp_dir = TempDir::new().unwrap();
        let state_path = create_minimal_state_file(temp_dir.path());

        assert!(state_path.exists());
        let loaded = state::load_state(&state_path).unwrap();
        assert_eq!(loaded.usage.sonnet_pct, 0.0);
        assert!(loaded.workers.is_empty());
    }

    #[test]
    fn test_create_state_file_with_workers() {
        let temp_dir = TempDir::new().unwrap();
        let mut workers = HashMap::new();
        workers.insert("agent-1".to_string(), (5, 1, 10));
        workers.insert("agent-2".to_string(), (3, 0, 8));

        let state_path = create_state_file_with_workers(temp_dir.path(), &workers);

        assert!(state_path.exists());
        let loaded = state::load_state(&state_path).unwrap();
        assert_eq!(loaded.workers.len(), 2);
        assert_eq!(loaded.workers["agent-1"].current, 5);
        assert_eq!(loaded.workers["agent-1"].min, 1);
        assert_eq!(loaded.workers["agent-1"].max, 10);
        assert_eq!(loaded.workers["agent-2"].current, 3);
        assert_eq!(loaded.workers["agent-2"].min, 0);
        assert_eq!(loaded.workers["agent-2"].max, 8);
    }

    #[test]
    fn test_create_state_file_with_utilization() {
        let temp_dir = TempDir::new().unwrap();
        let state_path = create_state_file_with_utilization(temp_dir.path(), 50.0, 40.0, 35.0);

        assert!(state_path.exists());
        let loaded = state::load_state(&state_path).unwrap();
        assert_eq!(loaded.usage.five_hour_pct, 50.0);
        assert_eq!(loaded.usage.all_models_pct, 40.0);
        assert_eq!(loaded.usage.sonnet_pct, 35.0);
    }

    #[test]
    fn test_create_full_state_file() {
        let temp_dir = TempDir::new().unwrap();
        let mut workers = HashMap::new();
        workers.insert("test-agent".to_string(), (5, 1, 10));

        let state_path = create_full_state_file(
            temp_dir.path(),
            &workers,
            50.0,
            40.0,
            35.0,
        );

        assert!(state_path.exists());
        let loaded = state::load_state(&state_path).unwrap();

        // Workers
        assert_eq!(loaded.workers["test-agent"].current, 5);

        // Utilization
        assert_eq!(loaded.usage.five_hour_pct, 50.0);
        assert_eq!(loaded.usage.all_models_pct, 40.0);
        assert_eq!(loaded.usage.sonnet_pct, 35.0);

        // Capacity forecast
        assert_eq!(
            loaded.capacity_forecast.five_hour.current_utilization,
            50.0
        );
        assert_eq!(
            loaded.capacity_forecast.seven_day.current_utilization,
            40.0
        );
        assert_eq!(
            loaded.capacity_forecast.seven_day_sonnet.current_utilization,
            35.0
        );
        assert_eq!(loaded.capacity_forecast.binding_window, "seven_day_sonnet");
    }

    #[test]
    fn test_fixtures_are_reusable() {
        // Create multiple fixtures in sequence to ensure they don't share state
        let config1 = test_agent_config("agent-1");
        let config2 = test_agent_config("agent-2");
        assert_ne!(config1.session_pattern, config2.session_pattern);

        let alert1 = test_alert_config();
        let alert2 = test_alert_config();
        assert_eq!(alert1.cooldown_minutes, alert2.cooldown_minutes);

        let gov1 = test_governor_config();
        let gov2 = test_governor_config();
        assert_eq!(gov1.daemon.target_ceiling, gov2.daemon.target_ceiling);
    }
}
