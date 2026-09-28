//! claudego-62bd7180: pin the `cgov config` render contract.
//!
//! CLAUDE.md §1 and docs/hysteresis-and-smooth-scaling.md define the
//! live-vs-template rule: `config/governor.yaml` is only the seed template,
//! the live machine file is the running configuration, and `cgov config` is
//! the authoritative view — it must print the file it actually loaded plus
//! every daemon key (claudego-3648483e recorded the operator confusion this
//! prevents). The `cgov status` render path got a pinning suite
//! (claudego-f1e38b15, persisted worker counts); nothing pinned this one.
//!
//! These tests render the committed fixture through the exact production
//! function `run_config_command` prints —
//! `GovernorConfig::render_config_display` — and pin three things:
//!
//! 1. the first line names the loaded fixture path, never a hard-coded or
//!    seed-template path;
//! 2. the rendered `daemon` section carries every documented daemon key with
//!    the fixture's distinctive value (a render that silently substitutes a
//!    default fails);
//! 3. the rendered key set matches the pinned documented set exactly, and
//!    every field declared on `DaemonConfig` in `src/config.rs` appears in
//!    the render — so a new daemon key added without being rendered (a
//!    `#[serde(skip_serializing)]` field, say) fails here, and one added
//!    *and* rendered forces `PINNED_DAEMON_KEYS` and the docs to be extended
//!    deliberately.

use claude_governor::config::GovernorConfig;
use serde_yaml::Value;

const FIXTURE_YAML: &str = include_str!("fixtures/config/governor_config_render.yaml");
const FIXTURE_FILE_NAME: &str = "governor-render-fixture.yaml";

/// The documented daemon key set: CLAUDE.md §1,
/// docs/hysteresis-and-smooth-scaling.md, and the seed template's `daemon:`
/// block all present this same surface. Pinned in full — a key may only be
/// added or removed here together with `DaemonConfig`, the template, and the
/// docs.
const PINNED_DAEMON_KEYS: [&str; 14] = [
    "loop_interval_secs",
    "adaptive_act_interval",
    "hysteresis_band",
    "max_scale_up_per_cycle",
    "max_scale_down_per_cycle",
    "progressive_scaling",
    "exponential_decay_scaling",
    "min_scale_interval_secs",
    "target_ceiling",
    "mode",
    "pre_scale_minutes",
    "log_max_bytes",
    "log_backup_count",
    "windows",
];

/// Distinctive fixture values the render must echo back verbatim. Checked as
/// a table so a key can neither disappear from the render nor silently fall
/// back to its default: every documented key is both present (key-set pins)
/// and equal to the value the fixture loaded.
fn assert_fixture_value(daemon: &Value, key: &str, expected: &Value) {
    let actual = daemon
        .get(key)
        .unwrap_or_else(|| panic!("daemon key `{key}` missing from the cgov config render"));
    assert_eq!(
        actual,
        expected,
        "daemon key `{key}` did not survive the render with the loaded fixture value"
    );
}

/// Write the fixture to disk and load it through the production loader —
/// the same `GovernorConfig::load_from_path` call `run_config_command` makes
/// before rendering.
fn loaded_fixture_config() -> (tempfile::TempDir, std::path::PathBuf, GovernorConfig) {
    let dir = tempfile::tempdir().expect("tempdir for config fixture");
    let path = dir.path().join(FIXTURE_FILE_NAME);
    std::fs::write(&path, FIXTURE_YAML).expect("write config fixture");
    let config = GovernorConfig::load_from_path(&path).expect("fixture loads through production path");
    (dir, path, config)
}

/// The rendered `daemon:` section of a `cgov config` display, as YAML.
fn rendered_daemon_section(display: &str) -> Value {
    let doc: Value =
        serde_yaml::from_str(display).expect("cgov config display must be valid YAML");
    doc.get("daemon")
        .cloned()
        .expect("cgov config display must carry the daemon section")
}

/// Extract the field names declared on `pub struct DaemonConfig` straight
/// from the committed source. Rust cannot introspect serialization, so the
/// declaration is the independent witness: a field added to the struct —
/// including one marked `#[serde(skip_serializing)]`, which the rendered
/// key set alone could never catch — appears here and must therefore show
/// up in the render. A refactor that hides the declaration from this parser
/// fails loudly (zero fields found), not silently.
fn daemon_config_source_fields() -> Vec<String> {
    let src = include_str!("../src/config.rs");
    let marker = "pub struct DaemonConfig {";
    let start = src
        .find(marker)
        .expect("pub struct DaemonConfig must remain findable in src/config.rs");
    let body_start = start + marker.len();
    let body_end = src[body_start..]
        .find("\n}")
        .expect("DaemonConfig must close its brace at column 0")
        + body_start;

    let mut fields = Vec::new();
    for line in src[body_start..body_end].lines() {
        let Some(rest) = line.trim_start().strip_prefix("pub ") else {
            continue;
        };
        let Some(colon) = rest.find(':') else {
            continue;
        };
        let name = rest[..colon].trim();
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            fields.push(name.to_string());
        }
    }
    fields
}

/// The span of `run_config_command` inside the committed src/main.rs, used by
/// the static routing check below.
fn run_config_command_source() -> &'static str {
    let src = include_str!("../src/main.rs");
    let start = src
        .find("fn run_config_command")
        .expect("fn run_config_command must remain in src/main.rs");
    let end = src[start..]
        .find("\nfn ")
        .map(|i| start + i)
        .unwrap_or(src.len());
    &src[start..end]
}

