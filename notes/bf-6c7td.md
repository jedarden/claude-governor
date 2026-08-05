# bf-6c7td — `calculate_window_pct_delta` is lexically inside the Some-Some block

Verified against `src/governor.rs` at commit `8e8bf9f` (2026-08-05), using the
line range recorded by [bf-65oue](bf-65oue-some-some-line-ranges.md).

## Result: confirmed

**`src/governor.rs:2404`** — the call line:

```rust
let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
```

It sits strictly inside the Some-Some block: `2393 < 2404 < 2516`.

## Containment proved by braces, not indentation

Brace-depth walk starting at the block's opening brace (comments and string
literals stripped so braces inside them do not skew the count):

| Line | Depth after processing the line | Meaning |
|------|--------------------------------|---------|
| 2393 | 1 | `{` opens the Some-Some body |
| 2404 | 1 | the call — still inside, block never closed in between |
| 2516 | 1 | `} else {` — the `}` closes the body, the `{` opens the else-arm, so the running depth is net-neutral here |
| 2518 | 0 | `}` ends the whole `if let … else` construct |

Depth is `1` continuously from 2393 through 2404, so no intervening `}` closes
the block before the call. The body's own closing brace is the `}` at the head
of line 2516, giving `2393 < 2404 < 2516` — the call is inside the body and not
in the else-arm. Because `} else {` is net-neutral, the raw depth counter does
not return to `0` until 2518, where the full construct ends.

The block header for reference:

- 2391 — `if let (Some(prev), Some(curr)) =`
- 2392 — `(&state.previous_api_snapshot, &state.current_api_snapshot)`
- 2393 — `{`
- 2394-2403 — `prev_pct` / `curr_pct` `WindowPctSnapshot` construction
- **2404 — the call**
- 2407-2409 — `state.p5h_delta` / `p7d_delta` / `p7ds_delta` assignments
- 2516 — `} else {`
- 2518 — `}`

## No second call inside the same test

Enclosing test is `test_consecutive_snapshots_governor_cycle`, spanning
**2249-2682** (verified by the same brace-depth walk: depth returns to 0 at
2682). Scanning that whole range for `calculate_window_pct_delta` yields exactly
two hits:

| Line | Kind | Inside the block? |
|------|------|-------------------|
| 2404 | the call | yes |
| 2484 | prose — `// This validates that the calculate_window_pct_delta function implements` | yes (comment, not a call) |

So there is **one** call in the test and it is inside the block. The 2484 hit is
a comment, and it also falls inside 2393-2516, so it is not a stray outside-the-
block call either.

The nearest calls outside the test are at 2130 (before, in a different test) and
2814 (after, in a different test) — both well outside 2249-2682 and therefore
not "in the same test".
