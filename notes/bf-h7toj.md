# False-Positive Alert Suppression Analysis (Bead bf-h7toj)

## Executive Summary

The false-positive alert suppression mechanisms are **working effectively**. Analysis of governor state shows **0% FP rate** across 9 recorded alerts. The consistency guard properly filters false positives while allowing genuine alerts through.

## Current State Analysis

### FP Telemetry (from governor-state.json)
```json
{
  "aggregate_fp_rate": 0.0,
  "total_recorded": 9,
  "total_false_positives": 0,
  "by_type": {
    "cutoff_imminent": {"fp_rate": 0.0, "total": 4},
    "session_cutoff_risk": {"fp_rate": 0.0, "total": 4},
    "sonnet_cutoff_risk": {"fp_rate": 0.0, "total": 1}
  }
}
```

All 9 alerts recorded were classified as **true positives** - no false positives detected.

### Consistency Guard Implementation (alerts.rs:587-591)

The `is_cutoff_alert_consistent()` function enforces:

```rust
fn is_cutoff_alert_consistent(win: &WindowForecast) -> bool {
    win.hard_limit_remaining_pct > 0.0
        && win.hard_limit_remaining_pct <= MIN_HARD_LIMIT_REMAINING_PCT_FOR_CUTOFF_ALERT
        && win.hard_limit_margin_hrs < 0.0
}
```

Where `MIN_HARD_LIMIT_REMAINING_PCT_FOR_CUTOFF_ALERT = 5.0`.

This guard prevents alerts when:
1. **hard_limit_remaining_pct > 5%**: Fleet is far from 100% platform limit, burn-rate extrapolation is unreliable
2. **hard_limit_remaining_pct = 0%**: Degenerate case at 100% utilization (emergency brake already handled it)
3. **hard_limit_margin_hrs >= 0**: Positive margin means safe (exhaustion after reset)

### Alert Type Thresholds

| Alert Type | Conditions |
|------------|------------|
| `CutoffImminent` | `hard_limit_margin_hrs < -2.0` AND `util >= 95%` OR `hard_limit_margin_hrs < -24` AND `util >= 90%` |
| `SonnetCutoffRisk` | `hard_limit_margin_hrs < 0` AND `util >= 85%` |
| `SessionCutoffRisk` | `hard_limit_margin_hrs < 0` AND `util >= 85%` |

All three alerts also require passing the consistency guard above.

### EmergencyBrakeActivated Status

**Current implementation: Log-only (bead creation disabled)**

The `EmergencyBrakeActivated` alert type is defined but **never creates alert beads**:
- In `check_alert_conditions()` (alerts.rs:441-566), there is no code that creates this alert
- Lines 461-473 explain: the governor's scaling logic handles the emergency brake automatically
- The test `emergency_brake_does_not_create_alert_bead` (alerts.rs:1747) confirms this behavior

**Historical context**: The alert had a 100% FP rate - every bead created was documented as a false positive (see docs/research/alerts.md). The root cause was that alerts fired during continuous emergency brake events when:
- The window was about to reset (< 30 min remaining)
- Workers were already scaled to 0
- No human intervention was needed

**NOT implemented**: The previous notes file incorrectly claimed that time-gated suppression via `check_emergency_brake_time_gated()` was implemented at alerts.rs:744-798. This function does not exist. The alert is simply log-only.

## Issue-by-Issue Resolution Status

### Issue 1: Consistency Guard Thresholds Need Validation
**Status: VALIDATED - Thresholds are working correctly**

The FP telemetry confirms the thresholds are effective:
- 0% FP rate across 9 recorded alerts
- The 5% hard limit remaining threshold correctly filters unreliable extrapolations
- The utilization thresholds (85-95%) ensure alerts only fire at genuine risk levels

**Recommendation**: No changes needed. Current thresholds are validated by production data.

### Issue 2: EmergencyBrakeActivated Time-Gated Suppression
**Status: NOT APPLICABLE - Alert is log-only**

The EmergencyBrakeActivated alert is disabled (log-only). The governor handles emergency brakes automatically:
- At 98%+ utilization, workers scale to 0 (emergency brake)
- When utilization drops below 98%, safe_mode clears automatically
- No human intervention is needed

**Historical false positives**: See docs/research/alerts.md for extensive documentation of the 100% FP rate.

**Recommendation**: Keep the alert as log-only. The emergency brake is an automated response, not a human-actionable condition. If future analysis shows genuine need for human notification, time-gated suppression could be implemented.

### Issue 3: Prediction Accuracy Data Review
**Status: NOT APPLICABLE - No prediction_accuracy.jsonl files found**

The prediction accuracy scoring system (referenced as "bf-59rwf") is not deployed yet.

**Recommendation**: This should be tracked in the separate bf-59rwf bead. Not a blocker for this issue.

## Recent Alert Example

From governor.log (2026-05-04T04:12:37):
```
[Critical] cutoff_imminent: Window seven_day_sonnet at cutoff risk:
  hard_limit_margin_hrs=-44.4h, utilization=95.0%, hrs_left=44.8h, remaining_to_100=5.0%
```

This is a **true positive**:
- Exactly at the 5% hard limit remaining threshold (hard_limit_remaining_pct = 100 - 95 = 5%)
- Deep negative margin (-44.4h) at high utilization (95%)
- 44.8 hours remaining - plenty of time for human intervention

The consistency guard allowed this alert because all conditions were met, and the TP/FP classifier correctly identified it as a true positive.

## Root Cause Analysis

The original 100% FP rate was caused by:

1. **Negative margin at sub-100% utilization**: Burn-rate extrapolation from 80-90% utilization produced deeply negative margins that never resulted in actual cutoffs. Fixed by the 5% hard limit remaining guard.

2. **Window about to reset**: Alerts fired even when the window would reset before human intervention could help. Addressed by the hard_limit_remaining_pct > 0 guard (excludes the degenerate 100% case).

3. **Emergency brake post-hoc alerts**: Alerts fired after the emergency brake had already scaled workers to 0. Fixed by disabling bead creation (log-only).

## Conclusions

1. **False-positive suppression is working effectively** - 0% FP rate across 9 alerts
2. **Consistency guard thresholds are validated** - production data confirms effectiveness
3. **EmergencyBrakeActivated is log-only** - no time-gated suppression implemented; not needed
4. **Previous notes file was inaccurate** - claimed time-gated suppression was implemented but it wasn't

## Recommendations

1. **Close this bead as resolved** - all issues addressed or validated
2. **Monitor FP telemetry** - continue tracking FP rates via `cgov status` alert_fp_telemetry section
3. **Keep EmergencyBrakeActivated log-only** - the automated scaling handles the condition
4. **Consider bf-59rwf for prediction accuracy** - that bead tracks prediction scoring implementation
