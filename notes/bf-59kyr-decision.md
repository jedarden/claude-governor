# Decision: ARMOR ADR-005 Part-Size Validation Fix Plan

## Task Context

Child 3 of 5 split from bf-4neof. Goal: Locate ARMOR part-size validation and produce a decided fix plan for ADR-005.

## File/Function References

### Validation Code Location

**Primary validation in UploadPart:**
- File: `/home/coding/ARMOR/internal/server/handlers/handlers.go`
- Lines: 2271-2274
- Function: `UploadPart`
- Code:
```go
if plaintextSize > 0 && !pinningP && !presumedFinal && plaintextSize%int64(state.BlockSize) != 0 {
    h.writeError(w, "InvalidPartSize",
        fmt.Sprintf("Part size %d is not a multiple of the block size (%d bytes). ARMOR's uniform-part-size contract (ADR-005) requires block-aligned parts. Use a part size that's a multiple of %d (e.g., 5,242,880 for 5MiB, 16,777,216 for 16MiB).", plaintextSize, state.BlockSize, state.BlockSize), 400)
    return
}
```

**Secondary validation in CompleteMultipartUpload:**
- File: `/home/coding/ARMOR/internal/server/handlers/handlers.go`
- Lines: 2556-2560
- Function: `CompleteMultipartUpload`
- Code:
```go
if len(completeReq.Parts) > 1 && P%int64(state.BlockSize) != 0 {
    h.writeError(w, "InvalidPartSize",
        fmt.Sprintf("Uniform part size %d is not a multiple of the block size (%d bytes), which is only valid for a single-part upload, but this upload has %d parts. %s", P, state.BlockSize, len(completeReq.Parts), multipartRetryMessage), 400)
    return
}
```

### ADR-005 Location

- File: `/home/coding/ARMOR/docs/adr/005-out-of-order-multipart-uniform-part-size.md`
- Status: Implemented (amended 2026-08-07 for single-part alignment exemption)

## WHY the Alignment Invariant Exists

### Cryptographic Requirement

ARMOR uses AES-256 in CTR mode for encryption. The CTR counter offset for part N in a multipart upload is calculated as:

```
offset(N) = (N-1) × P / BlockSize
```

Where:
- P = uniform part size (pinned from part 1)
- BlockSize = 65536 bytes (64 KiB)
- N = part number (1-indexed)

### The Alignment Contract

For this offset calculation to work correctly:

1. **Every part that another part follows must be block-aligned**
   - If P is not a multiple of BlockSize, then part 2+ would start mid-block
   - This would corrupt the CTR keystream and produce incorrect HMAC indices
   - The result: silent data corruption or unreadable ciphertext

2. **The final part has no such requirement**
   - Nothing is placed after the final part
   - Its partial trailing block is exactly what any single PUT of arbitrary size produces

3. **Part 1 has no such requirement**
   - Part 1 always starts at block 0: `(1-1) × P / BlockSize = 0`
   - For any P, part 1's ciphertext and HMAC indices are correct regardless of size

### What Breaks Without Alignment

If a non-block-aligned regular part were accepted:

1. **CTR mode corruption**: The next part would start at a mid-block keystream position
2. **HMAC table corruption**: Per-block HMAC indices would be misaligned with actual block boundaries
3. **Silent data loss**: The encrypted object would decrypt to garbage

This is exactly the class of bug that ADR-002/ADR-003/ADR-005 fought previously. The alignment invariant is a **correctness requirement**, not an arbitrary restriction.

## The Three Candidate Fixes

### Option (a): Relax ADR-005 to Accept Short Final Part ✅ **CHOSEN**

**What changes:**
- Weaken the alignment invariant ONLY for parts that nothing follows:
  - Part 1 (always at block 0)
  - Presumed-final part (size < P)
- Keep full alignment enforcement for all regular parts (parts that another part follows)
- Allow single-part uploads of arbitrary size (already implemented)

**How existing objects stay readable:**
- **No format change**: On-disk object layout, HMAC table structure, and encryption scheme unchanged
- **No metadata change**: No new fields needed
- **Backward compatible**: Existing objects with aligned parts continue to work exactly as before
- **Forward compatible**: New objects with unaligned part-1/final-part decrypt correctly

**Effect on Range reads and erasure/stripe layer:**
- **Range reads**: No change needed. Part 1 starts at block 0 regardless of size; final part has no follower
- **Erasure coding**: No effect (ARMOR does not use erasure coding)
- **Striping**: No effect (ARMOR concatenates parts directly)

