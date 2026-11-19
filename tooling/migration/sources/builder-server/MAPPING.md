# builder-server -> builder

Pinned at `98897af692f26063a088aebb2ea5bb3f683168a2` (migrations through
`1790640000001_create-collection-events`).

| Step | Upstream | Target |
| --- | --- | --- |
| `collection` | `collections` | `builder.collections` |
| `item` | `items` + newest `item_curations` + parent `collections` | `builder.items` |
| `item_content` | `items.contents` (file -> hash) | `builder.uploaded_contents`, `builder.item_contents` |

## Derived columns

- `collections.urn_suffix` is the full urn the node keeps: the provider urn
  (`third_party_id:urn_suffix`) for a linked collection; the collections-v2 urn
  of the contract for a published standard collection; NULL otherwise.
- `collections.contract_address`: the CREATE2 address upstream computed for
  every standard collection's draft, lowercased; NULL for a linked
  collection. `publication_pending` starts false: the upstream `lock` is a
  short-lived publish lock.
- A collection's published and approved state is not imported. Upstream's
  `collections.is_published` and `is_approved` columns are not carried; the
  node derives both on read from its copy of the Polygon collections
  subgraph (by contract address), its own publication records and the items'
  `blockchain_item_id`.
- `items.is_published`: a blockchain item id in a published standard
  collection. Third-party items stay `false` (they have no blockchain id; the
  node's CHECK requires one) and carry their state in `curation_status`.
- `items.urn_suffix`: `urn:decentraland:matic:collections-v2:<contract>:<id>`
  for a published standard item, `third_party_id:collection:item` for a
  third-party item.
- `items.curation_status`: newest `item_curations.status`; else `approved` for
  a published item in an approved collection; else `pending`. `is_approved` is
  its projection. `content_hash` is the newest curation's content hash.
- Names are trimmed (node CHECK); empty `price`/`beneficiary`/`rarity` become
  NULL; addresses are lowercased. An item whose collection is missing
  upstream becomes a loose draft (`collection_id` NULL).
- Timestamps: upstream `timestamp without time zone`, read as UTC.
- A collection or item the node wrote after upstream's last save (its
  `updated_at` is newer) is kept on re-runs.

## Blobs

`builder.item_contents.hash` is an FK to `uploaded_contents`, which records a
blob the node holds and its size. The blob copy (out of scope) must record
each hash in `dcl_migration.blob_inventory (source, object_key, hash, size)`
with `source = 'builder-server'` and `object_key = hash`; `item_content`
stops the batch on any missing hash. `uploaded_by` is the item owner.

## Not migrated

- `projects`, `asset_packs`, `assets`, `collection_curations`,
  `collection_events`, `slot_usage_cheques`, `virtual_third_parties`,
  `deployments`, `pools`, `pool_groups`, `pool_likes`: no target. The node
  keeps no scenes.
- `collections.is_published`, `is_approved`: derived on the node, not stored.
- `collections.managers`, `minters`, `reviewed_at`, `forum_link`, `forum_id`,
  `lock`, `linked_contract_address`, `linked_contract_network`: no target column.
- Left to the node: `collection_publications`, `linked_collection_publications`,
  `items.in_catalyst`, `items.catalyst_content_hash`,
  `newsletter_subscriptions` (upstream forwards opt-ins and stores none).

No watermarks (state is derived across tables); every run re-scans. Upstream
deletions, including files removed from an item's `contents`, are not detected.
