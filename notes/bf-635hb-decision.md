# ARMOR Part-Size Fix Decision — bf-635hb

**Date:** 2026-08-07
**Status:** DECISION
**Related ADRs:** ADR-005, ADR-010, ADR-011

## Executive Summary

**DECISION: Option (a) — Relax ADR-005 to accept a short FINAL part (standard S3 semantics)**

This option has **already been implemented** in the ADR-005 amendment (2026-08-07) and is currently deployed in ARMOR version 0.1.1923 on iad-ci. The immediate fix for barman backups is configuration-only: set a valid `--min-chunk-size` that forces single-part uploads within the exemption's scope.

## Context

### The Problem

Barman-cloud-backup of CNPG PostgreSQL clusters (`queue-db`, `forgejo-postgres`) has 100% backup failure rate because:
1. Current config uses invalid size string `--min-chunk-size=5G` (barman rejects before any work)
2. Even with a valid size, barman produces multipart parts that are not multiples of ARMOR's 64 KiB encryption block size
3. ARMOR's ADR-005 uniform-part-size contract requires block-aligned parts

### Why Alignment Exists

The alignment invariant is rooted in **CTR mode encryption offset arithmetic**:
- Each part's CTR counter offset is computed as: `(partNumber - 1) × partSize / blockSize`
- This formula requires `partSize` to be on a block boundary for correct offset placement
- Misaligned parts produce incorrect HMAC indices and corrupt ciphertext

This is documented in ADR-003 (multipart layout) and ADR-005 (uniform-part-size contract).

### The Amendment (Already Deployed)

ADR-005 amendment (2026-08-07, commit `1f6e6934`, version 0.1.1901+) exempts two cases from alignment:
1. **Part 1**: Always starts at block 0, so any size is correct for encryption offset
2. **Presumed-final part**: A part smaller than pinned `P` — nothing follows it, so no offset arithmetic needed

**Validation code location**: `internal/server/handlers/handlers.go:2269-2275`

```go
pinningP := state.PartSize == 0
presumedFinal := state.PartSize != 0 && plaintextSize < state.PartSize
if plaintextSize > 0 && !pinningP && !presumedFinal && plaintextSize%int64(state.BlockSize) != 0 {
    h.writeError(w, "InvalidPartSize", ...)
    return
}
```

## Options Evaluated

### Option (a): Relax ADR-005 to accept short final part ✅ **CHOSEN**

**Status**: Already implemented and deployed

**What it does**:
- Part 1 can be any size (it starts at block 0 regardless)
- A presumed-final part (size < pinned P) can be any size
- Only intermediate parts (that something is placed after) require block alignment

**Why this is correct**:
- Alignment exists solely to keep the `(N-1)×P/blockSize` offset on a block boundary
- Part 1 always starts at block 0: `(1-1)×P/blockSize = 0` for any `P`
- A final part has nothing after it, so its partial trailing block is irrelevant for offset arithmetic
- This is standard S3 semantics — every other S3 implementation accepts short final parts

**Invariant weakened**: None. The alignment invariant still holds for all parts that actually need it (parts that another part is placed after).

**Impact on existing objects**: None. The on-B2 format is unchanged. Objects encrypted with the old strict rules remain readable.

**Range reads**: No changes needed. Range reads already handle partial blocks at object start/end — a short final part is just another partial block.

**Erasure/stripe layer**: No changes needed. There is no erasure or striping layer at the encryption level.

**Metadata changes**: None needed. The `x-amz-meta-armor-multipart` marker and sidecar HMAC table work identically.

**Implementation**: Complete and deployed in ARMOR 0.1.1901+ (iad-ci running 0.1.1923).

---

### Option (b): Zero-pad final part server-side ❌ **REJECTED**

**What it would do**: Pad the final part to block size with zero bytes, store true object length separately

**Why rejected**:
1. **Unnecessary complexity**: Option (a) already solves the problem without padding
2. **Metadata bloat**: Requires a new field to store true length (adds complexity to GET/HEAD/Range)
3. **Storage waste**: Every object wastes up to 64 KiB on padding
4. **Confusing semantics**: GET would return padded bytes unless length field is consulted; Range would need offset adjustment
5. **No benefit**: Padding doesn't improve encryption correctness or security

**Invariant weakened**: None (would be preserved by padding), but adds implementation complexity.

**Metadata changes needed**: New field in `x-amz-meta-armor-*` namespace for true object length.

**Migration needed**: No — only affects new objects.

---

### Option (c): Point backups at different S3 endpoint ❌ **REJECTED**

**What it would do**: Route CNPG backups to a non-ARMOR S3 endpoint (e.g., Garage, B2 direct, MinIO)

