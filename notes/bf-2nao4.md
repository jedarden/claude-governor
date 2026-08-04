# Pluck Workspace Path Filter Test Results - Bead bf-2nao4

**Date:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`  
**Test Type:** Workspace path format isolation

## Objective

Test different workspace path formats to identify which successfully return beads from Pluck queries.

## Test Results

### ✅ Successful Formats (Return Beads)

| Format | Example | Ready Beads | Status |
|--------|---------|-------------|--------|
| **Absolute path** | `/home/coding/claude-governor` | 15 | ✅ Correct |
| **Relative path (dot)** | `.` | 15 | ✅ Works when run from workspace root |
| **Relative path (dot-slash)** | `./` | 15 | ✅ Works when run from workspace root |
| **Trailing slash** | `/home/coding/claude-governor/` | 15 | ✅ Normalizes correctly |
| **Double slash** | `/home/coding//claude-governor` | 15 | ✅ Normalizes correctly |

### ❌ Unsuccessful Formats (No Beads or Don't Exist)

| Format | Example | Ready Beads | Issue |
|--------|---------|-------------|-------|
| **Parent path (incorrect)** | `/home/coding` | 0 | Points to wrong bead store |
| **Workspace name only** | `claude-governor` | N/A | Path doesn't exist |
| **User home tilde** | `~/claude-governor` | N/A | Path doesn't exist (not expanded) |
| **Non-existent path** | `/nonexistent/path` | N/A | Path doesn't exist |

## Key Findings

### 1. Path Resolution Behavior

**Relative paths work only when executed from the correct directory:**
- Running from `/home/coding/claude-governor`: Relative paths resolve correctly
- Running from any other directory: Relative paths fail

**Absolute paths always work:**
- Both `/home/coding/claude-governor` and `/home/coding/claude-governor/` normalize correctly
- Double slashes are normalized: `/home/coding//claude-governor` → works

### 2. Parent Directory Pitfall (Root Cause of Original Issue)

The parent path `/home/coding` has its own bead store:
- **Total beads:** 5
- **Open beads:** 0
- **Ready beads:** 0

This was the root cause of the "empty pluck" issue documented in **bf-34ycm**. When NEEDLE's `workspace.default` was incorrectly set to `/home/coding`, workers queried the wrong database and found no ready beads.

### 3. Tilde Expansion Not Supported

The path format `~/claude-governor` does NOT work because PathBuf does not expand `~` to the home directory. The literal string `~/claude-governor/.beads/beads.db` does not exist.

## Correct Configuration

For NEEDLE configuration (`~/.config/needle/config.yaml`), use the **absolute path format**:

```yaml
workspace:
  default: /home/coding/claude-governor
```

This format:
- ✅ Works regardless of current working directory
- ✅ Resolves correctly to the intended bead store
- ✅ Is compatible with NEEDLE's workspace path handling

## Acceptance Criteria Status

- [x] **Test Pluck with workspace path filter only:** Completed
- [x] **Try different workspace path values:** Tested 9 formats
- [x] **Document which path format returns beads:** Absolute and relative paths work
- [x] **Identify correct workspace path format:** Absolute path `/home/coding/claude-governor`

## Database State at Test Time

**Claude-governor workspace:**
- Total beads: 1,215
- Open beads: 18
- Ready beads (Pluck query): 15

**Parent directory workspace (`/home/coding`):**
- Total beads: 5
- Open beads: 0
- Ready beads: 0

## Recommendations

1. **Always use absolute paths** in NEEDLE configuration files
2. **Verify workspace path** points to the intended bead store
3. **Check for conflicting bead stores** in parent directories
4. **Never use tilde expansion** - it's not supported by PathBuf
5. **Test path resolution** before deploying NEEDLE workers

## Related Beads

- **bf-34ycm:** Pluck Configuration Fix - Original issue where wrong path caused empty pluck
- **bf-4k2j5:** Pluck configuration investigation
- **bf-15prd:** Bead visibility configuration analysis

## Impact

This test confirms that the workspace path format is critical for Pluck functionality. The absolute path format is the only reliable method for ensuring NEEDLE workers query the correct bead store database.
