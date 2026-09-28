//! claudego-5fd20c2f: the configuration documentation contract.
//!
//! docs/configuration-reference.md documents the whole `governor.yaml`
//! schema; this test keeps that claim true mechanically. serde drops keys it
//! does not recognise, so "undocumented" and "silently ignored" are the same
//! failure mode read from two sides: a key the binary supports but the doc
//! omits, a key the doc lists but the binary drops, or a key in a config
//! file that quietly does nothing.
//!
//! The supported key set is **derived from the binary**, never hardcoded
//! here: a maximal config (every schema key set to a distinctive value) is
//! round-tripped through `GovernorConfig` (deserialize → serialize). The
//! output carries every struct field with its resolved value, so its key
//! paths are exactly what the binary supports, and `output − input` is
//! exactly what was silently ignored. Four set comparisons then close every
//! direction:
//!
//! 1. maximal fixture input vs round-trip output — a key the binary drops
//!    from the fixture, or a schema key the fixture forgot, fails;
//! 2. the shipped seed template must lose nothing on the same round-trip;
//! 3. the doc's key inventory must equal the binary's supported set, both
//!    directions — no undocumented key, no phantom documented key;
//! 4. the doc's defaults snapshot must be value-equal to the binary's own
//!    serialization of a minimal config — no stale documented default.
//!
//! A final test pins the silent-ignore behaviour itself (a typo key parses,
//! takes effect never, and vanishes on round-trip), and the README's
//! `yaml` examples are checked against the same supported set so the
//! quickstart cannot rot back into advertising keys the binary never had.

use claude_governor::config::GovernorConfig;
use serde_yaml::Value;
use std::collections::BTreeSet;

const DOC: &str = include_str!("../docs/configuration-reference.md");
const README: &str = include_str!("../README.md");
const SEED_TEMPLATE: &str = include_str!("../config/governor.yaml");

/// The minimal config: only the required keys. Everything the binary
/// serializes on top of this IS the defaults snapshot.
const MINIMAL_YAML: &str = "pricing:\n  models: {}\n";

/// Every schema key, set to a distinctive non-default value, in struct field
/// order. Adding a field to any config struct without extending this fixture
/// (and the doc) fails `maximal_fixture_covers_the_whole_schema` with the
/// exact key that went missing.
const MAXIMAL_YAML: &str = r#"
credentials_path: "~/.claude-work/.credentials.json"
pricing:
  models:
    claude-opus-5:
      input_per_mtok: 5.5
      output_per_mtok: 25.5
      cache_write_5m_per_mtok: 6.5
      cache_write_1h_per_mtok: 10.5
      cache_read_per_mtok: 0.55
sprint:
  underutilization_threshold_pct: 40.0
  underutilization_hours_remaining: 1.5
  horizon_minutes: 60.0
  min_headroom_pct: 20.0
  max_workers_boost: 5
  max_cone_ratio: 1.7
  sprint_end_headroom_pct: 3.0
  pace_blocks: 6
daemon:
  loop_interval_secs: 120
  hysteresis_band: 2.0
  max_scale_up_per_cycle: 2
  max_scale_down_per_cycle: 2
  progressive_scaling: true
  min_scale_interval_secs: 30
  target_ceiling: 85.0
  mode: tmux
  pre_scale_minutes: 15
  log_max_bytes: 52428800
  log_backup_count: 5
  windows:
    five_hour:
      target_utilization: 0.8
alerts:
  command: [cgov-alert, create, --json, --title]
  close_command: [cgov-alert, close]
  update_command: [cgov-alert, update]
  cooldown_minutes: 45
  enabled: false
  min_severity: info
  low_cache_eff_threshold: 0.25
  low_cache_eff_intervals: 3
  auto_bead: true
composite_risk:
  enabled: true
  cost_threshold: 0.5
  binding_weight: 3.0
cone_scaling:
  narrow_threshold: 1.2
