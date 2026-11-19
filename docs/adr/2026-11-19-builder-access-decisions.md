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
  chain copy through a decision view, and the committee.
- **Grants are values.** A decision returns a value with private fields that
  only `access.rs` builds. The store functions that read or change a
  collection or item take that value, and the curation status writes take the
  admin proof, so a route that skips the decision doesn't compile. Only the
  public search of published collections and the file reads by hash take
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
  waits on the committee.
- **One owner.** A standard collection's owner is its chain creator once the
  copy holds it, and its stored `eth_address` before.
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
- **Third-party managers.** Until the builder reads the third-party
  registry, a third-party collection's provider manager is the wallet that
  created the collection.
- **Access checks never write.** A decision reads its sources and changes no
  row, also when it refuses.
- **Admins.** `BUILDER_ADMIN_ADDRESSES` and the admin bearer open the
  curation routes only; they grant no role on collections or items.

## Consequences

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
  older than its decision bound, as writes do, with one exception: the
  stored owner of a draft whose answer no chain fact can change beyond
  what the owner already holds (a third-party draft, or a standard draft
  whose contract the copy doesn't hold) reads it in full
  from a copy that has a complete pass of any age, as the owner's own
  listing already answers it.
- Between a creator transfer and the copy's next pass, the previous creator
  keeps the owner's access.
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
