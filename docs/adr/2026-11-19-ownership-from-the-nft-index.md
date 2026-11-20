# Wearable and emote ownership from the nft index

- Status: Accepted
- Date: 2026-11-19

## Decision

The shop answers who holds which wearable or emote from its own nft index
(`shop.nft`), the fold of the collections' Transfer and v1 Issue logs it walks
on chains 1 and 137. No ownership read asks the collections subgraphs. The
reader lives in the shop (`services/catalyst/shop/src/market/owned_items.rs`)
and serves profiles and the Registry's profile batches, the lambdas wearable
and emote lists, the explorer backpack, `/v1/users`, outfits, the `/v1/orders`
owner guard and the wearable and emote pages of `/v1/nfts`. It reads only the
shop's tables, so no cross-area SQL edge is added. Content deployment
validation keeps its own check, by RPC at the entity's block plus
`shop.usage_grants`.

A v2 item is matched by contract and the item id its token id encodes, so the
profile and orders reads do not wait for the items mirror; a v1 item by its
collection's URN (`shop.collections`) and the wearable id of its Issue log.
When a profile names no token, the first held token is the newest transfer.
The orders guard checks the listing's exact token: a listing whose signer no
longer holds that token drops out even when the signer holds another copy,
and a token the index does not hold (LAND, an estate) passes.

Every read checks the index's cursor in the same `REPEATABLE READ, READ ONLY`
transaction as its rows. Two rules:

- Profiles need the index complete on the networks their items name: the
  whole history walked and the last fold published, with no reorg rewind or
  unpublishable fold since (`full_history` and `folded_block` set). An index
  that is complete but was published more than 30 minutes ago, as during an
  RPC outage, keeps serving them. An incomplete index fails the read: a
  direct read answers 503, and a Registry batch leaves the profile out and
  counts the failure.
- The other readers need the index complete and published within 30 minutes
  on the networks they read (wearables both, emotes Polygon), and the lists
  also the items mirror's first complete pass; otherwise they answer the
  `nft-index` unavailable envelope.

Neither rule compares `folded_block` with `last_block`: each pass advances
`last_block` before it folds and publishes, so that equality is false for part
of every pass while the table still holds a published fold. A reorg rewind
clears `folded_block` and deletes the fold's rows in one transaction, and only
a pass that reaches the head sets it again, so a refill is never read as
complete.

A node that does not walk the index (the switch off, or a network other than
mainnet) serves profiles and outfits as deployed and answers the envelope on
the inventory routes. A mainnet mirror refuses to start with the index off or
with a `TRADE_RPC_URLS` that names no chain 137 endpoint.

## Consequences

Ownership no longer depends on a hosted subgraph per request, and an owner of
any size is one query. The answers trail the chain head by the confirmation
depth (12 blocks on Ethereum, Polygon's finality) plus a pass, and the
inventory lists leave out an item the items mirror does not hold yet. Testnet
nodes serve profiles unchecked until the walk reads their chains. LAND,
estates and names on `/v1/nfts` keep reading the marketplace subgraph until
their reads move to the indexes.

A `/v1/nfts` page without an owner reads each source's first `first + skip`
rows in the page's order and pages their union, the collections and names
walking `nft_mint_order_idx` and the on-sale reads starting from the listed
tokens. On a 3.5M-token index the newest wearables take 15 ms, the page
naming no category 282 ms (most of it the LAND source, which reads every
parcel's mint before sorting) and the wearables on sale 121 ms
(DATABASE.md, "Wearable and emote ownership workload").

Revisit when the walk covers the testnets, or when a reader needs ownership
fresher than the confirmation depth.