agents:
  example-pool:
    launch_cmd: "needle run --agent example --workspace {workspace} --id {id}"
    session_pattern: "example-*"
    heartbeat_dir: "~/.needle/state/heartbeats"
    min_workers: 1
    max_workers: 4
    subscription: true
    baseline_burn_rate:
      pct_per_worker_per_hour: 2.5
      dollars_per_worker_per_hour: 8.0
    windows: ["five_hour", "seven_day"]
"#;

const KEY_INVENTORY_BEGIN: &str = "<!-- key-inventory:begin -->";
const KEY_INVENTORY_END: &str = "<!-- key-inventory:end -->";
const DEFAULTS_BEGIN: &str = "<!-- defaults-snapshot:begin -->";
const DEFAULTS_END: &str = "<!-- defaults-snapshot:end -->";

/// Parse with the exact startup path (retired-reference guard included).
fn parse_ok(yaml: &str, what: &str) -> GovernorConfig {
    GovernorConfig::parse_and_validate(yaml)
        .unwrap_or_else(|e| panic!("{what} must parse: {e:#}"))
}

/// Dot-joined canonical path for one node, with the three keyed maps
/// wildcarded so real model ids / pool names / window names never appear in
/// a compared path.
fn canonical(path: &[String]) -> String {
    let mut segs = path.to_vec();
    if segs.len() >= 3 && segs[0] == "pricing" && segs[1] == "models" {
        segs[2] = "*".to_string();
    } else if segs.len() >= 2 && segs[0] == "agents" {
        segs[1] = "*".to_string();
    } else if segs.len() >= 3 && segs[0] == "daemon" && segs[1] == "windows" {
        segs[2] = "*".to_string();
    }
    segs.join(".")
}

/// Every path in a config tree: one entry per mapping node (sections and
/// keyed maps included) and one per leaf. Scalars/sequences/nulls are
/// leaves; their contents are values, not keys.
fn canonical_paths(value: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk(value, &mut Vec::new(), &mut out);
    out
}

fn walk(value: &Value, prefix: &mut Vec<String>, out: &mut BTreeSet<String>) {
    match value {
        Value::Mapping(map) => {
            if !prefix.is_empty() {
                out.insert(canonical(prefix));
            }
            for (k, v) in map {
                let key = k
                    .as_str()
                    .unwrap_or_else(|| panic!("non-string config key: {k:?}"))
                    .to_string();
                prefix.push(key);
                walk(v, prefix, out);
                prefix.pop();
            }
        }
        _ => {
            assert!(
                !prefix.is_empty(),
                "walk reached a leaf at the document root"
            );
            out.insert(canonical(prefix));
        }
    }
}

fn round_trip(config: &GovernorConfig) -> Value {
    serde_yaml::to_value(config).expect("GovernorConfig must serialize")
}

fn diff(a: &BTreeSet<String>, b: &BTreeSet<String>) -> Vec<String> {
    a.difference(b).cloned().collect()
}

/// The lines between two HTML-comment markers in a markdown doc, with
/// comment lines and code fences dropped — the machine-readable block.
fn marker_section(doc: &str, begin: &str, end: &str) -> Vec<String> {
    let start = doc.find(begin).unwrap_or_else(|| panic!("doc missing {begin}"));
    let after = start + begin.len();
    let stop = doc[after..]
        .find(end)
        .unwrap_or_else(|| panic!("doc missing {end} after {begin}"));
    doc[after..after + stop]
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```"))
        .map(str::to_string)
        .collect()
}

/// Same region, but indentation preserved — for the defaults snapshot YAML.
fn marker_section_raw(doc: &str, begin: &str, end: &str) -> String {
    let start = doc.find(begin).unwrap_or_else(|| panic!("doc missing {begin}"));
    let after = start + begin.len();
    let stop = doc[after..]
        .find(end)
        .unwrap_or_else(|| panic!("doc missing {end} after {begin}"));
    doc[after..after + stop]
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with("```")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn documented_inventory() -> BTreeSet<String> {
    marker_section(DOC, KEY_INVENTORY_BEGIN, KEY_INVENTORY_END)
        .into_iter()
        .collect()
}

