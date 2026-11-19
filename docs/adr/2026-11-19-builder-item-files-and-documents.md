# ADR: The builder keeps item documents in order and stores only named, verified files

- Status: Accepted
- Date: 2026-11-19

## Context

The builder answers items as `github.com/decentraland/builder-server` at
`98897af` does. Three of its properties shape how the node stores them:

- An item's `data`, `metrics` and `contents` are JSON documents that
  builder-server keeps as saved, keys in order. The content hash it stores
  for a published standard item (ADR-32 over its files and entity metadata)
  and for a third-party item (keccak-256 of its metadata, files and
  mappings) serializes those documents in that order, and the clients compare
  that hash with the catalyst's.
- A client saves an item, naming each file by its content hash, and then
  uploads the files.
- Item files are private until their item is published, so a name in an
  item must not open a file to whoever wrote the name.

## Decision

- **Documents in order.** `builder.items.data` and `metrics` are `json`,
  written as `JSON.stringify` writes them, and `builder.item_contents` keeps
  each file's position. Bodies answer them as stored, and the content hashes
  the node computes on a save match builder-server's byte for byte.
- **What a published item takes.** A standard item is published once it is
  minted and the copy of the Polygon collections subgraph holds it (or the
  node published it itself); its body then takes the chain's URN, price,
  beneficiary, rarity, supply and content hash. A third-party item is
  published once the node submitted it. On a published collection, the
  owner and the on-chain managers save an existing item's metadata and
  contents; a new item, a move in or out and a rarity change are refused.
- **Uploads are named and verified.** Each uploaded part is sent under the
  content hash its item names. The node reads the body within its bounds,
  hashes each file, refuses a file whose bytes don't match the hash or whose
  item doesn't name it, and only then stores it.
- **Naming grants nothing.** An item may name any hash. What proves a wallet
  holds a file is its own upload record (`builder.uploaded_contents`, one row
  per hash and uploader), never a name in an item.
- **Records follow names.** When a save or a delete leaves a hash no item
  names, its upload records go in the same transaction. The blob collector
  counts upload records and never a name a save writes: neither a hash in
  an item's `contents` nor its `thumbnail` file name keeps a blob. The item
  hashes it does count (`local_content_hash`, `content_hash`,
  `catalyst_content_hash`) are written by the node and the import, never
  by a save or an upload. So it may take the bytes once no record and
  nothing else names them.
- **A publication takes only uploaded files.** Both publications refuse an
  item that names a hash its owner holds no upload record for. The
  publication snapshot computes it (`missing_content` in `SNAPSHOT_QUERY`,
  `services/catalyst/builder/src/publication.rs`); the tests
  `a_standard_publication_takes_only_files_the_items_owner_uploaded` and
  `a_linked_publication_takes_only_files_the_items_owner_uploaded` hold it.
- **Imported records.** Builder-server keeps no uploader, so the import
  records each item's owner as an uploader of every hash the item names,
  with the blob's size from the blob copy's inventory. A wallet whose
  imported item names a hash therefore holds a record for it, whoever sent
  the bytes. The node treats an imported record as one written on upload:
  it protects the bytes from the collector, it satisfies a publication's
  check, and what it lets a wallet read is the file routes' decision.

## Consequences

- No file is stored that its item doesn't name, so what an owner stores
  beyond what its items name is bounded by what is in flight.
- The file routes can decide a private file by the reader's upload records
  and the items that name it; a hash learned from elsewhere opens nothing.
- A database the release hasn't run on keeps `jsonb` documents and one
  upload record per hash until the release
  `schemas/releases/builder-item-documents-and-uploaders.sql` runs
  (`DATABASE.md`, "Builder items workload", gives the rollout order). Until
  then item saves and uploads answer 503 naming the release and the item
  import refuses to load, so no document loses its order; reads keep
  working.
- A client must save an item before uploading its files and must send each
  file under its hash, as the old builder web does.

## Alternatives considered

- **`jsonb` documents.** `jsonb` reorders keys, so the node's content hashes
  would differ from the catalyst's for third-party and published items.
- **Refusing a hash another wallet uploaded.** A creator who has the same
  bytes as another wallet, a common texture for example, couldn't name them
  before uploading, which is the order the clients save in.
- **Storing any uploaded file.** Storage would grow with files no item names,
  and nothing would tie a file to the item it was uploaded for.
