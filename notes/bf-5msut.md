# Pluck exclude_labels Configuration

## Location of Configuration

Pluck's `exclude_labels` configuration is stored in two locations:

1. **Global Config**: `~/.config/needle/config.yaml` under `strands.pluck.exclude_labels`
2. **Code Default**: `/home/coding/NEEDLE/src/strand/pluck.rs` constant `DEFAULT_EXCLUDE_LABELS`

## Currently Configured Values

### Global Config (`~/.config/needle/config.yaml`)

```yaml
strands:
  pluck:
    exclude_labels: []    # Empty array - no custom exclusions
    split_after_failures: 3
```

The current global config has an **empty** `exclude_labels` array. This means no custom labels are being excluded at the configuration level.

### Code Default (`NEEDLE/src/strand/pluck.rs:13`)

When `exclude_labels` is empty (as it currently is), PluckStrand falls back to the built-in default:

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

**Therefore, the ACTUAL labels being excluded are:**
- `deferred`
- `human`
- `blocked`
- `starvation-alert`

## How exclude_labels Affects Bead Visibility

### Selection Flow

1. **Bead Store Query**: Pluck calls `store.ready(&filters)` where `Filters` includes the exclude_labels
2. **Defensive Filtering**: Even if the backend doesn't apply label exclusion, PluckStrand applies a defensive `retain()` filter:
   ```rust
   candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));
   ```

### Filtering Behavior

A bead is **excluded from Pluck selection** if:
- Its `status` is `Open` AND it has **any** label matching the exclude_labels list
- This applies regardless of the bead's priority, age, or other attributes

### Example

Given the current defaults:
- A bead with labels `["deferred", "bug"]` → **EXCLUDED** (has "deferred")
- A bead with labels `["human", "documentation"]` → **EXCLUDED** (has "human")
- A bead with labels `["blocked", "bug"]` → **EXCLUDED** (has "blocked")
- A bead with labels `["starvation-alert"]` → **EXCLUDED** (exact match)
- A bead with labels `["bug", "enhancement"]` → **VISIBLE** (no excluded labels)

## Customization

### To Override Defaults

Edit `~/.config/needle/config.yaml`:

```yaml
strands:
  pluck:
    exclude_labels: ["deferred", "human"]  # Only exclude these two
```

When custom labels are provided, they **replace** the defaults entirely (the defaults are not merged).

### To Disable All Exclusions

Set an empty array (current configuration):

```yaml
strands:
  pluck:
    exclude_labels: []
```

This causes Pluck to use the built-in defaults (`deferred`, `human`, `blocked`, `starvation-alert`).

### To Exclude Additional Labels

You must explicitly list all labels you want excluded, including the defaults:

```yaml
strands:
  pluck:
    exclude_labels: ["deferred", "human", "blocked", "starvation-alert", "wip", "design"]
```

## Related Configuration

The `split_after_failures` setting (default: 3) controls when Pluck dispatches a SPLIT instruction instead of returning a bead for normal processing:

```yaml
strands:
  pluck:
    split_after_failures: 3  # Auto-split after 3 consecutive failures
```

## Source Reference

- **Config structure**: `/home/coding/NEEDLE/src/config/mod.rs:273-294` (PluckConfig)
- **Default constant**: `/home/coding/NEEDLE/src/strand/pluck.rs:13` (DEFAULT_EXCLUDE_LABELS)
- **Usage in PluckStrand**: `/home/coding/NEEDLE/src/strand/pluck.rs:105-125` (filtering logic)
- **Config file**: `~/.config/needle/config.yaml:82-84` (current values)