**Whether stored metadata needs new field for true length:**
- **No new field needed**. PlaintextSize is already tracked in ARMORMetadata
- For single-part uploads, the single part's size IS the true object length
- For multi-part uploads with short final part, the sum of all part sizes is the true length

**Rationale:**
1. **Already implemented**: ADR-011 (2026-08-07) amended ADR-005 with exactly these exemptions
2. **Correct reading of the contract**: The exemptions are not a concession—they're the correct interpretation
3. **Standard S3 semantics**: Short final parts are normal and expected in S3 clients
4. **No weakening for regular parts**: Full alignment enforcement stays in place for parts that need it
5. **Resolves immediate outage**: Barman backups work with single-part configuration (`--min-chunk-size` large enough)

**Rejected options' reasons:**
- **Option (b)** (zero-padding): Adds complexity, needs new metadata field, risks confusion about true length
- **Option (c)** (different endpoint): Abandons ARMOR's encryption for these backups, contradicts operator direction from ADR-011

### Option (b): Zero-Pad the Final Part Server-Side ❌ **REJECTED**

**What changes:**
- Accept short final part at upload time
- Pad with zeros to BlockSize boundary before encryption
- Store true object length in new ARMORMetadata field
- GET/HEAD/Range return unpadded bytes

**Why rejected:**
1. **Complexity**: Requires buffering and padding logic
2. **New metadata field**: Adds "TrueContentLength" or similar to track padding
3. **Confusion risk**: GetObject length differs from on-disk encrypted length
4. **Unnecessary**: Option (a) achieves the same client compatibility with zero changes
5. **Security consideration**: Zero-padding of sensitive data may have cryptographic implications

### Option (c): Route to Different S3 Endpoint ❌ **REJECTED**

**What changes:**
- Point forgejo/queue-db CNPG backups at a non-ARMOR S3 endpoint (e.g., Garage)
- Keep ARMOR/B2 as secondary off-site copy

**Alternative endpoint:** Garage (already used by apexalgo-iad/ardenone-cluster)

**Why rejected:**
1. **Operator direction**: ADR-011 explicitly states "barman backups stay on ARMOR" by operator decision
2. **Loses ARMOR encryption**: Backups would travel unencrypted to the alternative endpoint
3. **Two-backend complexity**: Adds operational overhead for these clusters
4. **Unnecessary**: Option (a) fixes the issue without changing backup topology
5. **ADR-011 superseded ADR-010**: The Garage route was the original ADR-010 decision, which was reversed

## Critical Discovery from Dependency Bead bf-3i9nr

The probe script (`~/ARMOR/scripts/probe-armor-multipart-alignment.py`) revealed:

**"ARMOR multipart uploads are completely broken (InternalError/InvalidPart), not just alignment-validated."**

This means:
1. The exemptions from ADR-011 may not be deployed yet
2. There is a broader multipart upload bug beyond alignment validation
3. PutObject works correctly (WAL archiving unaffected)
4. The alignment fix is necessary but not sufficient

## Acceptance Criteria for This Decision

All criteria met:
- ✅ File/function references provided (handlers.go:2271-2274, handlers.go:2556-2560, ADR-005)
- ✅ Written explanation of WHY alignment invariant exists (CTR offset calculation)
- ✅ One option explicitly chosen: Option (a) - relax ADR-005 with part-1/final-part exemptions
- ✅ Rationale provided: already implemented, correct interpretation, standard S3 semantics
- ✅ Rejected options' reasons documented: (b) adds unnecessary complexity, (c) contradicts operator direction
- ✅ Option (c) alternative endpoint named: Garage (rejected per ADR-1)

## Implementation Status

**Code**: The exemptions were implemented in ARMOR commit `fc6c1a86` (2026-08-07)
**Deployment**: ADR-011 notes "committed but not yet confirmed in production — the deployed image predates it"
**Additional bug**: bf-3i9nr probe found broader multipart upload failure requiring investigation

## Recommendation

Proceed with child 4 (pure execution) to:
1. Verify the ADR-011 exemptions are deployed and functional
2. Investigate the broader multipart upload failure discovered by bf-3i9nr's probe
3. Confirm fix with re-running the probe after deployment
4. Fix the `--min-chunk-size=5G` invalid syntax (from bf-3w5qp)
5. Verify CNPG backups reach phase=completed end-to-end (from bf-1lhi0)

---

**Decision recorded**: 2026-08-07
**Bead**: bf-59kyr
**Status**: Ready for child 4 (execution)
