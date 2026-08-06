# bf-47asm — Default alert-bead command: `br create` → `bf create`

## Outcome

One real fix: `docs/research/alerts.md:141` still configured the alert command as
`br`. Everything else in the bead's stated scope was already correct.

## Why prior sweeps missed it

Beads bf-4qd6k, bf-3g1ns, bf-1hbmk and bf-6bicx all swept for the **literal string**
`br create` and reported 0 hits in `src/ docs/ config/ scripts/`. That grep is
correct for shell/Rust call sites but blind to YAML sequences, where the argv is
split one token per line:

```yaml
command:
  - br        # <- 'br' and 'create' never adjacent; 'br create' does not match
  - create
```

Grepping for the bare token `\bbr\b` instead of `br create` surfaces it. Worth
remembering for any future deprecation sweep over config examples.

## Change made

`docs/research/alerts.md`, the `## Alert Configuration` example:

```diff
   command:
-    - br
+    - bf
     - create
-    --type
-    human
+    - --type
+    - human
```

Two defects in five lines: the deprecated `br` shim, and a malformed YAML list —
`--type` and `human` were missing their `- ` prefixes, so the block would have
parsed as `command: ["br", "create"]` plus a syntax error on the bare scalars.
Anyone copying this example got a broken config, not just a deprecated one.

## Verified already-correct (no edit needed)

| Item from bead description | State |
|---|---|
| `src/config.rs::default_alert_command()` | Already `["bf","create","--type","human"]` (fixed by `8b08f68`, 2026-08-03) |
| Unit test asserting `default_alert_command()[0] == "bf"` | Already exists — `test_default_alert_command_uses_bf` (added by `78fe193`), asserts `cmd[0]`, the full vec, and `AlertConfig::default().command[0]` |
| Doc comments in `src/config.rs` / `src/alerts.rs` | Only `br` mention is `src/config.rs:1306`, the test doc-comment that *names* `br` as the shim that must never reappear. Correct as written — left alone |
| `config/governor.yaml` | Has no `alerts.command` key at all; inherits the `bf` default |
| `~/.config/claude-governor/governor.yaml` (live) | Same — no `alerts.command` key. Zero `br` tokens in the file |

The bead cited `docs/research/alerts.md:470`; the file is now 188 lines (rewritten
since the bead was filed), and the surviving occurrence was at line 141.

## Out of scope — deliberately not changed

`\bbr\b` across `src/ docs/ config/ scripts/` leaves these, none of which are alert
config or new authored calls:

- `src/state.rs:1901-1909`, `src/governor.rs:1196-1199` — local variable `br`
  bound to a burn-rate value. Unrelated to the CLI.
- `docs/pluck-*.md`, `docs/bead-visibility-*.md`, `docs/plan/pluck-configuration.md`,
  `docs/research/pluck-starvation-reproduction.md` — reference docs that describe
  the `br` CLI itself (`br ready`, `br doctor --repair`, `br config list`) or record
  transcripts of it being run. Rewriting these to `bf` would falsify recorded
  output. Renaming them is a separate docs decision, not this bead's.
- `src/config.rs:1306` — see table above.

## Gate

`~/.cargo/bin/cargo test` → **850 passed, 0 failed** across all 18 targets,
including `config::tests::test_default_alert_command_uses_bf`.

(Per repo convention the wrapper `cargo` discards stderr and can exit 0 on failure;
`~/.cargo/bin/cargo` was used directly.)
