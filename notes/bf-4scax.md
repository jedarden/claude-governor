# Pluck exclude_labels Settings Documentation

**Task:** Document all current exclude_labels settings in the Pluck configuration  
**Completed:** 2026-08-03  
**Bead:** bf-4scax

## Current exclude_labels Settings

### Source Location
- **File:** `/home/coding/NEEDLE/src/strand/pluck.rs`
- **Line:** 21
- **Type:** Compiled constant (hardcoded in binary)

### exclude_labels List

| Label | Purpose |
|-------|---------|
| `deferred` | Beads marked for later processing |
| `human` | Beads requiring human intervention |
| `blocked` | Beads with blocking dependencies |

### Implementation Code
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

### Configuration Behavior

1. **Default Application:** When `PluckStrand::new(vec![])` is called with empty exclude_labels, these defaults are automatically applied.

2. **Custom Override:** Custom exclude_labels override defaults completely (not merged). Currently **no custom override is configured** in the deployment.

3. **Double Filtering:** The excluded labels are applied twice:
   - **Store-level filter:** Via `bead_store::Filters` in the initial query
   - **Strand-level defensive filter:** Line 139 in pluck.rs removes beads with excluded labels as a defensive guard

### Usage Context

- **Deployment:** No custom exclude_labels configured - uses defaults
- **Scope:** These exclude_labels are used in the Pluck strand, which handles >90% of all bead processing in NEEDLESS
- **Workspace:** Applied to `/home/coding/claude-governor` workspace bead store

### Related References

- Pluck Configuration Documentation: `/home/coding/claude-governor/docs/plan/pluck-configuration.md`
- NEEDLE Source: `/home/coding/NEEDLE/src/strand/pluck.rs`
- NEEDLE Config: `~/.needle/config.yaml`

### Patterns and Wildcards

**None used.** The exclude_labels are simple string literals without pattern matching or wildcard support.

---

## Summary

The Pluck configuration currently uses **3 hardcoded exclude_labels**:
1. `deferred`
2. `human` 
3. `blocked`

These are compiled into the NEEDLE binary at `/home/coding/NEEDLE/src/strand/pluck.rs:21` and no custom overrides exist in the current deployment. Labels are filtered both at the bead store query level and defensively in the strand itself.
