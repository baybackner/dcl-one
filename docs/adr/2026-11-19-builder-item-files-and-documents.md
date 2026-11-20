# ADR: The builder keeps item documents in order, stores only named, verified files and serves them only to their readers

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
  published once it has an item curation, whatever its status: a
  submission for review, recorded by the node or imported. On a published collection, the
  owner and the on-chain managers save an existing item's metadata and
  contents; a new item, a move in or out and a rarity change are refused.
- **Uploads are named and verified.** Each uploaded part is sent under the
  content hash its item names. The node reads the body within its bounds,
  hashes each file, refuses a file whose bytes don't match the hash or whose
  item doesn't name it, and only then stores it.
- **Naming grants nothing.** An item may name any hash. What proves a wallet
  holds a file is its own upload record (`builder.uploaded_contents`, one row
  per hash and uploader), never a name in an item.
- **Each name has a writer.** Every row of `builder.item_contents` keeps
  `named_by`, the wallet whose save wrote that name. A save keeps the writer
  of a file and hash it re-sends, and of a hash the item already named
  under another file name; only a hash the item didn't name takes the
  saving signer. A name counts only while its writer holds an upload record
  for its hash. An item's `video` is a name too: `builder.items.video_named_by`
  keeps the wallet whose save wrote it (kept while the value is, else the
  writer of the item's own entry for that hash, else the saving signer), so a
  published smart wearable's kept video stays readable after a re-save
  replaces its `video.mp4`, and a wallet that writes another wallet's hash
  there opens nothing.
- **Records follow names.** When a save or a delete leaves a hash no item
  names, in its contents or as its video, its upload records go in the same
  transaction. The blob collector
  counts upload records and never a name a save writes: neither a hash in
  an item's `contents` nor its `thumbnail` file name keeps a blob. The item
  hashes it does count (`local_content_hash`, `content_hash`,
  `catalyst_content_hash`) are written by the node and the import, never
  by a save or an upload. So it may take the bytes once no record and
  nothing else names them.
- **A publication takes only uploaded files.** Both publications refuse an
  item with a name that doesn't count: a hash whose writer holds no upload
  record for it. The
  publication snapshot computes it (`missing_content` in `SNAPSHOT_QUERY`,
  `services/catalyst/builder/src/publication.rs`); the tests
  `a_standard_publication_takes_only_files_each_names_writer_uploaded` and
  `a_linked_publication_takes_only_files_each_names_writer_uploaded` hold it.
- **Imported records.** Builder-server keeps no uploader, so the import
  attributes every name to the item's owner and records that owner as an
  uploader of every hash the item names, with the blob's size from the blob
  copy's inventory. A wallet whose
  imported item names a hash therefore holds a record for it, whoever sent
  the bytes. The node treats an imported record as one written on upload:
  it protects the bytes from the collector, it satisfies a publication's
  check, and it opens the file to that wallet as the file routes below
  decide.
- **Files are private until published.** The node serves item files from
  its own blob store, at `GET` and `HEAD /v1/storage/contents/{hash}` and
  the `exists` probe, and never redirects to a bucket. A request without a
  signature reads a file only while a published item names it in a name
  that counts. A standard item is published once its collection is; a
  third-party item once it has an item curation. A signed request also reads a
  file its signer holds an upload record for, and a file that an item the
  signer owns, or a third-party item whose collection the signer manages,
  names in a name that counts. On-chain roles, the committee and other
  wallets read what an unsigned request reads.
  `access::read_file` (`services/catalyst/builder/src/access.rs`) decides;
  the table `tests/item_files.rs` holds it.
- **A public list of names opens nothing more.** `GET
  /v1/items/{address}/{id}/contents`, which answers a minted item's
  `contents` to anyone, leaves out every entry an unsigned request couldn't
  read at the file routes, and a smart wearable's kept video when it is such
  a file; one predicate (`PUBLIC_FILE` in `access.rs`) decides both. So every
  hash it names opens for the client that follows it, and
  no answer names a file the rule above keeps private.
- **A refusal reveals nothing.** A hash that isn't a canonical CID, a
  withheld hash, a private file and a hash with no file all answer the same
  404 with `Cache-Control: no-store`, and the probe answers 404 `{ok:true}`
  for each. A signature that doesn't verify answers 401 whatever the hash.
- **Bytes never become active content.** The node reads the file's type
  from its first bytes, never from its upload; a type that isn't a media
  type is sent as an attachment, and every answer carries
  `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox`.
  An unsigned answer is cached publicly for a year, since a hash names its
  bytes for good; a signed answer is `private, no-store`.

## Consequences

- No file is stored that its item doesn't name, so what an owner stores
  beyond what its items name is bounded by what is in flight.
- A hash learned from elsewhere opens nothing: naming it in an item, even a
  published one, makes it readable only when the wallet that wrote the name
  uploaded the bytes.
- A save or a delete that leaves no item naming a hash removes its upload
  records, so the file is refused at once and the collector may take the
  bytes. A record that no item names, such as an imported record whose
  item is gone or a blob of a retired scene project, stays readable by its
  uploader alone.
- The decision reads no committee and no fresh chain fact. The chain copy
  never removes a collection, so a stale copy, or one without a complete
  pass, can only leave a collection the node holds no record of unpublished
  a while longer.
- A client that shows a draft's file signs that request, so a browser
  fetches the bytes with the signature headers and shows them through an
  object URL.
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
