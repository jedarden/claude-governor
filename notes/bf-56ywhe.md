# OAuth Token Refresh Failures Investigation (bf-56ywhe)

**Investigated**: 2026-08-03
**Issue**: Recurring OAuth token-refresh failures, never root-caused

## Current State (2026-08-03)

Credentials file is **completely empty**:
```json
{
  "claudeAiOauth": {
    "accessToken": "",
    "refreshToken": "",
    "expiresAt": 0,
    ...
  }
}
```

- **File last modified**: 2026-08-02 02:21:48
- **cgov doctor status**: "Token has expired" - needs `claude login`
- **Last alert**: 2026-08-02T08:13:18Z (shortly before file was cleared)

## Historical Pattern from Telemetry

From `~/.config/claude-governor/governor-state.json` alert_fp_telemetry:

| Date | Alerts (all confirmed true-positive) |
|------|-------------------------------------|
| 2026-07-28 | 5 alerts (02:17, 10:29, 11:30, 12:31, 13:32 UTC) |
| 2026-07-30 | 2 alerts (10:12, 11:13 UTC) |
| 2026-08-01 | 11+ alerts (02:57 → 21:32, roughly hourly) |
| 2026-08-02 | Multiple alerts ending at 08:13 |

**Pattern**: Alerts occur in clusters, roughly 1 hour apart during each cluster. The 2026-08-01 cluster was particularly intense (11+ alerts over ~19 hours).

## Detection Mechanism

From `src/governor.rs:4376`:
```rust
state.token_refresh_failing = usage_data.stale;
```

From `src/poller.rs:573-594`:
- When `get_access_token()` fails due to token refresh
- AND previous usage data exists (fallback)
- Sets `usage_data.stale = true`
- Governor sets `token_refresh_failing = true`
- Alert fires: "OAuth token refresh failing — Claude Code sessions may be unable to make API calls. Run: claude login"

## Code Vulnerabilities

### 1. Non-atomic credential writes (src/poller.rs:391-399)
```rust
fn write_credentials(&self, creds: &Credentials) -> Result<()> {
    let content = serde_json::to_string_pretty(creds)?;
    fs::write(&self.credentials_path, content)  // ⚠️ NOT atomic
}
```

**Problem**: `fs::write()` is not atomic. If interrupted:
- Process crash during write
- Concurrent write from another process
- System interruption

Result: Partial/corrupted credentials file → empty tokens on next read.

### 2. No error detail capture (src/poller.rs:431-437)
```rust
if response.status() != 200 {
    let text = response.into_string().unwrap_or_default();
    return Err(PollerError::TokenRefreshFailed(
        format!("HTTP {}: {}", status, text)
    ));
}
```

**Captures only**: HTTP status + response body
**Missing**: Detailed diagnostics for:
- Rate limiting (429)
- Invalid grant (400/401)
- Network errors
- Response body parsing

### 3. Concurrent access risk

Multiple processes share `~/.claude/.credentials.json`:
- cgov daemon (reads/writes every poll cycle)
- Other Claude Code sessions (read/write)

No file locking or coordination → race conditions.

## Root Cause Analysis

### Primary Hypothesis: File corruption from interrupted writes

**Evidence**:
1. Credentials file currently has empty tokens
2. File was last modified shortly after the last alert cluster
3. No backup/temporary file mechanism exists

**Scenario**:
1. Token refresh begins (every 5 minutes when token expiring)
2. Reads credentials, gets new token from platform
3. **Write is interrupted** (crash, concurrent write, system issue)
4. File partially written → empty tokens
5. Next refresh attempt fails (empty refresh_token)
6. Alert fires
7. Loop repeats hourly until manual intervention (`claude login`)

### Secondary Hypothesis: Concurrent write race condition

**Scenario**:
1. cgov daemon writes credentials during refresh
2. Another Claude Code session writes simultaneously
3. Last write wins, but data is corrupted
4. Empty tokens result from race

### Timing pattern explanation

- **1-hour intervals**: cgov daemon polls every poll cycle
- **Clusters**: Once corrupted, retries fail repeatedly
- **Self-recovery**: Manual `claude login` or eventual token expiry window passes

## Why This Has Never Been Root-Caused

1. **Transient and self-recovering**: Once `claude login` is run, it works again
2. **No detailed error logging**: Only "HTTP XXX: <body>" captured
3. **No visibility into writes**: No audit trail of what writes credentials and when
4. **No backup/rollback**: Once corrupted, only fix is manual re-auth

## Recommendations

### Immediate (this release):
1. **Add detailed logging**: Log before/after credential writes with file hash
2. **Capture refresh endpoint details**: Log full request/response on failure
3. **Add write timestamp**: Track when credentials were last written

### Short-term (next release):
1. **Atomic writes**: Use temp file + atomic rename pattern
2. **File validation**: Verify written file matches expected structure
3. **Write concurrency protection**: File locking or retry-with-backoff

### Long-term:
1. **Credential monitoring**: Detect corruption early, alert before complete failure
2. **Credential backup**: Keep last-known-good copy for rollback
3. **Dedicated credential daemon**: Single writer, readers via IPC

## Conclusion

The root cause is likely **file corruption from non-atomic, concurrent credential writes**, compounded by:
- No error detail capture
- No validation after write
- No backup/rollback mechanism
- Multiple processes writing simultaneously

The pattern of recurring clusters that self-recover after `claude login` fits this hypothesis: once corrupted, the system fails repeatedly until manually fixed, then the cycle repeats when the next write interruption occurs.

## References

- Historical beads: docs-tsmx, docs-fgjx, docs-l3om, docs-kvhc, docs-e4mf (all closed with "token refresh was failing but has recovered")
- Code: `src/poller.rs:391-399` (write_credentials), `src/poller.rs:408-450` (refresh_token)
- State: `~/.config/claude-governor/governor-state.json` alert_fp_telemetry
- Credentials: `~/.claude/.credentials.json` (currently corrupted/empty)
