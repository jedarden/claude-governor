# Task bf-45tkc: Normalize config path references in plan.md

## Task
Replace all references to the old ~/.needle/config/ path with the implemented ~/.config/claude-governor/ location in docs/plan/plan.md.

## Verification Result
All path references in plan.md are already correct and aligned with the implementation:

1. **Configuration File section** (line 1335):
   - `~/.config/claude-governor/governor.yaml` ✓ CORRECT

2. **promotions_file path** (line 1394):
   - `promotions_file: ~/.config/claude-governor/promotions.json` ✓ CORRECT

3. **Doctor remediation text in Component 19** (line 1229):
   - `"Update pricing in ~/.config/claude-governor/governor.yaml"` ✓ CORRECT

## Verification Method
- grep search for `.needle/config` in plan.md: **0 references found**
- Manual verification of all three sections mentioned in task description
- Comparison with implementation in src/config.rs (lines 514-554)
- Comparison with README.md

## Conclusion
The task acceptance criteria are already met:
- No remaining ~/.needle/config/governor.yaml references in plan.md ✓
- All paths point to ~/.config/claude-governor/governor.yaml ✓

No file changes were needed.
