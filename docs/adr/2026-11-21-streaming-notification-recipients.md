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
grants, without Social row locks or writes. This prevents a pre-maintenance
re-grant from erasing the earlier recipient. Retained grants supply fallback
recipients for older events after the safety transaction commits.
Notification-only enrichment runs after that commit. Resolve all external
authority classes before marking the snapshot complete, and checkpoint each
Social acceptance. Retries keep the event identity, original context and
complete recipient set. Social's existing recipient/kind/event deduplication
covers replay after acceptance but before the cursor write. A delivery failure
does not undo revocation.

Key cleanup bounds all notification preparation to five seconds and saves
partial recipients, exact keys and unresolved binding obligations. Runtime
binding reads use MVCC without publication locks; deferred resolution also
reads inactive credentials and cannot complete while an exact native binding
is required but unavailable.

Credentials save the exact bound scene admin namespace at issuance. Explore
keeps exact current identity keys in both catalogue legs before final scene
retirement when no backing scenes remain, and freezes them with active keys
into each removal event. The world row lock serializes publication; identity
locks precede catalogue writes. Actual scene insertion clears only catalogue
evidence, because an imported identity can precede its backing scene.

Explore also retains each displaced exact `(world_name, entity_id, identity)`
binding in `world_scene_binding`, in the transaction that replaces, retires or
deletes that identity. This preserves the namespace even if a legacy key's
binding lookup fails, revocation commits, and the scene retires before the
first deferred retry. Comms reads this evidence and the current binding; a
missing key resolves only when they identify exactly one namespace. A saved
credential/context key takes precedence. Reusing an entity under different
identities remains ambiguous and pending, never inferred from parcel overlap.
The capture writes Explore alone and takes no Social row locks.

## Consequences

Outbox storage grows with the number of affected wallets and is deleted after
completion. Historical feed seeding suppresses notices but preserves safety
cleanup. Existing scalar recipients are expanded rather than treated as a
complete set. Exact retired namespaces and overwritten grant history from
before this evidence existed cannot always be recovered. Complete snapshots
remain immutable across permission edits, scene re-adds and delivery retries.
Binding facts are retained without expiry because unresolved credentials and
cleanup intents can outlive scene publication. Their count grows with distinct
displaced bindings. The primary key supports exact lookup and idempotent
capture. The schema object pass installs the new table, function and trigger
on existing databases without rewriting credentials. Information destroyed
before capture was installed cannot be reconstructed.

Before exposing writers, a Social-writing serve process scans active native
credentials with missing admin keys in bounded UUID batches, decodes their
canonical rooms with the existing codec, and saves exact live world identity
bindings under world/identity locks. Read-only Social replicas skip this write.
The upgrade requires stopping old writers and finishing offline imports before
the coordinated startup, so identities cannot retire before this backfill.
Noncanonical imported rooms and scenes already retired before the upgrade
remain outside exact recovery; pending authority stays retryable where available.

## Credential issuance bound

REST creation, reset and Cast issuance share a rolling limit of six new
credentials per place per hour. Reusing an existing credential consumes no
issuance. Retained active and inactive credentials supply the count through
`(place_id, created_at DESC)`; a transaction advisory lock per place serializes
admission before deactivation and insertion. Refused issuance returns HTTP 429
and preserves the previous credential. Revocation remains available. Six allows
an initial key and five replacements in an hour; this is a configured budget,
not a measured production workload. A separate quota table is unnecessary
because credential history is already retained.
