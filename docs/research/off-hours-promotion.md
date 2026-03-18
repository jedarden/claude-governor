# Claude March 2026 Off-Peak Promotion — Research

## 1. What Is the Promotion?

Anthropic is running a limited-time **bonus usage promotion** for March 2026 that doubles Claude's usage limits during **off-peak hours**. The bonus usage is **additive** — it does not borrow from or count against existing weekly limits. During the off-peak window, Anthropic provides a second, parallel usage bucket of equal size to the normal one.

This is a capacity-management incentive: shifting load from busy daytime hours toward evenings, nights, and weekends when Anthropic's infrastructure has headroom.

**No activation required** — automatic for eligible plans.

---

## 2. Promotion Duration

- **Start:** March 13, 2026
- **End:** March 28, 2026 (ends at 11:59 PM PT)
- **Length:** 15 days

The local `capacity-governor.sh` on this server encodes `PROMO_START="2026-03-13"` and `PROMO_END="2026-03-27"`.

---

## 3. Peak vs. Off-Peak Hours

**Peak window:** 8 AM – 2 PM US Eastern Time (weekdays only)

**Weekends are 2x all day** with no peak window.

| Timezone | Peak Hours (1x) | Off-Peak (2x) |
|---|---|---|
| ET (US East) | 8 AM – 2 PM | 2 PM – 8 AM next day |
| PT (US West) | 5 AM – 11 AM | 11 AM – 5 AM next day |
| GMT (UK) | 1 PM – 7 PM | 7 PM – 1 PM next day |
| CET (Central Europe) | 2 PM – 8 PM | 8 PM – 2 PM next day |
| UTC | 1 PM – 7 PM | 7 PM – 1 PM next day |

---

## 4. How 2x Affects Effective Capacity

**The usage limit doubles during off-peak hours** — the ceiling is raised, not the per-token cost reduced.

- **During peak:** Normal limits apply. 1 token = 1 token toward your weekly budget.
- **During off-peak:** Your usage limit is doubled. Bonus usage is a separate parallel bucket.
- This is NOT "tokens cost 0.5x." It is "you get a second equal allocation on top of your first."

**Scheduling interpretation (used in capacity-governor.sh):**
```
effective_hours = peak_hours * 1.0 + offpeak_hours * 2.0
```
An off-peak hour is worth 2x a peak hour in capacity scheduling because you can do 2x the work in the same wall-clock time without hitting limits.

---

## 5. Interaction With Weekly/Monthly Reset Cycles

- Bonus off-peak usage is **entirely separate** from weekly limits. It does not reduce the weekly pool.
- After the promotion ends (March 28), remaining bonus capacity is lost — no carryover.
- Normal weekly limits reset 7 days after your session starts (rolling window, not calendar-based).
- The 5-hour rolling session window still applies; the ceiling doubles during off-peak hours.
- Weekly limit + 5-hour burst limit are both doubled during off-peak.

---

## 6. Eligible Plans

| Plan | Eligible |
|---|---|
| Free | Yes |
| Pro ($20/mo) | Yes |
| Max 5x ($100/mo) | Yes |
| Max 20x ($200/mo) | Yes |
| Teams Standard | Yes |
| Teams Premium | Yes |
| Enterprise | **No** (explicitly excluded) |

---

## 7. Which Models and Surfaces

The promotion applies to all Claude models on eligible plans and all surfaces (Claude web, desktop, mobile, Claude Code, Cowork, Claude for Excel, Claude for PowerPoint). Not Sonnet-only — includes Opus on Max plans.

---

## 8. Claude Code Plan Tiers and Usage Structure

### Plan Tiers

| Plan | Price | Throughput | Models |
|---|---|---|---|
| Free | $0 | ~40 short messages/day | Sonnet (limited) |
| Pro | $20/month | ~45 prompts per 5-hour window | Sonnet |
| Max 5x | $100/month | ~5x Pro throughput | Sonnet + Opus |
| Max 20x | $200/month | ~20x Pro throughput | Sonnet + Opus (1M context) |
| Teams Standard | Per-seat | ~1.25x Pro per session | Sonnet + Opus |
| Teams Premium | Higher per-seat | ~6.25x Pro per session | Sonnet + Opus |
| Enterprise | Custom | Custom | All models |

### How Usage is Measured

Usage is **token-based**, not message-count-based. Every file attachment, tool definition, conversation history, and context window contribution is tokenized and counted. The "prompts per window" figures are rough empirical guides — actual consumption depends on token volume per turn.

### Reset Cycles (Two Parallel Limits)

1. **5-Hour Rolling Window:** Governs burst rate. Starts from first message; resets 5 hours later.
2. **Weekly Limit:** Cumulative cap that resets 7 days after your session starts (rolling, not calendar). Both Claude Code and Claude web/mobile draw from the same pool.

---

## 9. Official Sources

- [Claude March 2026 Usage Promotion | Claude Help Center](https://support.claude.com/en/articles/14063676-claude-march-2026-usage-promotion)
- [Anthropic Is Giving Free And Pro Users 2x More Claude This Month — Dataconomy](https://dataconomy.com/2026/03/16/anthropic-is-giving-free-and-pro-users-2x-more-claude-this-month/)
- [Anthropic is Doubling Claude's Usage Limits During Off-Peak Hours — Engadget](https://www.engadget.com/ai/anthropic-is-doubling-claudes-usage-limits-during-off-peak-hours-for-the-next-two-weeks-163645928.html)
- [Claude's 2x Usage Boost Is Live — DEV Community](https://dev.to/sivarampg/claudes-2x-usage-boost-is-live-heres-how-to-maximize-it-march-13-28-2026-31d0)

---

## 10. Implications for Governor Design

A governor that is aware of the off-peak promotion can:

1. **Run more workers during off-peak hours** — double the burn rate is acceptable
2. **Save workers during peak hours** — be conservative to preserve daily budget
3. **Compute effective remaining capacity correctly:**
   ```
   if off_peak:
       effective_remaining = normal_remaining * 2
   else:
       effective_remaining = normal_remaining
   ```
4. **Model effective hours until reset:**
   ```
   effective_hours = sum over each future hour: (2.0 if off_peak else 1.0)
   ```

After March 28, the governor should fall back to a 1x flat model unless a new promotion is detected.
