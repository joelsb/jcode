# 0021 - Memory confidence decays only on a real judge verdict

Status: Accepted
Date: 2026-08-16

## Decision

`post_retrieval_maintenance` may decay a memory's confidence **only when an LLM
judge actually returned a verdict that turn**. When no verdict exists, the
rejected set is discarded rather than applied. The gate is `decay_targets()` in
`crates/jcode-base/src/memory_agent.rs`, fed by `RetrievalContext::judge_ran`,
which is set true on exactly one path: `RerankOutcome::Judged`.

Boosting is deliberately left ungated. A boost cannot destroy data, and it only
happens for memories that were actually surfaced.

## What forced it

`rejected_ids` is computed by subtraction:

```rust
let rejected_ids = candidate_ids.iter().filter(|id| !verified_ids.contains(id))
```

That expression cannot distinguish two very different facts:

1. the judge read this memory and ruled it irrelevant;
2. no judge ran, so nothing was surfaced and every candidate fell through.

On a judge failure the rerank surfaces nothing by design (it refuses to inject
unvetted hybrid order). So `verified_ids` is empty and **every candidate is
labelled rejected**. Maintenance then decayed all of them, at 0.02 per turn,
and `prune_low_confidence` deletes anything under 0.15 that is older than 24h.

This is not hypothetical. Between 2026-08-04 and 2026-08-15 the sidecar on this
machine was pinned to a model id that no longer exists,
`claude-haiku-4-5-20241022` (the live id is `claude-haiku-4-5-20251001`). Every
periodic extraction 404'd - 67 failures across 9 days, each `extraction_started`
followed within ~300ms by an `error` event in `~/.jcode/logs/memory-events-*.jsonl`.

At `2026-08-10T09:35:34` the logs show the whole chain in four consecutive events:

```
judge_decision         {"decision":"all_judges_failed","degradation":true,"no_llm":true}
maintenance_started    {"rejected":3,"verified":0}
maintenance_confidence {"boosted":0,"decayed":3}
maintenance_complete   {"latency_ms":40}
```

`~/.jcode/memory/global.json` has that exact mtime and went from 2 memories to
0. The surviving `global.bak` entries both carry `confidence: 0.0` - the decay
signature. Nothing was reported to the user; memory simply stopped existing.

## Consequences

- An outage now costs recall for its duration and nothing more. Memory goes
  dormant instead of self-destructing.
- Genuinely stale memories survive longer when the judge is flaky, because the
  only thing that can demote them is a working judge. Accepted: false retention
  is recoverable, deletion is not.
- `judge_ran` must be threaded to any future call site that decays or prunes.
  The doc comment on the field says so; the tests below enforce it.

## Rejected alternatives

- **Decay by a smaller amount on failure.** Still monotonic toward deletion,
  just slower. 67 failures at any positive rate reaches zero.
- **Stop pruning entirely.** Throws away the working garbage-collection path to
  fix a signal problem, and lets low-value memories accumulate forever.
- **Treat a failed judge as verifying everything (boost instead).** Equally
  wrong in the opposite direction; invents evidence that does not exist.
- **Fix only the model id.** Removes this trigger, not the class. Any future
  transport error, expired login or provider outage re-arms the same wipe.

## What would make us revisit

Evidence that ungated decay is needed for precision, or a redesign where
retrieval reports rejection explicitly instead of by subtraction. If
`rejected_ids` ever becomes a positive signal produced by the judge itself,
this gate becomes redundant and should be removed with its tests.

## Evidence

- `crates/jcode-base/src/memory_agent_tests.rs`:
  - `decay_targets_empty_when_judge_did_not_run`
  - `decay_targets_pass_through_on_real_verdict`
  - `repeated_judge_failures_never_prune_the_store` - replays all 67 failures
    against a 72h-old memory and asserts confidence is untouched and nothing
    is pruned.
- Meta-test: reverting `decay_targets` to the pre-fix body
  (`rejected_ids` unconditionally) fails the suite with
  `left: Some(0.0), right: Some(0.5)`, reproducing the observed wipe exactly.
  A guard that has never failed proves nothing, so this was checked by hand
  before the fix was accepted.
- Logs: `~/.jcode/logs/memory-events-2026-08-{04,05,06,07,10,11,12,14,15}.jsonl`.
