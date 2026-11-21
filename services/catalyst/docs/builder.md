# Builder: what a node needs to serve it

The builder routes (`/v1/...` at the root and `/builder-api/v1/...`,
`services/catalyst/builder`) answer the wearable and emote builder of
`/create`, the Marketplace's reads and the wearable validator. This page
lists what a node needs before it serves them. Each section names the
setting and what a request gets while it is missing.

## Database

Run `catalyst schema migrate` before the binary that serves the builder
starts. Then run the builder's release scripts, each after its step and a
restore point, with `psql -X -f`, in this order:

1. `schemas/releases/builder-scenes-retired.sql`
2. `schemas/releases/builder-collection-flags-retired.sql` (creator writes
   off while old and new binaries overlap)
3. `schemas/releases/builder-item-documents-and-uploaders.sql` (until it
   runs, item saves and uploads answer 503)
4. `schemas/releases/builder-item-curation-retired.sql`
5. `schemas/releases/builder-linked-forum-retired.sql`
6. `schemas/releases/builder-linked-publications-retired.sql`

`DATABASE.md`, the "Retired builder" sections, gives each script's rollout.

## Chain state and the committee

The builder derives a collection's published and approved state and its
roles from the shop's Polygon collections copy, and the curation committee
from the shop's committee index, on the same node.

- `MARKETPLACE_MIRRORS_ENABLED` with `MATIC_COLLECTIONS_SUBGRAPH_URL`: the
  collections mirror. Until its first complete pass, listings and bodies
  answer 503; once a pass is more than 30 minutes old, saves and deletes
  answer 503 and a publish 401.
- `MARKETPLACE_COMMITTEE_INDEX_ENABLED`, which walks the Polygon committee
  over `TRADE_RPC_URLS` (its `137` entry), else `RPC_ENDPOINT_POLYGON`.
  While the index is unpublished or was published more than 30 minutes ago,
  every decision that needs the committee answers 503.
- `BUILDER_POLYGON_RPC_URL`: an `https` Polygon JSON-RPC on a public host,
  with the `finalized` block tag. It verifies publication receipts, reads the
  rarities' prices and checks a collection's contract before a save that
  changes its address, a delete and a publish. Unset, the publication and
  rarities routes answer 503 and the contract check is skipped.
- `THIRD_PARTY_REGISTRY_L2_SUBGRAPH_URL`: the third-party registry. Unset or
  unreadable, every request about a third party answers 503.
- The content sync's item history coverage: a new third-party item URN is
  taken only where the content history is covered or this node is its own
  content authority; elsewhere the URN check answers 503.

## Ingress

- `HTTP_TRUSTED_PROXY_CIDRS` lists the gateway's networks. Without it, a
  signed request on a path the gateway rewrote (`builder-api.<domain>`) is
  refused, and the builder can't tell clients apart, so it applies no
  per-source request budget and only the gateway's per-client limit holds.
- `CORS_ORIGIN` lists the origins that may read the answers (`;`-separated,
  `regex:` entries), `CORS_METHOD` the methods a preflight allows. Unset,
  `CORS_ORIGIN` is the Decentraland front-ends and `http://localhost:<port>`.
  The gateway adds no `Access-Control-Allow-Origin` of its own under
  `/builder-api/`.
- The node's clock is synchronized: a signature is accepted at most 60
  seconds old or ahead.

## Writes, files and flags

- `CREATE_WRITES_ENABLED=true` opens the writes; off, each answers 503.
- `STORAGE_ROOT_FOLDER` holds item files and videos; a wallet's uploads may
  hold at most 5,000,000,000 bytes that no published item names.
- The `builder` flags document, which Explore's flags mirror stores, turns
  the automated review on (`builder-auto-curation`) and the review flow
  (`builder-auto-curation-review-flow`); both also need
  `WEARABLE_VALIDATOR_CALLBACK_SECRET`.

## Outside services and secrets

Each secret may also be given as `<NAME>_FILE`; none is printed.

| Service | Settings | Without them |
| --- | --- | --- |
| Wearable validator | `WEARABLE_VALIDATOR_CALLBACK_SECRET` (at least 32 bytes) | No validation is requested; the validator routes refuse |
| Curators' Slack | `SLACK_BOT_TOKEN`, `SLACK_CURATION_CHANNEL`, `CREATORS_URL` (else `BUILDER_URL`) | No notice is posted |
| Curators' forum | `FORUM_URL`, `FORUM_API_KEY`, `FORUM_API_USERNAME`, `FORUM_CATEGORY`, `BUILDER_SERVER_URL` | Forum posts and replies answer 503 |
| Terms of service warehouse | `WAREHOUSE_URL`, `WAREHOUSE_TOKEN`, `WAREHOUSE_CONTEXT_PREFIX` | Acceptances are stored and forwarded later |
| Newsletter service | `NEWSLETTER_SERVICE_URL`, `NEWSLETTER_PUBLICATION_ID`, `NEWSLETTER_SERVICE_API_KEY` | Subscriptions are stored only |
| Curation console | `CATALYRST_BUILDER_ADMIN_TOKEN` (at least 32 bytes), `BUILDER_ADMIN_ADDRESSES` | The console refuses |
