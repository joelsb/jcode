# 0027 - Anthropic fast mode is the `speed` field, never `service_tier`

**Status:** Accepted
**Date:** 2026-08-27

## Decision

Anthropic fast mode is requested with the `speed: "fast"` field on the Messages
request, together with the `anthropic-beta: fast-mode-2026-02-01` header. It is
supported on exactly two models: `claude-opus-5` and `claude-opus-4-8`.

jcode never sends `service_tier` on an Anthropic request. That parameter selects
Priority Tier, a different feature.

The provider trait keeps its existing method names (`service_tier()`,
`set_service_tier()`, `available_service_tiers()`) and keeps returning the
cross-provider string `"priority"`. Only what the Anthropic runtime *emits* for
that state changed. The names are a deliberate misnomer, documented at the call
site; see "Rejected alternatives".

## What forced it

`/fast on` in a Claude session did nothing, and the code looked correct while
doing nothing.

jcode modeled Anthropic fast mode as `service_tier: "auto"`, gated to
`claude-opus-4-8` only. Two consequences, both silent:

1. On `claude-opus-5`, the default model in real configs, `set_service_tier`
   returned an error and no request ever carried a fast-mode field. The banner
   still reported a saved fast-mode default, because that default lives in
   `provider.openai_service_tier` and is read only by the OpenAI runtime.
2. Even where the gate allowed it, the emitted parameter was the wrong feature.
   Priority Tier and fast mode are mutually exclusive, and Priority Tier is
   explicitly unsupported on Opus 5, so the request jcode would have sent could
   not have produced fast mode on any model.

The two features are easy to conflate because both are described as
"latency" controls and both are toggled by one UI switch. They are not the same:

| | Fast mode | Priority Tier |
| --- | --- | --- |
| Request field | `speed: "fast"` | `service_tier: "auto"` |
| Requires | beta header `fast-mode-2026-02-01` | a capacity commitment |
| What it buys | up to 2.5x output tokens/sec | prioritized availability, fewer overload errors |
| Price | $10 / $50 per MTok | standard rates |
| Models | Opus 5, Opus 4.8 only | all models *except* Mythos 5, Mythos Preview, Opus 5, Sonnet 5 |
| Response field | `usage.speed` | `usage.service_tier` |

They are mutually exclusive: "Fast mode is not available with a Priority Tier
commitment."

The model list is not a rounding of "recent Opus". It is exact, and the two
excluded neighbours fail differently:

- **Opus 4.7** returns an error for `speed: "fast"`.
- **Opus 4.6** *silently runs at standard speed* and bills standard rates.

Opus 4.6 is the dangerous one. A client that sends fast mode there gets a
successful response, no error, and no speedup. jcode's old pricing table
compounded this by billing Opus 4.7 and 4.6 fast mode at $30/$150 per MTok,
rates that appear nowhere in Anthropic's pricing and that neither model can ever
charge.

## Consequences

- `/fast on` works on Opus 5 and Opus 4.8, and is refused with a message naming
  both models everywhere else.
- Cost estimates for fast mode are correct for the first time: $10/$50 on both
  supported models, standard rates everywhere else.
- `usage.speed` is parsed and logged. This is the only ground truth for whether
  fast mode was granted; a request may ask for fast and be served standard when
  the dedicated fast-mode rate limit is hit, capacity is short, or the
  organization has no fast-mode access provisioned.
- Cost: the trait method named `service_tier` no longer has anything to do with
  `service_tier` on the Anthropic path. This is confusing on first read and is
  why this ADR exists.
- If jcode ever gains real Priority Tier support, it needs a *separate* control.
  Reusing this one would recreate the bug.

## Two follow-on defects the live run exposed

Unit tests could not have found either; both needed a real request.

**The saved default never reached an Anthropic session.** `AnthropicProvider::new`
hardcoded `service_tier: None`. `/fast default on` is stored once for all
providers under the historical key `provider.openai_service_tier`, and only the
OpenAI runtime read it. So `/fast` reported the default as on while every Claude
request went out standard. The provider now reads the same key, filtered by the
fast-mode model gate.

**An organization without fast-mode access got a hard 400 on every turn.** With
the default honored, the first live request returned:

```
400 invalid_request_error: Fast mode is not enabled for your organization.
An organization admin must enable this feature.
```

Fast mode is a research preview: Console organizations need access provisioned,
and subscription plans need usage credits enabled. jcode now self-heals by
dropping `speed` and retrying at standard speed, matching the documented
fallback, so an entitlement gap costs latency instead of the turn. This mirrors
the existing `is_reasoning_unsupported_error` self-heal.

## Rejected alternatives

**Rename the trait surface to `fast_mode` / `set_fast_mode`.** Honest, and it
removes the misnomer. Rejected because the name spans the provider trait, the
client/server protocol (`Request::SetServiceTier`, the `ServiceTierChanged`
event), the TUI command handlers, the info widget and the account center, and it
is shared with OpenAI, whose feature genuinely *is* a service tier. The rename
buys no user-visible behavior and enlarges a bug-fix diff into a protocol change.
Worth revisiting if the OpenAI side ever diverges too.

**Keep `service_tier: "auto"` and just add Opus 5 to the gate.** This was the
tempting one-line fix and it is wrong: Priority Tier is unsupported on Opus 5, so
it would replace a silent no-op with an API error, and it would still never
produce fast mode on Opus 4.8.

**Send both `speed` and `service_tier`.** Rejected: the docs state the two are
mutually exclusive.

## What would make us revisit

- Fast mode leaving research preview; the beta header, pricing and model list
  are all explicitly subject to change.
- Anthropic supporting fast mode on more models, or unifying the two parameters.
- jcode adding real Priority Tier support, which needs its own control rather
  than this one.

## Evidence

- https://platform.claude.com/docs/en/build-with-claude/fast-mode - supported
  models, `speed: "fast"`, beta header, `usage.speed`, Opus 4.6 silent downgrade,
  mutual exclusivity with Priority Tier.
- https://platform.claude.com/docs/en/api/service-tiers - `service_tier` is
  Priority Tier; "supported on all available Claude models except Claude Mythos
  5, Claude Mythos Preview, Claude Opus 5, and Claude Sonnet 5".
- https://platform.claude.com/docs/en/about-claude/pricing - fast mode $10/$50
  per MTok for Opus 5 and Opus 4.8, flat across the full context window.
- Meta-test, 2026-08-27: widening `anthropic_supports_fast_mode` to include
  `claude-opus-4-6` fails three tests across two crates
  (`fast_mode_is_exactly_opus_5_and_opus_4_8`,
  `anthropic_fast_mode_pricing_covers_opus_5_and_opus_48`,
  `test_fast_mode_rejected_on_unsupported_models`). The gate is enforced, not
  merely written down.
- A correction worth recording: the first diagnosis of this bug read only
  jcode's source, concluded "fast mode is unsupported on Opus 5", and pinned that
  conclusion with three passing tests. All three were green, plausible, and
  wrong, because they asserted jcode's own mistaken model rather than Anthropic's
  documented contract. Reverted in `7de08b385`. Where the question is what a
  provider accepts, the provider's docs are the spec; the local code is only a
  claim about them.
