# ADR-0018: A rotation claims the shared account override only when it has better information

Status: Accepted
Date: 2026-08-12

Narrows the blast radius that [ADR-0009](0009-defer-per-session-account-identity.md) deferred. Does not resolve it.

## Context

ADR-0009 shipped account rotation on a process-global override and named the condition that would force a revisit:

> More than one interactive session routinely sharing a daemon on the same provider. Single-session-per-daemon usage keeps the defect theoretical; concurrent usage makes it a billing-attribution bug.

That condition arrived. The logs from the 2026-08-12 incident show **five sessions** (`badger`, `bear`, `buffalo`, `crab`, `duck`) on one daemon sharing two Anthropic accounts, producing 53 rate-limit errors in ten minutes and this pattern:

```
10:23  failover: retrying on account 'claude-1'
10:25  failover: retrying on account 'claude-2'
10:30  failover: retrying on account 'claude-2'
10:33  failover: retrying on account 'claude-1'
```

Because the override is process-wide, session A rotating to `claude-1` moves *every* session to `claude-1`. Session B then hits a burst limit there and rotates everyone back. Each session is behaving correctly on its own and the fleet oscillates.

The user described the observable asymmetry precisely, and it is diagnostic:

> when it changes account, it changes account on all jcode sessions, right? And in that specific account, when I run out of credits, I cannot continue to chat with that jcode session. But meanwhile, in the other sessions, the first change that it did I could already continue to work.

Sessions *not* holding the limited turn benefit from the switch and carry on. The session that owns the limited turn keeps being dragged back onto an account another session has just moved away from.

ADR-0016 and ADR-0017 remove the main driver, since a genuinely blocked account is now excluded from rotation, so there is much less to oscillate about. But two sessions hitting the same *burst* limit seconds apart still push each other, and the fix for that is independent of the verdict being right.

**Correction, added after this was tested:** the reasoning above is sound, but its application to the reported incident is not. With only two accounts this decision is unreachable, because the candidate list already excludes the active account, so there is nothing left to rotate to at the moment the question would be asked. See the Evidence section. The five-session incident was a two-account incident, and it is fixed by ADR-0016 and ADR-0017, not by this. This decision is correct for three or more accounts and was kept on that basis.

## Decision

A rotation always moves the request that is rotating. It rewrites the **shared** override only when it has strictly better information than whatever is there now:

```rust
let claim_shared_override = override_before
    .as_deref()
    .map(|active| tried.iter().any(|seen| seen == active))
    .unwrap_or(true);
```

That is: claim the shared pointer when the currently-active account is one *this request has already proven refuses it*. If another session has meanwhile settled on an account this request has not tried, leave the pointer alone; this request still uses its own chosen token for its own attempt.

The rotation announcement on the bus follows the same rule. ADR-0011 established that the broadcast carries no session id and is filtered client-side by provider, so an unclaimed switch must not be announced: it would repoint every other session's usage widget at an account they are not on.

The rollback on a failed credential load is likewise conditional, so a request that never claimed the override cannot restore a value it did not set.

## Consequences

The oscillation stops being fleet-wide. A session that has proven both accounts refuse it still claims the override, which is correct: at that point its information is genuinely the freshest available. A session that merely wants to move while another session is settled elsewhere no longer drags the fleet.

The costs, stated plainly:

- **This does not make the account per-session.** Two sessions still share one override, one credential slot, and therefore still misattribute usage in the window ADR-0009 described. This narrows *who moves whom*, not *who owns what*.
- **A session can now be on a different account from the one the shared override names**, for the duration of its request. That was already possible during rotation; it is now possible more often and for longer. The in-flight request holds its own token, so it is correct, but any future code that assumes "the override names this session's account" is wrong in one more case. ADR-0009's warning applies with more force, not less.
- **Fewer rotation announcements** means the usage widget follows the shared account rather than the requesting session's real account, in the unclaimed case. Choosing the quieter wrong answer over the noisier wrong one; only per-session identity makes it right.

## Rejected alternatives