**Why rejected**:
1. **Topology fragmentation**: Adds a second backend for these two backups only
2. **Operational overhead**: Two sets of credentials, two monitoring dashboards, two failure modes
3. **No actual fix**: Doesn't address the underlying incompatibility; just moves it elsewhere
4. **Operator direction rejected**: ADR-011 explicitly reversed ADR-010's Garage reroute decision
5. **Size ceiling returns**: Even on another endpoint, barman's non-aligned parts would eventually hit the same issue if that endpoint has alignment requirements

**Alternative endpoints available**:
- Garage (exists on `apexalgo-iad`/`ardenone-cluster` as ARMOR secondary backend per ADR-006)
- B2 direct (via native B2 API, not S3-compatible)
- MinIO (could be deployed, but adds infrastructure)

**None chosen**: Barman backups stay on ARMOR per ADR-011.

---

## Implementation Plan for Chosen Option

### Immediate Fix (Already Deployed)

The exemption is already in ARMOR 0.1.1901+ and deployed on iad-ci (0.1.1923). No code changes needed.

### Configuration Fix Required

**Invalid current config** (both clusters):
```yaml
spec.backup.barmanObjectStore.data.additionalCommandArgs: ["--min-chunk-size=5G"]
```

**Corrected config** (two-step fix):

1. **Fix the invalid size string** and set it to force single-part uploads:
   ```yaml
   # queue-db: ~30 MB database
   spec.backup.barmanObjectStore.data.additionalCommandArgs: ["--min-chunk-size=100MB"]

   # forgejo-postgres: ~63 MB database
   spec.backup.barmanObjectStore.data.additionalCommandArgs: ["--min-chunk-size=100MB"]
   ```

2. **Verify with probe**:
   ```bash
   # Run ARMOR multipart alignment probe
   cd /home/coding/ARMOR
   ./scripts/probe-multipart-alignment.sh
   ```

**Rationale for 100MB**:
- Both databases fit well under 100MB in single part
- 100MB >> 5 MiB S3 minimum, so barman won't try to split
- Well below S3's 5 GiB per-part ceiling
- Single part falls under the ADR-005 exemption for part 1

### Files to Change

1. **declarative-config repo**:
   - `k8s/iad-ci/forgejo/cnpg-cluster.yaml`: Fix `--min-chunk-size`
   - `k8s/iad-ci/queue-db/cnpg-cluster.yaml`: Fix `--min-chunk-size`

2. **Verification**:
   - Watch for `ScheduledBackup` objects to reach `phase: completed`
   - Check barman pod logs for successful backup completion
   - Verify via ARMOR multipart canary (probes run automatically)

### Future Work (Beyond This Bead)

Per ADR-011, when databases grow beyond single-part size:

**Non-uniform multipart support** (not yet implemented):
1. **Per-part offsets from cumulative sizes**: Track running offset instead of `(N-1)×P`
2. **CTR seek to arbitrary byte offset**: Encryption must handle mid-block starting positions
3. **Boundary-block HMAC backfill**: Re-read boundary blocks at CompleteMultipartUpload to compute HMACs

**Alternative** (if boundary-block backfill proves too complex):
- Patch barman-cloud-backup to pad parts to 64 KiB boundaries before upload
- Deploy patched barman in CNPG image

**Note**: Given ARMOR's history of silent multipart corruption (ADR-002/003/005), non-uniform support must ship with adversarial tests covering mid-block boundaries, out-of-order arrival, retried parts, and end-to-end byte verification.

## Verification

**Acceptance criteria from parent bead bf-4neof**:
- ✅ Part 1 exemption implemented and deployed (ARMOR 0.1.1901+)
- ✅ Final part exemption implemented and deployed
- ⏳ Configuration fix to declarative-config (next bead)
- ⏳ Backup reaches `phase: completed` (verification)
- ⏳ Probe shows short final part accepted (verification)

## References

- ADR-005: `docs/adr/005-out-of-order-multipart-uniform-part-size.md` (amendment 2026-08-07)
- ADR-011: `docs/adr/011-barman-stays-on-armor-non-uniform-multipart.md`
- Validation code: `internal/server/handlers/handlers.go:2269-2275`
- Exemption commit: `1f6e6934` (2026-08-06 22:55:07 -0400, version 0.1.1901)
- Deployed version: ARMOR 0.1.1923 on iad-ci (pod `armor-8947648f9-lmh6p`)

## Summary

**Option (a) is chosen because**:
1. It's already implemented and deployed
2. It's the correct reading of the alignment contract (alignment only needed where something is placed after)
3. It matches standard S3 semantics
4. It requires no code changes, only configuration
5. It preserves the encryption invariant for all parts that actually need it

**Options (b) and (c) are rejected** because:
- (b) adds unnecessary complexity and storage waste for a problem already solved
- (c) fragments topology and was explicitly rejected by operator direction in ADR-011

**Next steps** (in child beads):
- Fix declarative-config `--min-chunk-size` values
- Verify backups reach `completed` phase
- Monitor for when databases exceed single-part size (non-uniform support needed)