fn documented_defaults() -> Value {
    let yaml = marker_section_raw(DOC, DEFAULTS_BEGIN, DEFAULTS_END);
    serde_yaml::from_str(&yaml)
        .unwrap_or_else(|e| panic!("defaults snapshot block must be valid YAML: {e}"))
}

/// Fenced ```yaml blocks in a README-style markdown doc.
fn fenced_yaml_blocks(doc: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = doc.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "```yaml" {
            let mut body = Vec::new();
            for line in lines.by_ref() {
                if line.trim() == "```" {
                    break;
                }
                body.push(line);
            }
            blocks.push(body.join("\n"));
        }
    }
    blocks
}

// ---------------------------------------------------------------------------
// 1. The maximal fixture and the binary agree — in both directions.
// ---------------------------------------------------------------------------

/// input − output: a fixture key the binary silently dropped. output −
/// input: a schema key (struct field) the fixture forgot. Either way the
/// fixture is stale and every downstream comparison would be blind.
#[test]
fn maximal_fixture_covers_the_whole_schema() {
    let input = canonical_paths(&serde_yaml::from_str(MAXIMAL_YAML).unwrap());
    let output = canonical_paths(&round_trip(&parse_ok(MAXIMAL_YAML, "maximal fixture")));

    let ignored = diff(&input, &output);
    assert!(
        ignored.is_empty(),
        "fixture keys the binary silently ignores (dropped on round-trip): {ignored:?}\n\
         these keys are NOT part of the schema — remove them from MAXIMAL_YAML and from \
         docs/configuration-reference.md, or the struct field they were meant to test is gone"
    );

    let missing = diff(&output, &input);
    assert!(
        missing.is_empty(),
        "schema keys missing from MAXIMAL_YAML: {missing:?}\n\
         a struct field was added (or un-defaulted) in src/config.rs without a fixture entry — \
         add each key to MAXIMAL_YAML with a distinctive value AND to the key inventory in \
         docs/configuration-reference.md"
    );
}

/// The fixture's values must survive verbatim: a key that parses but does
/// not bind (shadowed, re-defaulted, coerced) is silent all the same. Full
/// value-tree equality, not just key names.
#[test]
fn maximal_config_round_trips_value_identical() {
    let input: Value = serde_yaml::from_str(MAXIMAL_YAML).unwrap();
    let output = round_trip(&parse_ok(MAXIMAL_YAML, "maximal fixture"));
    assert_eq!(
        input, output,
        "a configured value did not survive the parse→serialize round-trip"
    );
}

// ---------------------------------------------------------------------------
// 2. The shipped seed template loses nothing.
// ---------------------------------------------------------------------------

/// The template is what every first run copies into the live path; a key it
/// carries that the binary drops is a lie shipped to every new install. The
/// reverse direction is not asserted — the template legitimately exercises
/// only a subset of the schema and lets defaults cover the rest (that
/// coverage is `defaults_snapshot_matches_the_binary`).
#[test]
fn seed_template_has_no_silently_ignored_keys() {
    let input = canonical_paths(&serde_yaml::from_str(SEED_TEMPLATE).unwrap());
    let output = canonical_paths(&round_trip(&parse_ok(SEED_TEMPLATE, "seed template")));
    let ignored = diff(&input, &output);
    assert!(
        ignored.is_empty(),
        "config/governor.yaml carries keys the binary silently ignores: {ignored:?}\n\
         fix the template, or the schema lost a field the template still documents"
    );
}

// ---------------------------------------------------------------------------
// 3. The documented inventory IS the supported set.
// ---------------------------------------------------------------------------

/// Supported but undocumented: a struct field added without a doc entry
/// fails here, named.
#[test]
fn every_supported_key_is_documented() {
    let supported = canonical_paths(&round_trip(&parse_ok(MAXIMAL_YAML, "maximal fixture")));
    let documented = documented_inventory();
    let undocumented = diff(&supported, &documented);
    assert!(
        undocumented.is_empty(),
        "keys the binary supports but docs/configuration-reference.md does not list: \
         {undocumented:?}\n\
         add them to the key-inventory block (and the schema tables) — the doc may not \
         lag the schema"
    );
}

