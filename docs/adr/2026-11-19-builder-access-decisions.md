# ADR: The builder decides access in one module and reads the current committee

- Status: Accepted
- Date: 2026-11-19

## Context

The builder serves the clients of `builder-api.decentraland.org` with the
roles `github.com/decentraland/builder-server` at `98897af` defines:

- the owner of a collection;
- an on-chain manager of a collection;
- a member of the curation committee;
- a manager of a third-party collection's provider.

Each role comes from a different source: the builder's own rows, the node's
copy of the Polygon collections subgraph, the committee as the subgraph
lists it, and the third-party registry. A route that checks its roles by
itself can forget one, read one from the request, or grant access when a
source is down.

## Decision

- **One module decides.** `services/catalyst/builder/src/access.rs` decides
  every read and write of a collection or item. It reads only the verified
  signer, the resource ids the request acts on, the builder's rows, the
  chain copy through a decision view, the committee and the third-party
  registry.
- **Grants are values.** A decision returns a value with private fields that
  only `access.rs` builds. The store functions that read or change a
  collection or item take that value, and the curation status writes take the
  admin proof, so a route that skips the decision doesn't compile. Only the
  public search of published collections, the published collections'
  contract addresses (`GET /v1/addresses`) and the file reads by hash take
  neither, by design. A write decides again inside its own transaction, after
  it locks its rows.
- **Deny by default, fail closed.** A decision grants as soon as one role
  answers yes. It refuses only when every source it needs answered no. When
  a source it needs can't tell, it answers 503 and grants nothing. A source
  is consulted only when the answer depends on it. For published content
  the committee only widens a caller's view, and that question is answered
  from the read at hand without waiting on the committee source: a good read
  while it is fresh, and while the one refresh that follows its expiry runs.
  A failed refresh drops an expired read. With no good read at hand (before
  the first read succeeds, or once a refresh after the hour has failed), a
  committee member reads published content as any signer does.
- **No connection across an outbound read.** A decision reads the builder's
  rows and the chain copy, returns its database connection, and only then
  waits on the committee. It reads the registry before it takes a
  connection, and a write decides again inside its transaction from the
  same registry read.
- **One owner.** A standard collection's owner is its chain creator once the
  copy holds it, and its stored `eth_address` before. The owner is derived on
  each request, so `POST /v1/collections/{id}/owner/sync` stores nothing and
  answers the collection's body.
- **The lifecycle routes.** Only the owner of a standard collection sets its
  lock. The owner, an on-chain manager or the provider's manager deletes a
  collection and accepts its terms of service. Those, or a current committee
  member, publish a standard collection and sync its owner; the committee's
  admission stands when the publish decides again inside its transaction.
- **Managers come from the chain.** Only a collection on chain has managers,
  read from the copy. A request body never sets them.
- **Current committee, read once an hour.** The builder reads the current
  committee from the Polygon collections subgraph (`accounts` with
  `isCommitteeMember`) through the chain index's subgraph client: its SSRF
  guard, response cap and retries, under a 10-second bound. One read answers
  for an hour. At most one read runs at a time, in a task of its own that
  keeps its answer, a failure or a panic included, even when every caller
  waiting on it went away; concurrent requests share it. A failed
  read, or a node without `MATIC_COLLECTIONS_SUBGRAPH_URL`, can't tell: that
  answer lasts 30 seconds, and every decision the committee could change
  answers 503 until a read succeeds. `GET /v1/committee` answers from the
  same read. Decisions that only the committee can grant use a fresh read
  only.
- **Third-party managers.** A third-party collection's provider is the
  provider id of its URN. Its managers, in the order
  `github.com/decentraland/builder-server` reads its sources:
  - once the registry approves the provider, the managers the registry
    lists;
  - before approval, the managers the registry lists that the provider's
    virtual third party also lists, or every manager the registry lists
    when the provider has no virtual third party;
  - with no registry record, the virtual third party's managers;
  - nobody otherwise.

  The wallet that created a third-party collection decides nothing; a
  third-party collection has no owner. A provider's managers read, save,
  delete and accept the terms of every collection and item of the provider,
  and read the files those items name in a name whose writer holds the
  bytes. The node's own publication of a third-party collection also
  requires its stored creator.
- **Third-party creates.** A save that creates a third-party collection
  under a provider the registry or a virtual third party has is a
  provider manager's. Under a provider neither has, any signer creates it,
  and the save creates the provider's virtual third party with the signer
  as its only manager, in the same transaction.