**Per-session account identity, now.** The real fix, and still out of scope for the same reason ADR-0009 gave: it touches auth storage, the single-slot credential cache, the provider instance model, and the usage cache. Doing it inside a rate-limit bug fix would couple an architectural change to an urgent one.

**Lock the override for the duration of a turn.** Considered, and worse: a turn can run for minutes, so one session would pin the fleet's account choice while it works, converting oscillation into starvation.

**Never write the shared override from a rotation.** Then a session that has exhausted every account leaves the fleet pointed at an account known to refuse it, and the next turn in every session rediscovers that. The "only when we know more" rule keeps the useful case.

**Stop broadcasting rotations entirely.** Would leave every client's usage widget silently wrong, which is what ADR-0007 added the broadcast to fix.

## Revisit when

- Per-session account identity is picked up. This decision should be deleted then, not preserved: it is a mitigation whose whole purpose is to make a shared mutable pointer less harmful.
- Anyone observes a session using an account the shared override does not name, and is surprised by it. That is now expected, and the surprise means the model needs to become per-session rather than that this rule is wrong.
- Oscillation is seen again with ADR-0016 in place. That would mean the driver is not the shared override but something else, and this decision would need re-justifying rather than extending.

## Evidence

Written first as "no direct test, this needs two concurrent daemons". That was the wrong frame, and testing it properly changed what this decision is understood to do.

Concurrency reaches this code only as *state another session left behind*: which account is currently active, and which accounts this request has already tried. Those are two values, so the policy is a pure function of them, now extracted as `should_claim_shared_override` and covered as a decision table by `a_rotation_claims_the_shared_override_only_when_better_informed`:

| Active account | This request has tried | Claim the shared pointer? |
|---|---|---|
| `claude-1` | `claude-1` | yes, it knows the pointer is wrong |
| `claude-1` | `claude-2` | **no**, it knows nothing about `claude-1` |
| `claude-1` | `claude-2`, `claude-1` | yes, it has proven both refuse it |
| none | anything | yes, preserves pre-ADR-0018 behaviour |

`a_rotation_leaves_another_sessions_account_alone` drives the real `switch_to_next_claude_account` against the real process-global override, with the other session simulated by writing the same global it would have written, and asserts the override is untouched afterwards. Negative control for both: forcing `should_claim_shared_override` back to `true` fails them with "a request must not move a session onto an account it has not tried" and "the rotation must report that it did not claim the shared override".

### What the test exposed: this is a no-op with only two accounts

Writing the integration test surfaced a limitation that reading the code had not. `same_provider_account_candidates` excludes the *currently active* account from the candidate list. So with two accounts, the situation this decision guards against cannot arise: if another session holds `claude-1` and this request has already tried `claude-2`, there is no third account left to rotate to, and the rotation ends before the question is asked. The test needs a third account to reach the branch at all.

That materially narrows the claim. For the reported incident, which is two accounts, **this decision changes nothing**; the flapping there is fixed by ADR-0016 and ADR-0017 making the verdict correct, not by this. This matters from three accounts up, and it remains the right shape for a shared pointer, but it should not be credited with fixing the two-account case.

## Correction (2026-08-14): the two-account case was not fixed by 0016/0017 either

The paragraph above says the two-account flapping "is fixed by ADR-0016 and ADR-0017 making the verdict correct". That prediction was wrong, and it is worth recording rather than editing away.

On 2026-08-14 the same alternating pattern reappeared with two accounts, on a binary carrying 0016, 0017, this ADR, *and* the single-session cross-turn fix. Five concurrent sessions, four of them rotating within 25 seconds.

The reason the earlier reasoning missed it: 0016/0017 make each session's verdict correct **given the information it has**, and this ADR gates who may *write* the shared pointer. None of them serialise the *decision*, so with several sessions hitting one limit simultaneously, every session still evaluates a correct-but-stale view and every session still rotates. Correct inputs do not help when the race is in the timing.

Fixed by [ADR-0020](0020-serialise-account-rotation.md), which serialises rotation and has followers adopt the leader's account. The "no-op with only two accounts" observation above remains true of *this* decision in isolation.
