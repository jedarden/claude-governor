# Pluck Configuration Documentation

**Bead:** bf-3y6nm  
**Date:** 2026-07-06  
**Purpose:** Baseline documentation of all Pluck configuration settings affecting bead visibility

---

## Overview

Pluck is the primary bead selection strand in NEEDLE. It filters beads based on labels, manages failure-driven splitting, and controls which beads are presented to agents for processing.

---

## Configuration Sources

Configuration resolution order (later overrides earlier):
1. Built-in defaults (code constants)
2. Global config: `~/.config/needle/config.yaml`
3. Workspace config: `.needle.yaml` (if present)
4. Environment variables (`NEEDLE_*`)
5. CLI arguments

---

## 1. exclude_labels Configuration

### Current Configuration

**Source:** `~/.config/needle/config.yaml:88`

```yaml
strands:
  pluck:
    exclude_labels: []        # Empty array
    split_after_failures: 3
```

### Built-in Default (Active Since Global Config is Empty)

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:13`

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

**Actual labels being excluded:**
- `deferred` - Beads marked for later processing
- `human` - Beads requiring human intervention
- `blocked` - Beads blocked by dependencies
- `starvation-alert` - Beads flagged for starvation risk

### Filtering Behavior

A bead is **excluded from Pluck selection** if:
- Its `status` is `Open` AND
- It has **any** label matching the exclude_labels list

**Examples:**
- Bead with `["deferred", "bug"]` → EXCLUDED (has "deferred")
- Bead with `["human", "documentation"]` → EXCLUDED (has "human")
- Bead with `["blocked", "bug"]` → EXCLUDED (has "blocked")
- Bead with `["starvation-alert"]` → EXCLUDED (exact match)
- Bead with `["bug", "enhancement"]` → VISIBLE (no excluded labels)

### Implementation Flow

1. **Bead Store Query**: Pluck calls `store.ready(&filters)` where `Filters` includes exclude_labels
2. **Defensive Filtering**: PluckStrand applies a secondary `retain()` filter:
   ```rust
   candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));
   ```

---

## 2. split_after_failures Configuration

### Current Configuration

**Source:** `~/.config/needle/config.yaml:89`

```yaml
strands:
  pluck:
    split_after_failures: 3
```

**Value:** `3` (default)

### Behavior

Controls when Pluck dispatches a `SPLIT` instruction instead of returning a bead for normal processing:

- After N consecutive claim failures, Pluck returns a `SPLIT` instruction
- This triggers workload distribution across workers
- Prevents starvation in multi-worker scenarios

**Implementation:** `/home/coding/NEEDLE/src/strand/pluck.rs`

---

## 3. Workspace Path Configuration

### Current Configuration

**Source:** `~/.config/needle/config.yaml:71-76`

```yaml
workspace:
  default: /home/coding/zai-proxy
  home: /home/coding/.needle
  labels: []
```

**Description:**
- `default`: Default workspace directory when not specified on CLI
- `home`: NEEDLE home directory (heartbeat files, log output)
- `labels`: Domain labels for cross-workspace skill sharing

### Workspace-Specific Override

**Current workspace:** `/home/coding/claude-governor`

**Status:** No `.needle.yaml` file exists in this workspace, so no local overrides are active.

---

## 4. Related Strand Configurations

### explore (Multi-workspace Discovery)

**Source:** `~/.config/needle/config.yaml:99-102`

```yaml
explore:
  enabled: true
  workspaces: []
  workspace_root: ~/
```

**Behavior:**
- Discovers beads across multiple workspaces
- Currently scanning from `~/` (user home)
- No workspace whitelist configured (empty `workspaces` array)

### weave (Gap Analysis)

**Source:** `~/.config/needle/config.yaml:113-118`

```yaml
weave:
  enabled: true
  max_beads_per_run: 5
  cooldown_hours: 24
  exclude_workspaces: []
  doc_patterns:
    - README*
    - AGENTS.md
    - docs/**
```

**Relevance to Pluck:**
- Creates new beads based on documentation gaps
- No workspaces excluded from analysis
- New beads flow through Pluck's exclude_labels filter

### mitosis (Bead Splitting)

**Source:** `~/.config/needle/config.yaml:121-124`

```yaml
mitosis:
  enabled: true
  first_failure_only: true
  force_failure_threshold: 0
```

**Relevance to Pluck:**
- Splits complex beads into smaller sub-beads
- Sub-beads inherit parent labels (affecting Pluck visibility)
- If parent has excluded label, sub-beads may also be excluded

---

## 5. Environment Variables

### Checked Variables

```bash
$ env | grep -i pluck
# (no output - no Pluck-specific environment variables set)
```

**Note:** Pluck does not currently support environment variable overrides. All configuration is via YAML files.

---

## 6. Worker Configuration (Affects Pluck Dispatch)

### Current Configuration

**Source:** `~/.config/needle/config.yaml:35-48`

```yaml
worker:
  max_workers: 17
  launch_stagger_seconds: 2
  idle_timeout: 60
  idle_action: wait
  max_claim_retries: 3
  claim_race_lost_skip: 5
  identifier_scheme: hostname_random
  cpu_load_warn: 0.8
  memory_free_warn_mb: 512
  building_timeout: 600
```

**Relevance to Pluck:**
- `max_workers: 17` - Pluck must distribute beads across 17 workers
- `idle_action: wait` - Workers wait for new beads when queue empty
- `claim_race_lost_skip: 5` - After 5 consecutive race losses, treats queue as empty

---

## Summary of Active Settings

| Setting | Value | Source | Effect |
|---------|-------|--------|--------|
| `exclude_labels` | `["deferred", "human", "blocked", "starvation-alert"]` | Code default (global config empty) | Filters beads with these labels |
| `split_after_failures` | `3` | Global config | Splits workload after 3 failures |
| Workspace default | `/home/coding/zai-proxy` | Global config | Fallback workspace when not specified |
| NEEDLE home | `/home/coding/.needle` | Global config | Heartbeat and log directory |
| Workspace overrides | None | No `.needle.yaml` in workspace | Uses global defaults |

---

## Configuration File Locations

| File | Path | Status |
|------|------|--------|
| Global NEEDLE config | `~/.config/needle/config.yaml` | ✅ Exists |
| Workspace config | `/home/coding/claude-governor/.needle.yaml` | ❌ Does not exist |
| Beads project config | `/home/coding/claude-governor/.beads/config.yaml` | ✅ Exists (minimal: issue_prefix, default_priority, default_type) |

---

## Verification Commands

```bash
# Check current Pluck exclude_labels
grep -A 2 "pluck:" ~/.config/needle/config.yaml

# Check workspace-specific overrides (should be empty)
cat /home/coding/claude-governor/.needle.yaml

# View active NEEDLE configuration
cat ~/.config/needle/config.yaml

# Check for Pluck environment variables
env | grep -i pluck
```

---

## Related Documentation

- **Pluck exclude_labels deep dive:** `notes/bf-5msut.md`
- **NEEDLE source code:** `/home/coding/NEEDLE/src/strand/pluck.rs`
- **Config structure:** `/home/coding/NEEDLE/src/config/mod.rs:273-294`
