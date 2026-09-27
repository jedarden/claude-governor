//! Configuration path and seed-template contracts (claudego-c8da1ee9).
//!
//! The checked-in `config/governor.yaml` is a seed, not the live machine
//! configuration. These tests run the real `cgov config` binary in isolated
//! XDG/HOME directories and pin the resolution order, first-startup seeding,
//! loaded-path reporting, and preservation of deliberate live differences.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_yaml::Value;
use tempfile::TempDir;

const SEED_TEMPLATE: &str = include_str!("../config/governor.yaml");
const LIVE_CONFIG: &str = r#"
pricing:
  models: {}
agents:
  live-pool:
    launch_cmd: "echo live"
    session_pattern: "live-*"
    heartbeat_dir: "/tmp/live-heartbeats"
daemon:
  loop_interval_secs: 17
  hysteresis_band: 0.5
  mode: tmux
"#;

struct Sandbox {
    root: TempDir,
    home: PathBuf,
    xdg_config: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("create config sandbox");
        let home = root.path().join("home");
        let xdg_config = root.path().join("xdg-config");
        fs::create_dir_all(&home).expect("create sandbox home");
        fs::create_dir_all(&xdg_config).expect("create sandbox XDG config");
        Self {
            root,
            home,
            xdg_config,
        }
    }

    fn xdg_path(&self) -> PathBuf {
        self.xdg_config
            .join("claude-governor")
            .join("governor.yaml")
    }

    fn home_path(&self) -> PathBuf {
        self.home
            .join(".config")
            .join("claude-governor")
            .join("governor.yaml")
    }

    fn write_xdg(&self, contents: &str) {
        write_config(&self.xdg_path(), contents);
    }

    fn write_home(&self, contents: &str) {
        write_config(&self.home_path(), contents);
    }

    fn run_config(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cgov"))
            .args(["config"])
            .current_dir(self.root.path())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .output()
            .expect("run cgov config")
    }
}

fn write_config(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("config parent")).expect("create config parent");
    fs::write(path, contents).expect("write config");
}

fn config_body(output: &Output) -> (&str, Value) {
    assert!(
        output.status.success(),
        "cgov config failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("cgov output is UTF-8");
    let (header, body) = stdout
        .trim_end()
        .split_once("\n\n")
        .expect("cgov config has a path header and YAML body");
    let yaml = serde_yaml::from_str(body).expect("cgov config body is YAML");
    (header, yaml)
}

fn daemon_value<'a>(config: &'a Value, key: &str) -> &'a Value {
    config
        .get("daemon")
        .and_then(|daemon| daemon.get(key))
        .unwrap_or_else(|| panic!("rendered config is missing daemon.{key}"))
}

#[test]
fn xdg_config_precedes_home_config() {
    let sandbox = Sandbox::new();
    sandbox.write_home(
        r#"
pricing:
  models: {}
daemon:
  loop_interval_secs: 111
"#,
    );
    sandbox.write_xdg(
        r#"
pricing:
  models: {}
daemon:
  loop_interval_secs: 222
"#,
    );

    let output = sandbox.run_config();
    let (header, config) = config_body(&output);
    assert_eq!(
        header,
        format!("Config file: {}", sandbox.xdg_path().display())
    );
    assert_eq!(
        daemon_value(&config, "loop_interval_secs"),
        &Value::from(222)
    );
    assert_ne!(
        daemon_value(&config, "loop_interval_secs"),
        &Value::from(111)
    );
}

#[test]
fn home_config_is_used_when_xdg_config_is_missing() {
    let sandbox = Sandbox::new();
    sandbox.write_home(
        r#"
pricing:
  models: {}
daemon:
  loop_interval_secs: 333
"#,
    );

    let output = sandbox.run_config();
    let (header, config) = config_body(&output);
    assert_eq!(
        header,
        format!("Config file: {}", sandbox.home_path().display())
    );
    assert_eq!(
        daemon_value(&config, "loop_interval_secs"),
        &Value::from(333)
    );
}

#[test]
fn missing_live_config_is_created_byte_for_byte_from_seed_template() {
    let sandbox = Sandbox::new();
    assert!(!sandbox.xdg_path().exists());
    assert!(!sandbox.home_path().exists());

    let output = sandbox.run_config();
    let (header, _config) = config_body(&output);
    assert_eq!(
        header,
        format!("Config file: {}", sandbox.xdg_path().display())
    );
    assert_eq!(
        fs::read_to_string(sandbox.xdg_path()).expect("seeded config exists"),
        SEED_TEMPLATE,
        "first-startup config must be copied from the checked-in seed"
    );
}

#[test]
fn cgov_config_reports_loaded_path_and_preserves_live_seed_differences() {
    let sandbox = Sandbox::new();
    sandbox.write_xdg(LIVE_CONFIG);

    assert_ne!(
        LIVE_CONFIG, SEED_TEMPLATE,
        "the fixture must model intentional live configuration drift from the seed"
    );

    let output = sandbox.run_config();
    let (header, config) = config_body(&output);
    assert_eq!(
        header,
        format!("Config file: {}", sandbox.xdg_path().display())
    );
    assert_eq!(
        daemon_value(&config, "loop_interval_secs"),
        &Value::from(17)
    );
    assert_eq!(daemon_value(&config, "hysteresis_band"), &Value::from(0.5));
    assert_eq!(daemon_value(&config, "mode"), &Value::from("tmux"));

    let rendered_agent = config
        .get("agents")
        .and_then(|agents| agents.get("live-pool"))
        .expect("live-only agent survives cgov config render");
    assert_eq!(
        rendered_agent.get("heartbeat_dir"),
        Some(&Value::from("/tmp/live-heartbeats"))
    );
    assert_eq!(
        fs::read_to_string(sandbox.xdg_path()).expect("live config remains readable"),
        LIVE_CONFIG,
        "loading and rendering must not replace intentional live configuration"
    );
}