- **The registry, read live.** The builder reads the whole third-party
  registry (`thirdParties`, every provider, approved or not, with its
  managers, merkle root, item allowance, approval, kind and metadata)
  through the chain index's subgraph client, under a 10-second bound, from
  `THIRD_PARTY_REGISTRY_L2_SUBGRAPH_URL`. A decision accepts only a read
  that started after the decision began. One read runs at a time and every
  decision waiting when it started shares it. A read that fails, an answer
  that breaks the expected shape, or a node without the setting can't tell:
  that answer lasts 30 seconds, and every decision, listing and view that
  involves a third-party collection answers 503 until a read succeeds.
- **Access checks never write.** A decision reads its sources and changes no
  row, also when it refuses.
- **Curations.** A collection's curations, and an item's, are read and
  written by a wallet with a role on the collection (for an item's, the
  item's own owner instead of the collection's owner) or by a current
  committee member. The collection must exist (404) and be published: a
  standard one on chain, a third-party one with an item curation (409);
  only then are the roles asked, so a stranger gets 401. The committee is
  asked only when no role answers, and every committee question here waits
  for a fresh read: while the committee can't be read, the committee's
  curation listing and every decision only the committee can make answer
  503, never a narrower view. Which status change each caller may make is
  one table (`builder/src/curation/transitions.rs`); a write asks it again
  inside its transaction, with the collection row locked.
- **Admins.** `BUILDER_ADMIN_ADDRESSES` and the admin bearer open the
  operator's item status routes only; they grant no role on collections,
  items or their curations.

## Consequences

- A wallet the registry stops listing as a provider's manager is refused
  on its next request once the registry subgraph has indexed the change. A
  request that involves a third-party collection waits for a registry read
  that starts after it arrives.
- While the registry can't tell, every request that involves a third-party
  collection answers 503 and writes nothing, an address's own listings
  included; requests about standard collections only are unaffected.
- A wallet that a virtual third party lists and that the registry doesn't
  list for the same unapproved provider manages nothing of it, so a wallet
  that claims a provider id before another wallet registers it on chain
  keeps no access once the registry has the record.
- After the builder's last successful read, a wallet removed from the
  committee:
  - can still be granted a decision only the committee can grant for up to
    one hour;
  - can still get the committee's wider view of published content (a
    published collection's unminted items, an item's `local_content_hash`)
    for that hour, and then only while the one refresh that follows the
    hour runs, which ends within the read's 10-second bound. That refresh
    starts at the first request after the hour, which the removed wallet's
    own request can be.
- A wallet added to the committee waits up to the same hour.
- While the subgraph is down, a request that only the committee could grant
  answers 503 rather than 401, and `GET /v1/committee` answers 503 rather
  than an empty list. Published content stays readable as any signer reads
  it, a committee member included.
- Every by-id read is a decision, so it answers 503 when the chain copy is
  older than its decision bound, as writes do, with one exception: a
  reader whose answer no chain fact can change beyond what it already holds
  (a manager of a third-party collection's provider, or the stored owner of
  a standard draft whose contract the copy doesn't hold) reads it in full
  from a copy that has a complete pass of any age, as its own listing
  already answers it.
- Between a creator transfer and the copy's next pass, the previous creator
  keeps the owner's access.
- A wallet the committee no longer lists reads and changes no curation,
  even one it is assigned to or decided; a committee member who also holds a
  role reads and writes as that role until the table asks for the
  committee.
- A new role or source is one change to `access.rs`, which the authorization
  matrix test (`builder/src/tests/access_matrix.rs`) covers for every routed
  operation.

## Alternatives considered

- **A check in each route.** Nothing stops a route from skipping a role or
  admitting a caller by a value the request supplied.
- **A committee read per decision.** Every decision would depend on the
  subgraph's latency, and an outage would turn each request into an outbound
  call.
- **The committee as every wallet that ever curated.** The subgraph keeps
  past curators, so removed members would keep the role for good.
- **A registry read kept for a time, or one read per provider.** A kept read
  would let a removed manager keep access for its lifetime, and would break
  the old builder web's wait for a new merkle root. Reads keyed by provider
  would let a caller cycling provider ids send one subgraph query per id;
  the whole registry is small enough to read at once.
- **The virtual third party alone before approval.** It would leave the
  provider to whichever wallet claimed its id first, until approval.
