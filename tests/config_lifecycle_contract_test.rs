//! Configuration lifecycle contracts (claudego-fe51ff0f).
//!
//! The live configuration is a user-owned file, while the checked-in YAML is
//! only a seed. These tests exercise the real binary in isolated homes and
//! keep the daemon's startup snapshot observable through its log line.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use claude_governor::config::GovernorConfig;
use tempfile::TempDir;

const SEED_TEMPLATE: &str = include_str!("../config/governor.yaml");

struct Sandbox {
    root: TempDir,
    home: PathBuf,
    xdg_config: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("create lifecycle sandbox");
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

    fn repo_path(&self) -> PathBuf {
        self.root.path().join("config/governor.yaml")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cgov"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"));
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run cgov")
    }
}

fn write_config(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("config parent")).expect("create config parent");
    fs::write(path, contents).expect("write config");
}

fn minimal_config(loop_interval_secs: u64, hysteresis_band: f64, target_ceiling: f64) -> String {
    format!(
        "pricing:\n  models: {{}}\ndaemon:\n  loop_interval_secs: {loop_interval_secs}\n  hysteresis_band: {hysteresis_band}\n  target_ceiling: {target_ceiling}\n"
    )
}

fn successful_output(output: Output, command: &str) -> String {
    assert!(
        output.status.success(),
        "{command} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("cgov stdout is UTF-8")
}

fn assert_config_header(output: Output, expected: &Path) {
    let stdout = successful_output(output, "cgov config");
    let expected_header = format!("Config file: {}", expected.display());
    assert_eq!(
        stdout.lines().next(),
        Some(expected_header.as_str()),
        "cgov config must report the file that supplied the loaded snapshot"
    );
}

#[test]
fn config_paths_precede_home_then_repository_fallback() {
    let sandbox = Sandbox::new();
    let config = |interval| minimal_config(interval, 1.0, 90.0);

    write_config(&sandbox.repo_path(), &config(111));
    write_config(&sandbox.home_path(), &config(222));
    write_config(&sandbox.xdg_path(), &config(333));

    assert_config_header(sandbox.run(&["config"]), &sandbox.xdg_path());

    fs::remove_file(sandbox.xdg_path()).expect("remove XDG config for fallback check");
    assert_config_header(sandbox.run(&["config"]), &sandbox.home_path());

    fs::remove_file(sandbox.home_path()).expect("remove home config for repository fallback");
    assert_config_header(sandbox.run(&["config"]), Path::new("config/governor.yaml"));
}

#[test]
fn first_run_copies_the_seed_template_to_the_highest_precedence_path() {
    let sandbox = Sandbox::new();
    assert!(!sandbox.xdg_path().exists());
    assert!(!sandbox.home_path().exists());

    assert_config_header(sandbox.run(&["config"]), &sandbox.xdg_path());
    assert_eq!(
        fs::read_to_string(sandbox.xdg_path()).expect("first-run config exists"),
        SEED_TEMPLATE,
        "first-run config must be a byte-for-byte copy of the checked-in seed"
    );
}

#[test]
fn init_without_force_preserves_a_user_owned_config() {
    let sandbox = Sandbox::new();
    let live = minimal_config(17, 0.5, 81.0);
    write_config(&sandbox.xdg_path(), &live);

    let output = successful_output(sandbox.run(&["init", "--no-systemd"]), "cgov init");
    assert!(
        output.contains("Config file exists (use --force to overwrite)"),
        "non-destructive init should explain why the live config was skipped:\n{output}"
    );
    assert_eq!(
        fs::read_to_string(sandbox.xdg_path()).expect("live config remains readable"),
        live,
        "init must not overwrite a deliberate live configuration without --force"
    );
}

#[test]
fn init_force_replaces_the_live_config_with_the_current_seed() {
    let sandbox = Sandbox::new();
    write_config(&sandbox.xdg_path(), &minimal_config(19, 0.25, 72.0));

    let output = successful_output(
        sandbox.run(&["init", "--force", "--no-systemd"]),
        "cgov init --force",
    );
    assert!(
        output.contains("Overwrote config file"),
        "forced init should report the replacement:\n{output}"
    );
    assert_eq!(
        fs::read_to_string(sandbox.xdg_path()).expect("forced config exists"),
        SEED_TEMPLATE,
        "--force must replace the live file with the current seed, not merge it"
    );
}

#[test]
fn config_edit_targets_the_precedence_selected_file() {
    let sandbox = Sandbox::new();
    write_config(&sandbox.home_path(), &minimal_config(27, 1.0, 90.0));
    write_config(&sandbox.xdg_path(), &minimal_config(28, 1.0, 90.0));

    let editor = sandbox.root.path().join("fake-editor");
    fs::write(
        &editor,
        b"#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$EDITOR_MARKER\"\n",
    )
    .expect("write fake editor");
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).expect("chmod fake editor");
    let marker = sandbox.root.path().join("editor-argument");

    let output = sandbox
        .command(&["config", "--edit"])
        .env("EDITOR", &editor)
        .env("EDITOR_MARKER", &marker)
        .output()
        .expect("run cgov config --edit");
    assert!(
        output.status.success(),
        "cgov config --edit failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(marker)
            .expect("editor received a path")
            .trim(),
        sandbox.xdg_path().to_string_lossy(),
        "--edit must open the same highest-precedence file cgov loaded"
    );
}