/// Pin 1: the display's first line names the file that was loaded — the
/// fixture path itself, not a hard-coded location and not the seed template.
#[test]
fn render_names_the_loaded_file_not_a_hardcoded_or_template_path() {
    let (_dir, path, config) = loaded_fixture_config();
    let display = GovernorConfig::render_config_display(&path, &config)
        .expect("render succeeds for a valid config");

    let expected_header = format!("Config file: {}", path.display());
    let first_line = display.lines().next().expect("display has a header line");
    assert_eq!(
        first_line, expected_header,
        "cgov config must name the file it loaded, exactly"
    );

    assert!(
        !display.contains("config/governor.yaml"),
        "cgov config must never name the seed template — the loaded path is the contract (CLAUDE.md §1)"
    );
}

/// Pin 2: every documented daemon key is rendered, and each carries the
/// fixture's distinctive value — not a default substituted on the way out.
#[test]
fn render_carries_every_documented_daemon_key_with_its_loaded_value() {
    let (_dir, path, config) = loaded_fixture_config();
    let display = GovernorConfig::render_config_display(&path, &config).unwrap();
    let daemon = rendered_daemon_section(&display);

    assert_fixture_value(&daemon, "loop_interval_secs", &Value::from(271));
    assert_fixture_value(&daemon, "adaptive_act_interval", &Value::from(true));
    assert_fixture_value(&daemon, "hysteresis_band", &Value::from(1.75));
    assert_fixture_value(&daemon, "max_scale_up_per_cycle", &Value::from(4));
    assert_fixture_value(&daemon, "max_scale_down_per_cycle", &Value::from(2));
    assert_fixture_value(&daemon, "progressive_scaling", &Value::from(true));
    assert_fixture_value(&daemon, "exponential_decay_scaling", &Value::from(true));
    assert_fixture_value(&daemon, "min_scale_interval_secs", &Value::from(97));
    assert_fixture_value(&daemon, "target_ceiling", &Value::from(82.5));
    assert_fixture_value(&daemon, "mode", &Value::from("tmux"));
    assert_fixture_value(&daemon, "pre_scale_minutes", &Value::from(44));
    assert_fixture_value(&daemon, "log_max_bytes", &Value::from(12_345_678));
    assert_fixture_value(&daemon, "log_backup_count", &Value::from(6));

    let windows = daemon
        .get("windows")
        .expect("daemon.windows is a documented key and must render");
    assert_eq!(
        windows.get("five_hour").and_then(|w| w.get("target_utilization")),
        Some(&Value::from(0.71)),
        "a configured window override must survive the render"
    );
}

/// Pin 3 (both directions): the rendered daemon key set equals the pinned
/// documented set exactly. A key added to `DaemonConfig` shows up in the
/// render and fails here until the documented contract — this list, the
/// seed template, and the docs — is extended deliberately; a key removed or
/// renamed fails the other way.
#[test]
fn rendered_daemon_key_set_matches_the_documented_contract_exactly() {
    let (_dir, path, config) = loaded_fixture_config();
    let display = GovernorConfig::render_config_display(&path, &config).unwrap();
    let daemon = rendered_daemon_section(&display);

    let mut rendered: Vec<String> = daemon
        .as_mapping()
        .expect("daemon section renders as a mapping")
        .keys()
        .map(|k| k.as_str().expect("daemon keys render as strings").to_string())
        .collect();
    rendered.sort();

    let mut pinned: Vec<&str> = PINNED_DAEMON_KEYS.to_vec();
    pinned.sort();

    assert_eq!(
        rendered, pinned,
        "cgov config's daemon surface drifted from the documented key set — update PINNED_DAEMON_KEYS together with DaemonConfig, config/governor.yaml, and docs/hysteresis-and-smooth-scaling.md"
    );
}

/// Pin 4: every field declared on `DaemonConfig` in src/config.rs appears in
/// the render. Serialization alone cannot witness a `#[serde(skip_serializing)]`
/// field — the source declaration can. This is the "a new daemon key was
/// added to GovernorConfig without being rendered" tripwire.
#[test]
fn every_daemon_config_field_declared_in_source_is_rendered() {
    let (_dir, path, config) = loaded_fixture_config();
    let display = GovernorConfig::render_config_display(&path, &config).unwrap();
    let daemon = rendered_daemon_section(&display);

    let fields = daemon_config_source_fields();
    assert!(
        fields.len() >= PINNED_DAEMON_KEYS.len(),
        "source parse found {} DaemonConfig fields but {} keys are pinned — the field parser is stale, fix it before trusting this suite",
        fields.len(),
        PINNED_DAEMON_KEYS.len()
    );

    for field in &fields {
        assert!(
            daemon.get(field).is_some(),
            "DaemonConfig field `{field}` is declared in src/config.rs but is not rendered by `cgov config` — an operator must be able to see every daemon key they can set (claudego-62bd7180)"
        );
    }
}

/// Pin 5: `run_config_command` prints the pinned render — it routes through
/// `render_config_display` and does not re-inline its own serialization that
/// the contract tests could never see.
#[test]
fn run_config_command_prints_the_pinned_render() {
    let body = run_config_command_source();
    assert!(
        body.contains("render_config_display"),
        "run_config_command must print GovernorConfig::render_config_display — the pinned render is the contract surface (claudego-62bd7180)"
    );
    assert!(
        !body.contains("serde_yaml::to_string"),
        "run_config_command must not re-inline its own config serialization; the render lives in the library where the contract test drives it"
    );
}
