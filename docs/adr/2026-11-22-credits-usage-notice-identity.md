# ADR: Credits usage notices share the selected season's identity

- Status: Accepted
- Date: 2026-11-22

## Context

A wallet can hold seasonal MANA grants from several seasons with overlapping
expiry windows. Separate notices by grant origin split the balance and let an
older notice key suppress a later season's notice. Changing the keys alone can
queue another email for someone who already received that season's reminder.

## Decision

Each usage reminder kind sums all eligible grants per wallet and uses the
current or last season's key. Seven-day and one-day reminders are independent.
A newly created notice retains the deterministic UUID derived from that key.

The shared notification writer takes a per-wallet transaction advisory lock.
An existing selected-season key wins. Otherwise, it attaches the selected key
to one same-kind notice under an exact origin-season key that was first created
since the selected season started. All started seasons' keys are candidates,
including those whose grants no longer qualify. The notice retains its UUID,
creation time, read state and delivery records; its metadata and occurrence time
refresh through the ordinary shared upsert. Other historical notices remain.
Reconciliation and the upsert commit or roll back together.

## Consequences

Replays, concurrent passes and changes in eligible grants create no second
selected-season delivery. An older season's notice does not suppress a new
season's notice. Terminal email delivery retention does not erase the retained
notification evidence. Deleted notification history cannot be reconstructed.

The same still-unused credits can therefore produce a seven-day reminder on
each side of a season change. With one to six grace days, the expiry window
can open before the granting season ends. A gap between seasons can also let
the window open while that season remains the last selected season, including
with longer grace periods. If the next season starts before expiry and the
wallet has progress in it, its selected-season key permits another reminder
and email, subject to email preferences. Each kind still creates at most one
delivery per wallet and selected season. Zero grace does not leave credits
unexpired after their granting season ends.

Existing partial notices and already queued emails are not deleted or recalled.
No new table, index, process or service dependency is needed. Retire writers
using origin-season keys before the corrected leased job runs: those writers
do not participate in its notification lock.

## Verification

PostgreSQL regressions cover combined balances, selected keys, concurrent fresh
inserts and reconciliation, existing selected notices, previous-season history,
sent/pending/failed/skipped and retained-away deliveries, expired origin grants,
email preferences, and rollback after a notification failure. Existing tests
cover expiry boundaries and consumed credits.
