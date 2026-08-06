# bf-5ke9f — cargo test suite run

Part 1 of the split of bf-4qd6k (final gate for the `br create` -> `bf create`
default-alert-command fix). Scope: run the test suite only; no config or doc edits.

## Command

```
~/.cargo/bin/cargo test
```

Run from `/home/coding/claude-governor` using the absolute cargo path, since the
`cargo` wrapper on PATH discards stderr and exits 0 even on failure.

## Result: PASS

All 18 test targets green — 849 passed, 0 failed, 8 ignored.

```
running 750 tests
test result: ok. 750 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.28s
running 10 tests
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
running 10 tests
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.90s
running 4 tests
test result: ok. 1 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 12 tests
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 5 tests
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 9 tests
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 2 tests
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 5 tests
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
running 11 tests
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 22 tests  (doc-tests)
test result: ok. 17 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in 0.59s
```

No follow-up bead filed — nothing failed.

## Note

The build emits warnings (unused imports/variables in test modules, three
`unnecessary parentheses` warnings in `src/governor.rs` around lines 7213 and
7219). These are pre-existing and out of scope for this bead.
