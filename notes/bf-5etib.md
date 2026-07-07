# Pluck Query Construction Analysis

## Task: Identify Pluck query construction code

**Bead:** bf-5etib

## Summary

Located the exact code where Pluck constructs queries with filters.

## File Paths and Functions

### Primary Query Construction

**File:** `/home/coding/NEEDLE/src/bead_store/mod.rs`

Two implementations handle query construction:

1. **`BrCliBeadStore::ready()`** (lines 578-598)
   - Uses `br ready --json` command
   - Adds `--assignee` arg if filter.assignee is set

2. **`BfCliBeadStore::ready()`** (lines 1090-1110)
   - Uses `bf list --json --status open --limit 0` command
   - Adds `--assignee` arg if filter.assignee is set

### Query Construction Flow

**File:** `/home/coding/NEEDLE/src/strand/pluck.rs`

**Function:** `PluckStrand::evaluate()` (lines 103-156)

1. Creates `Filters` struct (line 105-108):
   ```rust
   let filters = Filters {
       assignee: None,
       exclude_labels: self.exclude_labels.clone(),
   };
   ```

2. Calls bead store: `store.ready(&filters).await` (line 110)

3. Store constructs CLI command:
   - **br**: `vec!["ready", "--json", "--assignee", assignee]` (if present)
   - **bf**: `vec!["list", "--json", "--status", "open", "--limit", "0", "--assignee", assignee]` (if present)

4. Both apply label exclusion filter client-side (lines 592-597 for br, 1104-1109 for bf):
   ```rust
   if !filters.exclude_labels.is_empty() {
       beads.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
   }
   ```

5. Pluck strand does defensive filtering (lines 118-126):
   ```rust
   candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));
   ```

## Parameter Flow

| Parameter | Source | Destination |
|-----------|--------|-------------|
| `workspace_path` | `store.workspace` (set during store initialization) | `current_dir` in `run_br_in()` / `run_bf_in()` |
| `assignee` | `filters.assignee` | CLI arg `--assignee` (if not None) |
| `exclude_labels` | `self.exclude_labels` (PluckStrand) | Client-side filter (not supported by CLIs) |
| `state` | Implicit in `br ready` / baked into `bf list --status open` | Handled by CLI command itself |

## Key Insight

The `exclude_labels` parameter is **NOT passed to the CLI** - it's applied client-side after the query returns. This is because neither `br` nor `bf` CLI supports label exclusion natively.

The double filtering (store-side + strand-side) is defensive - it prevents spin loops when the backend omits label data from its JSON output.

## Next Steps

When adding logging, the key insertion points are:
1. Line 110 in `pluck.rs`: after `store.ready()` returns
2. Lines 589 and 1101 in `bead_store/mod.rs`: after CLI command returns