fn read_startup_line(sandbox: &Sandbox, args: &[&str]) -> (Child, String) {
    let mut child = sandbox
        .command(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cgov _act");
    let stderr = child.stderr.take().expect("capture cgov stderr");
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        let read = reader.read_line(&mut line).expect("read cgov startup log");
        if read == 0 {
            let status = child.wait().expect("wait for cgov _act");
            panic!("cgov _act exited before reporting startup: {status}");
        }
        if line.contains("[act] daemon started") {
            return (child, line);
        }
        line.clear();
    }
}

fn stop_child(mut child: Child) {
    child.kill().expect("stop lifecycle daemon");
    child.wait().expect("wait for lifecycle daemon");
}

#[test]
fn file_changes_apply_only_when_the_daemon_is_restarted() {
    let sandbox = Sandbox::new();
    write_config(&sandbox.xdg_path(), &minimal_config(41, 1.25, 73.0));

    let (child, before_edit) = read_startup_line(&sandbox, &["_act", "--dry-run"]);
    assert!(
        before_edit.contains("interval=41s"),
        "old interval missing: {before_edit}"
    );
    assert!(
        before_edit.contains("hysteresis=1.2"),
        "old hysteresis missing: {before_edit}"
    );
    assert!(
        before_edit.contains("ceiling=73%"),
        "old ceiling missing: {before_edit}"
    );

    write_config(&sandbox.xdg_path(), &minimal_config(9, 2.75, 61.0));
    stop_child(child);

    let (child, after_restart) = read_startup_line(&sandbox, &["_act", "--dry-run"]);
    assert!(
        after_restart.contains("interval=9s")
            && after_restart.contains("hysteresis=2.8")
            && after_restart.contains("ceiling=61%"),
        "restart must load the edited file, got: {after_restart}"
    );
    stop_child(child);
}

#[test]
fn cli_overrides_win_over_global_config_but_not_per_window_ceilings() {
    let sandbox = Sandbox::new();
    write_config(
        &sandbox.xdg_path(),
        "pricing:\n  models: {}\ndaemon:\n  loop_interval_secs: 111\n  hysteresis_band: 4.5\n  target_ceiling: 88.0\n  windows:\n    five_hour:\n      target_utilization: 0.82\n    seven_day:\n      target_utilization: 0.0\n",
    );

    let (child, startup) = read_startup_line(
        &sandbox,
        &[
            "_act",
            "--dry-run",
            "--interval",
            "7",
            "--hysteresis",
            "1.25",
            "--ceiling",
            "55",
        ],
    );
    assert!(
        startup.contains("interval=7s"),
        "CLI interval did not win: {startup}"
    );
    assert!(
        startup.contains("hysteresis=1.2"),
        "CLI hysteresis did not win: {startup}"
    );
    assert!(
        startup.contains("ceiling=55%"),
        "CLI ceiling did not win: {startup}"
    );
    stop_child(child);

    let config = GovernorConfig::load_from_path(&sandbox.xdg_path()).expect("load ceiling fixture");
    assert_eq!(
        config
            .daemon
            .get_target_ceiling_for_window_with_fallback("five_hour", 55.0),
        82.0,
        "a positive per-window ceiling must outrank the CLI global fallback"
    );
    assert_eq!(
        config
            .daemon
            .get_target_ceiling_for_window_with_fallback("seven_day", 55.0),
        55.0,
        "a zero window override must inherit the CLI global fallback"
    );
    assert_eq!(
        config
            .daemon
            .get_target_ceiling_for_window_with_fallback("weekly_scoped", 55.0),
        55.0,
        "an absent window override must inherit the CLI global fallback"
    );
}
