# ADR: The builder keeps builder-server's curation records and reads curation state from them

- Status: Accepted
- Date: 2026-11-19

## Context

`github.com/decentraland/builder-server` at `98897af` reviews collections
and third-party items through four records: a collection's curations (one
per review request, with its assignee, decision and rejection reasons), an
item's curations (one per submission of a third-party item, with the
content hash reviewed), a collection's review events and the slot cheques a
third-party publication signs. Its clients read curation state from these
rows: a third-party item reads as published once it has an item curation,
and a collection's review state is its newest curation.

The node takes curation over from builder-server, so it must hold these
records with the meaning builder-server gives them, and imported data must
answer as it does upstream.

## Decision

- **The rows are the state.** `builder.collection_curations`,
  `builder.item_curations`, `builder.collection_events` and
  `builder.slot_usage_cheques` keep builder-server's columns and values. An
  item's curation status is the status of its newest item curation, read
  where it is needed. No column on an item or a collection copies it, so no
  writer has to keep a copy in step.
- **Newest is well defined.** A collection or an item has at most one
  curation per instant, and every writer stamps a new item curation after
  the item's newest one, so the newest row is the last written. Events are
  read newest first by time, then id, and a validation has one verdict.
- **Published follows curation.** A third-party item is published once it
  has an item curation, whatever its status. Its files, its reads, the
  listings and the provider's slot count use that one rule, and a
  collection with a curated item is published.
- **Stricter only where no client writes.** Statuses, event types, actors
  and rejection reasons are checked against builder-server's values;
  addresses are lowercase; a forum link is an `http(s)` URL that the node
  stores and returns but never fetches.
- **Deletes.** An item's curations and a collection's curations and events
  go with them; a slot cheque, the record of slots consumed, keeps its
  collection.

## Consequences

- The import copies the four tables and the forum columns. Of an item's
  curations created at the same instant, it keeps the one updated last; it
  clears a forum link that is not a web URL.
- A database bootstrapped before this decision keeps
  `builder.items.curation_status` and `is_approved` until
  `schemas/releases/builder-item-curation-retired.sql` drops them, after
  recording a curation for each third-party item the node submitted itself.
- A writer of curations or events holds the collection locked, which the
  code upholds; the schema can't.