/// Documented but unsupported: a key the doc advertises that serde drops is
/// worse than undocumented — operators will configure it and nothing will
/// happen. Fails named.
#[test]
fn no_phantom_documented_keys() {
    let supported = canonical_paths(&round_trip(&parse_ok(MAXIMAL_YAML, "maximal fixture")));
    let documented = documented_inventory();
    let phantom = diff(&documented, &supported);
    assert!(
        phantom.is_empty(),
        "keys documented in docs/configuration-reference.md that the binary does not \
         support: {phantom:?}\n\
         remove them from the inventory, or restore the struct field — a documented key \
         that does nothing is a silent config trap"
    );
}

/// Defaults are part of the contract: the snapshot block must equal the
/// binary's own serialization of the minimal config, value for value.
#[test]
fn defaults_snapshot_matches_the_binary() {
    let actual = round_trip(&parse_ok(MINIMAL_YAML, "minimal config"));
    let documented = documented_defaults();
    assert_eq!(
        documented, actual,
        "docs/configuration-reference.md defaults snapshot drifted from the binary — \
         regenerate it from a minimal `pricing: {{models: {{}}}}` config"
    );
}

// ---------------------------------------------------------------------------
// 4. The silent-ignore behaviour itself, pinned.
// ---------------------------------------------------------------------------

/// A typo'd key parses clean and does nothing — the hazard the retired-
/// reference guard and the inventory above exist because of. Pinned here so
/// the behaviour can only change deliberately, with this test and the doc's
/// "Silent tolerance" section changed together.
#[test]
fn unknown_keys_parse_clean_but_are_silently_ignored() {
    let typoed = r#"
pricing:
  models: {}
daemon:
  poll_interval_secs: 30
agents:
  example-pool:
    launch_cmd: "echo test"
    session_pattern: "test-*"
    heartbeat_dir: "/tmp/heartbeats"
    min_worker: 2
"#;
    let config = parse_ok(typoed, "typo'd config");
    assert_eq!(
        config.daemon.loop_interval_secs, 300,
        "the typo'd daemon block must not disturb the real default"
    );
    assert_eq!(
        config.agents.get("example-pool").unwrap().min_workers, 0,
        "the typo'd agent key must not disturb the real default"
    );

    let input = canonical_paths(&serde_yaml::from_str(typoed).unwrap());
    let output = canonical_paths(&round_trip(&config));
    let ignored: BTreeSet<String> = diff(&input, &output).into_iter().collect();
    assert_eq!(
        ignored,
        BTreeSet::from([
            "agents.*.min_worker".to_string(),
            "daemon.poll_interval_secs".to_string()
        ]),
        "expected exactly the two typo keys to be dropped — anything else dropped means \
         the schema changed shape"
    );
}

// ---------------------------------------------------------------------------
// 5. README examples cannot rot back into phantom keys.
// ---------------------------------------------------------------------------

/// The README quickstart once showed `polling:`, flat `pricing.<model>`, and
/// `agents.<pool>.workspace` — none of which the binary has ever read. Every
/// fenced ```yaml block in the README must be a subset of the supported key
/// set, so a future edit re-introducing a phantom key fails here.
#[test]
fn readme_yaml_examples_use_only_supported_keys() {
    let supported = canonical_paths(&round_trip(&parse_ok(MAXIMAL_YAML, "maximal fixture")));
    let blocks = fenced_yaml_blocks(README);
    assert!(
        !blocks.is_empty(),
        "no ```yaml blocks found in README.md — if the examples moved, update this test"
    );
    for (i, block) in blocks.iter().enumerate() {
        let value: Value = serde_yaml::from_str(block)
            .unwrap_or_else(|e| panic!("README yaml block #{i} must be valid YAML: {e}"));
        let paths = canonical_paths(&value);
        let phantom = diff(&paths, &supported);
        assert!(
            phantom.is_empty(),
            "README yaml block #{i} shows keys the binary does not support: {phantom:?}\n\
             the quickstart must only ever advertise real configuration"
        );
    }
}
