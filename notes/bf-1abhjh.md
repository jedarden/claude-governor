# sonnet_pct Usage Survey (bf-1abhjh)

## Summary
Searched the cgov codebase for all `sonnet_pct` usage. Found **88 total occurrences** across **5 Rust source files**.

## Files by Occurrence Count

| File | Occurrences | Role |
|------|-------------|------|
| `src/governor.rs` | 60 | Core governor logic - heavy usage |
| `src/state.rs` | 25 | State management & serialization |
| `src/poller.rs` | 1 | API polling comments |
| `src/simulator.rs` | 1 | Test simulation defaults |
| `src/status_display.rs` | 1 | Display formatting |

## Detailed Breakdown

### 1. `src/governor.rs` (60 occurrences)
- **Primary usage location** - the core governor daemon logic
- Likely used for:
  - Subscription window calculations
  - Worker scaling decisions
  - Cost-priority distribution logic
  - Capacity forecasting

### 2. `src/state.rs` (25 occurrences)
- State struct definition and persistence
- Likely includes:
  - Field definition in `Usage` struct
  - Serialization/deserialization logic
  - Test fixtures and assertions
  - Default values

### 3. `src/poller.rs` (1 occurrence)
- Appears in comments/documentation
- Context: "conditional logic like 'only update sonnet_pct if...'"

### 4. `src/simulator.rs` (1 occurrence)
- Test/simulation code
- Default value: `sonnet_pct: 50.0`

### 5. `src/status_display.rs` (1 occurrence)
- Display/formatting for status output
- Example value: `sonnet_pct: 63.5`

## Search Command Used
```bash
grep -r "sonnet_pct" /home/coding/claude-governor --include="*.rs"
```

## Notes
- All usage is in Rust source files (`*.rs`)
- No occurrences found in config files, markdown, or YAML
- The field appears to be a legacy/backward-compatibility field based on comments in `state.rs`
- Heavy concentration in `governor.rs` suggests this is central to the governor's capacity management logic
