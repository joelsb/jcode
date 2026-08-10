# ADR-0012: Announce a dead-end restore as its own outcome, not as a switch

Status: Accepted
Date: 2026-08-04 (added after code review of `f122474`)
Plan: `docs/superpowers/plans/2026-08-04-account-rotation-tui-sync.md`

## Context

[ADR-0003](0003-restore-the-account-override-only-on-a-dead-end.md) makes the restore path reachable in exactly one situation: `switch_to_next_claude_account` returned `None`, meaning every account was rate limited, so the override goes back to where the request found it.

[ADR-0007](0007-announce-rotation-to-clients-over-the-bus.md) then made every account change an announced event - including that one. Both were published through the same helper with the same shape, so the client rendered the dead end as:

> ⚡ Rate limited; switched Anthropic account: claude-2 → claude-1

Every word is literally true and the meaning is inverted. A rotation notice tells the user "your request is carrying on somewhere that works". The restore means the opposite: nothing worked, we are back at the start, and the turn is about to fail. Presenting them identically makes the widget most misleading at the moment the user most needs to understand what happened - and the restore is *also* the case where `claude-1` is known to be exhausted, so a user reading "switched to claude-1" may reasonably conclude they can keep working.

## Decision

Carry the distinction in the event.

- `BusEvent::AnthropicAccountRotated` gains `restored: bool`, set by `publish_anthropic_account_restored` on the dead-end path and left false by `publish_anthropic_account_rotated`.
- `ServerEvent::AnthropicAccountRotated` carries it over the wire as `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`.
- The client words the two cases separately: "Every Anthropic account is rate limited; restored X" versus "Rate limited; switched Anthropic account: X → Y".

One event with a flag, rather than two variants, because both cases mean the same thing to everything except the wording: the active account changed, so mirror it and invalidate the usage caches. Two variants would duplicate that handling and invite the two copies to drift.

The serde attributes are the compatibility contract. Skipping the field when false keeps the common case byte-identical to what a pre-flag daemon emitted, and `default` means a client reading an older daemon's event gets `restored: false` - an ordinary rotation, which is what the omission means.

## Consequences

- The give-up path reads as a dead end, which is what it is.
- The absent-equals-false default is only correct because `restored` is the rare case. If a later field's natural default were the common case, this pattern would silently mislabel old events; that reasoning has to be redone per field, not assumed from this precedent.
- `restored: true` is a *terminal* signal in a way `false` is not - the turn is about to fail. Nothing downstream depends on that yet. Anything that starts to should read ADR-0003 first, because the restore also compare-and-swaps and may decline to write at all, in which case no event is published and the client never learns.
- A reader may see a lone `bool` on the event and try to collapse the two publish helpers back into one to simplify. The wording difference is the reason they are separate; collapsing them reintroduces the inverted message.

## Evidence

- `crates/jcode-base/src/bus.rs` - the two publish helpers over one private `publish_anthropic_account_change`.
- `crates/jcode-provider-anthropic-runtime/src/lib.rs` - `restore_claude_account` calls the restore helper.
- `restoring_the_original_account_is_flagged_as_a_restore`, `anthropic_account_rotated_defaults_restored_when_the_daemon_omits_it`, `bus_restore_keeps_its_flag_on_the_wire`, and `restore_event_does_not_read_as_a_successful_switch` cover the flag from bus to rendered wording.
