# 0022 - Memory retention decays from last use, and expiry retires before it deletes

Status: Accepted
Date: 2026-08-16

## Decision

Memory garbage collection scores each entry with `retention_score_at()`, a new
function that applies the existing per-category half-life to the time since
**`updated_at`** (last use), not since `created_at`. Entries scoring below
`PRUNE_CONFIDENCE_FLOOR` (0.15) are **retired** (`active = false`, kept on
disk). Only an entry that has been retired and untouched for
`RETIRED_GRACE_DAYS` (30) is deleted.

`effective_confidence()` keeps decaying from `created_at` and is unchanged. The
two answer different questions and must not be merged:

| Question | Function | Decays from |
|---|---|---|
| How much do I trust this claim? | `effective_confidence()` | `created_at` |
| Should I delete this? | `retention_score_at()` | `updated_at` |

## What forced it

ADR-0021 stopped confidence decaying without a judge verdict. That was correct
(it was destroying live data) but it also removed the only force that ever
moved confidence downward. New memories start at `confidence: 1.0`, decay is
0.02 per judge rejection and the prune floor is 0.15, so deletion required
**43 consecutive rejections with zero boosts**. In practice nothing was ever
deleted again: garbage collection had been riding on a broken signal, and
fixing the signal silently ended the collection.

Measured on this machine the same day: 35 memories, 32 written in one day,
bursts of up to 13 per session end, and no eviction path. Roughly 1,200/month.

The obvious fix - point the pruner at the `effective_confidence()` half-life
that already existed - is wrong, and the test written for it caught that before
it shipped:

```
gc_keeps_a_used_memory_beside_an_expired_one ... FAILED
  left: Retire
 right: Keep
```

Two 150-day-old facts, one with 400 accesses, were both retired.
`effective_confidence()` decays from `created_at`, so **a memory reinforced
every single day still expires on its birthday**. Its access-count term cannot
compensate, and this is arithmetic, not tuning:

```
access boost = 1 + 0.1*ln(n+1)   ->  max ~2.1x even at n = 100,000
age decay (fact, 150d)           ->  0.031x
```

A logarithmic boost can never offset exponential decay. Recency of use, not the
access tally, has to be what keeps a memory alive.

## Consequences

- An unused fact retires after ~4 half-lives (~120 days); a fact used last week
  scores near its raw confidence no matter how old it is. Corrections (365d
  half-life) effectively never expire unless abandoned for years.
- Reinforcement now has teeth: touching an entry resets its retention clock,
  which is what makes ADR-0022 and the shared write gate work together.
- Deletion is delayed by 30 days beyond retirement, so a wrong retirement is
  recoverable and leaves evidence on disk. Cost: retired entries occupy space
  and still appear in `list_all()`.
- This is the first mechanism in the system that deletes on a **timer** rather
  than in response to something a user or judge did. That is why it is
  two-stage and why it emits `maintenance_gc {retired, deleted}` on every
  non-zero sweep - deletion must never be silent.
- `prune_low_confidence` now returns `(retired, deleted)` instead of one count.

## Rejected alternatives

- **Point the pruner at `effective_confidence()`.** The obvious reading of
  "wire up the half-life that already exists". Evicts memories that are in
  active use; see the failing test above.
- **Make the access boost stronger.** Tuning a logarithmic term to beat an
  exponential one only moves the age at which useful memories die.
- **Delete outright at the floor, no retirement.** Simpler, but makes a
  timer-driven, irreversible deletion the normal path one day after a
  data-loss bug in the same subsystem. Not worth the saved complexity.
- **Cap the store at N memories, evict oldest.** Ignores value entirely: a
  365-day correction would be evicted by a burst of trivia.
- **Keep the store append-only.** Viable today at 35 entries, but the judge's
  candidate pool degrades silently as it grows, which is the same failure
  shape that hid the ADR-0021 outage for nine days.

## What would make us revisit

Evidence that useful memories are being retired (check `maintenance_gc` counts
against complaints of forgotten context), or a retrieval design where a large
store costs nothing, which would make expiry unnecessary. If reinforcement ever
stops updating `updated_at`, this whole scheme silently reverts to age-based
expiry - that invariant is load-bearing.

## Evidence

- `crates/jcode-base/src/memory_agent_tests.rs`:
  - `gc_keeps_a_used_memory_beside_an_expired_one` - the test that rejected the
    first implementation
  - `gc_never_expires_a_memory_that_keeps_being_used` - 10 years old, used 2
    days ago, must be kept
  - `gc_retires_before_it_deletes`, `gc_reinforcement_rescues_a_retired_memory`
  - `gc_respects_category_half_lives`, `gc_keeps_everything_inside_the_age_floor`
  - `gc_sweep_retires_and_deletes_on_disk` - end-to-end over a real graph
  - `reinforcement_resets_the_gc_clock` - ties the write gate to this ADR
- Planted-defect runs (a guard that has never failed proves nothing):
  - retention decaying from `created_at` -> 4 tests fail
  - `Retire` replaced by `Delete` -> 9 tests fail
  - dedup disabled in the shared write gate -> 2 tests fail
