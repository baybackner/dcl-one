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

- `CREATE_WRITES_ENABLED=true` opens the writes; off, each answers 503, and
  the builder's jobs (the terms forward to the warehouse and the
  validation-request sweep) change no row and call no outside service.
- `STORAGE_ROOT_FOLDER/builder` holds item files and videos, apart from
  the content tree, so no content or worlds route serves them. The next
  section bounds what each wallet, source address and the whole node may
  store and send there.
- The `builder` flags document, which Explore's flags mirror stores, turns
  the automated review on (`builder-auto-curation`) and the review flow
  (`builder-auto-curation-review-flow`); both also need
  `WEARABLE_VALIDATOR_CALLBACK_SECRET`.

## Uploads, drafts and request budgets

The builder bounds what one wallet, one source address and the whole node
store and send, so no caller fills the node's disk, database or memory. The
values are in the [bounds table](#bounds); this section says what each
bound counts and what a request past it gets.

- **Sources.** A source address is the client's address, an IPv6 address
  widened to its /64; an upload source widens an IPv6 address to the block
  the table names. Behind the gateway, set `HTTP_TRUSTED_PROXY_CIDRS`, or
  the node sees no client address and applies no per-source bound.
- **Request budgets.** Every request spends from its source address's
  reads or writes a minute, and a verified signer from its wallet's, before
  the body is read; past either, 429 with `Retry-After`.
- **Unpublished uploads.** A wallet's unpublished uploads are its upload
  records whose file no published item names. Before an upload's body is
  read, its declared length, else the route's largest body, counts as
  stored: past the wallet's bound the upload answers 413 with `maxBytes`
  or `maxFiles` ("The wallet's unpublished files reached their limit"),
  past the node's 507 ("The node's unpublished files reached their limit").
  `BUILDER_UNPUBLISHED_MAX_BYTES` and `BUILDER_UNPUBLISHED_MAX_FILES` set
  the node's bound. The node counts every wallet's unpublished uploads in
  the background, one count at a time, and keeps each count for the time
  the table names, so a publication frees a wallet's uploads within it.
- **Days of uploads.** A wallet has a day of uploaded files, an upload
  source a day of files and of bytes. Each is checked before the body and
  charged with what arrived, whatever came of the upload. Once every
  wallet's unpublished uploads hold half the node's bound, in bytes or in
  files, each byte and file an upload source sends costs its day the price
  the table names. A spent day answers 429 with `Retry-After`.
- **File uploads.** A file upload takes a place among the node's file
  places, of which a wallet and an upload source hold a share; with none
  free it answers 429 with `Retry-After` at once. A body that hasn't arrived
  within its deadline answers 408 ("The upload timed out") and frees the
  place.
- **Video uploads.** A video upload takes a place among the node's video
  places, of which a wallet and an upload source hold a share, and spends
  upload time from the wallet's and the source's day. While every place is
  held, a newcomer takes the place of the upload that has run past its
  first idle window and whose wallet or source spent the longest uploading
  today, if longer than the newcomer's; that upload ends with 408 ("The
  upload gave its place to another while the node was busy"). Otherwise the
  newcomer gets 429 with `Retry-After`. `BUILDER_VIDEO_UPLOADS_MAX` sets the
  node's places; startup refuses a value outside the table's range.
- **Drafts.** A wallet's drafts are the items it holds that no publication
  names (a standard item until its collection is published, a third-party
  item until it has an item curation), the collections it holds
  unpublished, and the stored bytes of those items' documents. A save that
  would pass one answers 413 ("The wallet's unpublished drafts reached
  their limit") with `maxItems`, `maxCollections` or `maxBytes`. A wallet
  at its item bound still edits the drafts it holds.
- **Days of documents.** An item or collection save spends one more write
  token per the table's bytes of its body, and its size, at the least the
  table's minimum, from the wallet's and its upload source's day of
  documents; past either, 429 with `Retry-After`.

### Bounds

Each row names the constant of the builder crate
(`services/catalyst/builder/src`) that holds its value. The builder's test
`the_builder_doc_states_each_bound_the_code_enforces` fails when a row and
its constant differ or a row names no bound it checks.

| Bound | Value | Code |
| --- | --- | --- |
| Reads of a source address a minute | 3,000 | `limits::SOURCE_READS` |
| Writes of a source address a minute | 2,400 | `limits::SOURCE_WRITES` |
| Reads of a wallet a minute | 2,400 | `limits::WALLET_READS` |
| Writes of a wallet a minute | 1,200 | `limits::WALLET_WRITES` |
| Body of an item save | 256 KiB | `ITEM_BODY_LIMIT` |
| Body of a collection save, a terms acceptance or a publication | 2 MiB | `LARGE_BODY_LIMIT` |
| Body of another route but the uploads | 64 KiB | `BODY_LIMIT` |
| A wallet's unpublished upload bytes | 5,000,000,000 bytes | `limits::WALLET_BYTES` |
| A wallet's unpublished upload files | 100,000 | `limits::WALLET_FILES` |
| The node's unpublished upload bytes, `BUILDER_UNPUBLISHED_MAX_BYTES` unset | 1,000,000,000,000 bytes | `limits::NODE_BYTES` |
| The node's unpublished upload files, `BUILDER_UNPUBLISHED_MAX_FILES` unset | 10,000,000 | `limits::NODE_FILES` |
| How long a count of the unpublished uploads is kept | 1 minute | `limits::UNPUBLISHED_KEPT` |
| Bytes an upload source sends a day | 20 GiB | `limits::SOURCE_UPLOAD_MIB` |
| Files an upload source sends a day | 100,000 | `limits::SOURCE_UPLOAD_FILES` |
| Files a wallet uploads a day | 50,000 | `limits::WALLET_UPLOAD_FILES` |
| Price of a source's bytes and files past half the node's bound | 10 | `limits::NEAR_BOUND_PRICE` |
| The IPv6 block of one upload source | /48 | `limits::upload_source` |
| Parts of a file upload | 256 | `items::upload::MAX_FILES` |
| Bytes of a file upload's files | 20 MiB | `items::upload::MAX_BYTES` |
| Body of a file upload | 21 MiB | `UPLOAD_BODY_LIMIT` |
| Time for a file upload's body to arrive | 5 minutes | `items::upload::DEADLINE` |
| File uploads at once on the node | 16 | `FILE_UPLOADS` |
| File uploads at once of a wallet | 4 | `FILE_UPLOADS_PER_SIGNER` |
| File uploads at once of an upload source | 8 | `FILE_UPLOADS_PER_SOURCE` |
| `Retry-After` of a refused place | 30 seconds | `items::places::RETRY_AFTER_SECS` |
| Bytes of a video | 250,000,000 bytes | `items::video::DEFAULT_MAX_BYTES` |
| Time for a video's body to arrive | 20 minutes | `items::video::DEFAULT_DEADLINE` |
| Idle window: time without a byte before a video upload ends, and before a newcomer may take its place | 30 seconds | `items::video::DEFAULT_IDLE_TIMEOUT` |
| Video uploads at once on the node, `BUILDER_VIDEO_UPLOADS_MAX` unset | 32 | `items::video::MAX_UPLOADS` |
| The most `BUILDER_VIDEO_UPLOADS_MAX` sets, from 1 | 256 | `MAX_VIDEO_UPLOADS_SETTING` |
| Video uploads at once of a wallet | 1 | `items::video::MAX_UPLOADS_PER_SIGNER` |
| Video uploads at once of an upload source | 2 | `items::video::MAX_UPLOADS_PER_SOURCE` |
| Video upload time of a wallet a day | 4 hours | `items::video::WALLET_VIDEO_SECS_A_DAY` |
| Video upload time of an upload source a day | 12 hours | `items::video::SOURCE_VIDEO_SECS_A_DAY` |
| A wallet's draft items | 25,000 | `limits::WALLET_ITEMS` |
| A wallet's unpublished collections | 5,000 | `limits::WALLET_COLLECTIONS` |
| Stored bytes of a wallet's draft item documents | 256 MiB | `limits::WALLET_DOCUMENT_BYTES` |
| Documents a wallet saves a day | 1 GiB | `limits::WALLET_DOCUMENT_KIB` |
| Documents an upload source saves a day | 2 GiB | `limits::SOURCE_DOCUMENT_KIB` |
| The least a save costs its day | 4 KiB | `limits::DOCUMENT_MIN_KIB` |
| Body bytes one more write token pays for | 16 KiB | `limits::BYTES_PER_WRITE_TOKEN` |

## Outside services and secrets

Each secret may also be given as `<NAME>_FILE`; none is printed.

| Service | Settings | Without them |
| --- | --- | --- |
| Wearable validator | `WEARABLE_VALIDATOR_CALLBACK_SECRET` (at least 32 bytes) | No validation is requested; the validator routes refuse |
| Curators' Slack | `SLACK_BOT_TOKEN`, `SLACK_CURATION_CHANNEL`, `CREATORS_URL` (else `BUILDER_URL`) | No notice is posted |
| Curators' forum | `FORUM_URL`, `FORUM_API_KEY`, `FORUM_API_USERNAME`, `FORUM_CATEGORY`, `BUILDER_SERVER_URL` | Forum posts and replies answer 503 |
| Terms of service warehouse | `WAREHOUSE_URL`, `WAREHOUSE_TOKEN`, `WAREHOUSE_CONTEXT_PREFIX` | Acceptances are stored and forwarded later |
| Newsletter service | `NEWSLETTER_SERVICE_URL`, `NEWSLETTER_PUBLICATION_ID`, `NEWSLETTER_SERVICE_API_KEY` | Subscriptions are answered `{ok:true}` and go nowhere; the node never stores one |
| Operator routes (item status, `GET /v1/collections/curation`) | `CATALYRST_BUILDER_ADMIN_TOKEN` (at least 32 bytes), the curation console's bearer; `BUILDER_ADMIN_ADDRESSES`, the wallets whose signed requests they admit | The routes refuse every request |
