# bf-ns3pv — governor.yaml parse check + alert-command inheritance note

Follow-up to bf-mohgm, which recorded the `br` -> `bf` alert-command fix as "not
applicable" because the live config has no `alerts.command` key. This bead
verifies the live file is intact and documents *why* that absence is correct.

## 1. The live file parses

`~/.config/claude-governor/governor.yaml` (4921 bytes) is valid YAML and
deserializes cleanly through the application's own loader — not just a generic
YAML parser.

Plain YAML parse — top-level keys `pricing`, `agents`, `daemon`, `alerts`.

Loader parse — `GovernorConfig::load_from_path` (`src/config.rs:665`) against the
live path succeeded, yielding:

```
alerts.command  = ["bf", "create", "--type", "human"]
alerts.enabled  = true
alerts.auto_bead = false
```

This is the stronger check: `serde` would reject unknown-shaped or malformed
`alerts` fields at deserialization, so a successful load proves the file is
structurally valid for this binary, not merely well-formed YAML.

## 2. No `alerts.command` key is present — and that is correct

The `alerts` block in the live file contains exactly:

`enabled`, `cooldown_minutes`, `min_severity`, `low_cache_eff_threshold`,
`low_cache_eff_intervals`, `auto_bead`

There is no `command` key. The field is supplied by a serde default:

- `src/config.rs:534-535` — `#[serde(default = "default_alert_command")]` on
  `pub command: Vec<String>`
- `src/config.rs:569-576` — `fn default_alert_command() -> Vec<String>` returns
  `vec!["bf", "create", "--type", "human"]`
- `src/config.rs:605` — `AlertConfig::default()` routes through the same function,
  so the programmatic default and the deserialization default cannot drift.

Because the key is absent, the config **inherits the `src/config.rs` default,
which already uses `bf`**. Editing the YAML to add an explicit `command: [bf, ...]`
would be redundant, and would additionally pin the value against future changes
to the default — which is why bf-mohgm correctly closed as not applicable rather
than adding the key.

## 3. Regression coverage already exists

`src/config.rs:1310` — `test_default_alert_command_uses_bf` asserts both
`cmd[0] == "bf"` and the full vector, plus `AlertConfig::default().command[0]`.
Its doc comment states the intent: catch a silent regression that would send
every alert to the wrong command. Run and passing:

```
test config::tests::test_default_alert_command_uses_bf ... ok
```

So a `bf` -> `br` regression in the default is caught by CI, and the live file
inherits that guarded default.

## Verification method

The loader check was done with a temporary `#[ignore]`d probe test appended to
`src/config.rs`, run with `--ignored --nocapture`, then removed. `git diff` on
`src/config.rs` is empty and `cargo check --lib` still passes, so this bead
leaves no source changes behind. (Note: use `~/.cargo/bin/cargo` — the `cargo`
wrapper on PATH discards stderr and exits 0 even on failure.)

## Close note

File parses (both plain YAML and via `GovernorConfig::load_from_path`); no
`alerts.command` key present; **inherits `src/config.rs` default (`bf`)** from
`default_alert_command()` at `src/config.rs:569`, covered by
`test_default_alert_command_uses_bf`.
