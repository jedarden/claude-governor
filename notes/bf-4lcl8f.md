# Compiler Warnings - Filter Changes (bf-4lcl8f)

## Date: 2026-07-27

## Summary
Ran `RUSTFLAGS="-D warnings" cargo build` to identify compiler warnings introduced by recent filter changes in binding-window selection.

## Warnings Found

### 1. Unused Import - `std::collections::HashMap`

**Files affected:**
- `src/alerts.rs:12` - unused import of `std::collections::HashMap`
- `src/capacity_summary.rs:23` - unused import of `std::collections::HashMap`
- `src/narrator.rs:14` - unused import of `std::collections::HashMap`

**Warning type:** `unused_imports`

**Description:** These files import `HashMap` from std::collections but don't use it in the current code.

**Fix:** Remove the unused import lines or add `#[allow(unused_imports)]` if needed for future use.

---

### 2. Unused Import - `chrono::Datelike`

**File:** `src/snapshot_fixtures.rs:13`

**Line:** `use chrono::{DateTime, Datelike, Utc};`

**Warning type:** `unused_imports`

**Description:** The `Datelike` trait is imported from chrono but not used in the code.

**Fix:** Remove `Datelike` from the import or add `#[allow(unused_imports)]`.

---

### 3. Unused Doc Comment

**File:** `src/poller.rs:292`

**Line:** 
```rust
/// **Model-agnostic weekly_scoped pct source: reads from limits[].percent**
```

**Warning type:** `unused_doc_comments`

**Description:** A documentation comment (`///`) is used on an expression field where rustdoc doesn't generate documentation. This is a struct expression field, not a struct definition field.

**Fix:** Change to a regular comment (`//`):
```rust
// **Model-agnostic weekly_scoped pct source: reads from limits[].percent**
```

---

### 4. Type Mismatch in Filter Call (ERROR, not warning)

**File:** `src/governor.rs:4863`

**Line:**
```rust
.filter(|(_, window)| !is_structurally_inactive(window, state))
```

**Error type:** Type mismatch (E0308)

**Description:** The `is_structurally_inactive` function expects:
- First parameter: `&UsageWindow` 
- Second parameter: `&GovernorState`

But the filter provides:
- First parameter: `&&WindowForecast` (double reference, wrong type)
- Second parameter: `GovernorState` (not a reference)

**Fix:** The compiler helpfully suggests:
```rust
.filter(|(_, window)| !is_structurally_inactive(window, &state))
```

This issue is in the recent filter() addition for excluding inactive windows in binding-window selection.

---

## Notes

1. **Severity:** With `-D warnings`, all warnings are treated as errors and prevent compilation.
2. **Scope:** These warnings exist in the committed code (post bf-1nw6na filter changes).
3. **Impact:** The type mismatch at governor.rs:4863 prevents the code from compiling at all.
4. **Recent changes:** The filter() call was added in commit 7d6a224 to exclude inactive windows from binding-window selection.

## Recommendation

Fix the type mismatch error first (governor.rs:4863), then decide whether to:
1. Remove unused imports, or
2. Allow them with `#[allow(unused_imports)]` if they're planned for future use
