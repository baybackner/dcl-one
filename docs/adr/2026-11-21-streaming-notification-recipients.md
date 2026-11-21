# Durable streaming notification recipients

Streaming key reset, revoke, expiry, timeout and place-removal notices must
reach every affected wallet while preserving independent safety cleanup.
Owner or permission services can be unavailable; world identities can retire
before catalogue derivation observes removal; bounded jobs can stop midway
through a recipient list.

## Decision

Use the existing cleanup and removal outboxes for normalized recipient arrays,
a resolution bit and a durable delivery cursor. Capture valid active local
scene admins at removal-event creation through a plain MVCC read of Social
grants, without Social row locks or writes. This
prevents a pre-maintenance re-grant from erasing the earlier recipient. Retain
safety-transaction capture as a fallback for older events, and resolve all
external authority classes before marking the snapshot complete, and checkpoint each Social acceptance. Retries
keep the event identity, original context and complete recipient set. Social's
existing recipient/kind/event deduplication covers replay after acceptance but
before the cursor write. A delivery failure does not undo revocation.

Credentials save the exact bound scene admin namespace at issuance. Explore
keeps exact current identity keys in both catalogue legs before final scene
retirement when no backing scenes remain, and freezes them with active keys
into each removal event. The world row lock serializes publication; identity
locks precede catalogue writes. Actual scene insertion clears only catalogue
evidence, because an imported identity can precede its backing scene. No new
historical authority table or grant reconstruction by overlap is introduced.

## Consequences

Outbox storage grows with the number of affected wallets and is deleted after
completion. Historical feed seeding suppresses notices but preserves safety
cleanup. Existing scalar recipients are expanded rather than treated as a
complete set. Exact retired namespaces and overwritten grant history from
before this evidence existed cannot always be recovered. Complete snapshots
remain immutable across permission edits, scene re-adds and delivery retries.

Before exposing writers, a Social-writing serve process scans active native
credentials with missing admin keys in bounded UUID batches, decodes their
canonical rooms with the existing codec, and saves exact live world identity
bindings under world/identity locks. Read-only Social replicas skip this write.
The upgrade requires stopping old writers and finishing offline imports before
the coordinated startup, so identities cannot retire before this backfill.
Noncanonical imported rooms and scenes already retired before the upgrade
remain outside exact recovery; pending authority stays retryable where available.
