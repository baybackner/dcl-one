# Builder conformance

These files compare the node's builder routes with
`github.com/decentraland/builder-server` at
`98897af692f26063a088aebb2ea5bb3f683168a2`. Each capture is a request and
the answer the pinned upstream gave to it. The node replays every capture
and the parity ledger records, per operation, what the replay shows.

## Files

The folder holds the ledger, the seed both sides start from, and the
captures.

| Path | Holds |
| --- | --- |
| `parity.json` | The per-operation parity ledger |
| `seed.json` | The synthetic wallets and collections both sides start from |
| `source/*.json` | Captures of the pinned upstream, written by `tooling/builder-server-capture.cjs` |
| `live/*.json` | Dated captures of the public service; none are recorded |

Captures use the shared conformance fixture format
(`services/catalyst/testing/src/conformance/fixture.rs`): `request` with
`method`, `path`, `query`, `headers` and `body`; `response` with `status`,
the kept headers and `body_json` or `body_bytes_b64`; `captured_from`,
`captured_at` and `volatile_paths`. A volatile leaf is recorded as
`<<VOLATILE>>`, which matches any value on replay. A capture whose
volatile timestamps were equal before masking lists them in
`volatile_equal`. No other marker may appear in a builder capture: a
`<<REDACTED` string anywhere, or a `<<VOLATILE>>` that none of its
`volatile_paths` reaches, refuses the capture. A capture whose signature is the point of the case
carries `sign`, the one defect it was signed with (below); the replay
signs it again at replay time with the same defect, so the node's
refusal comes from the check the case names. A capture of literal headers
is marked `"auth": "recorded"` and replayed with its own `x-identity-*`
headers. A capture carries one or the other, `sign` must name a recorded
signer and only the fields below, and `auth` takes no other value. `sign`
names a defect: a non-zero `offset`, a `path`, a `method` or a
`sentMetadata` (with the signed `metadata`), or `untimestamped`, which
takes no `path`, `offset`, `metadata` or `sentMetadata`; `withHeaders` goes
only with `untimestamped`. The capture script refuses a case that breaks
these rules, as the ledger test refuses such a capture.

## Parity ledger

`parity.json` lists every route of the route ledger
(`services/catalyst/builder/src/tests/upstream_routes.rs`) that is `Routed`
or `Todo`. Its schema is the social family's (`../social/parity.json`): each operation has
`id` (`api_http:<METHOD>:/v1<path>`), `owner`, `status`, `result`,
`evidence`, `gaps` and `remaining_verification`.

- `not_reviewed`: the captures it cites, if any, don't settle the
  operation.
- `known_gap`: a capture replays with a difference. `gaps` states each
  difference in prose.
- `reviewed`: it cites at least one capture and every capture it cites
  replays with no difference. It covers the cases those captures exercise,
  not every input.

`deliberate_gaps` states, once each, a difference the node keeps on
purpose, by name. An operation lists it among its `gaps` as
`deliberate:<name>`. Every reference must name a stated gap, and every
stated gap must be referenced.

An evidence entry whose path is in this folder must be a capture directly in
`source/` or `live/` whose request method and path match the operation (a
`:name` segment matches one non-empty segment; a `/builder-api` prefix, a
query string and a trailing slash are ignored), and must carry `replay`: the
exact notes the replay produces for it, empty for a match. A capture whose
request matches more than one in-scope operation is refused. A capture's
`volatile_paths` may only end in a field named in `volatile_fields`
(`created_at`, `updated_at`), and the recorded value there must be
`<<VOLATILE>>`. A note is `status: ...`,
`header ...`, a body path with `missing`, `extra` or `value`, `body bytes
differ: ...` or `candidate body is not JSON: ...`, as
`social::test_fixtures::replay_signed` words them.

## Tests

The tests in `services/catalyst/tests/builder_conformance.rs` (test group
`node_1`) hold the ledger to the captures and the route ledger.

- `the_parity_ledger_names_every_in_scope_route` checks that the parity
  ledger and the route ledger name the same in-scope operations, that every
  status has its evidence or gaps, that every cited file exists, that every
  JSON file in this folder other than `parity.json` and `seed.json` is a
  capture directly in `source/` or `live/`, and that every capture is cited
  by exactly one operation with its `replay` notes. The tests named
  `a_..._is_refused` hold each of those rules against a ledger built in the
  test.
- `the_builder_captures_replay_against_the_mounted_router` seeds a scratch
  database from `seed.json`, builds the builder routers as `serve` composes
  them (at the root and under `/builder-api`), replays every capture in file
  name order and requires each one's notes to equal its `replay` list. A
  `GET` or `HEAD` capture is also replayed under `/builder-api` and must
  produce the same notes. A signed capture is signed again for the replay by
  the same wallet, over the path it is sent to, with its recorded metadata,
  a delegated key valid until 2099 and a timestamp of now, plus its `sign`
  defect; one marked `"auth": "recorded"` is sent as recorded. It
  needs `CATALYST_TEST_DATABASE_URL` and does nothing without it. The outer
  layers `serve` adds around every service are not part of the replay.
  The builder replay compares every key, `id` and `url` included (the
  social replays keep the shared differ's list of keys it never compares).
  Beyond the differ's notes, it adds `<path> volatile value is not a
  timestamp` when the node answers anything but a string or a number at a
  volatile path, and `<a> and <b> differ` when the node breaks a recorded
  `volatile_equal` group.

`seed.json` accepts only the fields both seeders apply; any other field
fails the replay test with its name. `collections` are the builder's rows,
each with the contract address its draft computed, and a third-party one with
its provider id and URN suffix. `items` are the builder's item rows.
`third_parties` are providers neither registry lists: the capture inserts
them as upstream's virtual third parties; the node reads no registry yet and
admits the wallet that owns a third-party collection as its provider's
manager, so the replay requires each provider's managers to be exactly that
wallet. `chain` names, once, what
the Polygon collections subgraph holds: the committee's roles, its
collections (contract, name, creator and owner roles, approval, manager and
minter roles and unix timestamps) and their items. The capture serves
`chain` from its fake subgraph to upstream; the replay writes the same
collections and items into the node's copy of that subgraph
(`shop.collections` and `shop.items`), records a fresh complete pass of both
mirrors in `shop.mirror_state` and serves the committee to the node's reader
from a loopback fake subgraph, so a collection is published, and a wallet is
on the committee, on both sides exactly when `chain` says so. The fake
third-party registry lists no provider.

```sh
cd services/catalyst
CATALYST_TEST_JOBS=4 cargo test --test node_1 -- builder_conformance::
```

## Capturing the pinned upstream

`tooling/builder-server-capture.cjs` runs upstream's own Express app and
records the cases listed in its `CASES` table. It needs Node 24; the
committed captures were written with Node 24.22.3, the version upstream's
`Dockerfile` pins. It shares its checkout and database-URL checks with the
social captures (`tooling/social-capture-common.cjs`).

The script refuses a checkout that isn't the Git working tree root, isn't
at the pinned revision, has staged, unstaged or untracked changes, or whose
`node_modules` doesn't match `package-lock.json` as `npm ci` installs it.
Then it:

1. Creates its own database, `builder_capture_<pid>_<random>`, on the
   cluster `CATALYST_TEST_DATABASE_URL` names, and drops it at the end of
   the run, also when the capture or its teardown fails, and on `SIGINT` or
   `SIGTERM`, including one that arrives while the database is being
   created; a second signal waits for the same drop. An uncaught error is
   reported redacted after the same drop. Upstream runs with that URL's credentials, so use a role that
   can create databases and owns nothing else.
2. Runs upstream's migrations and inserts the rows of `seed.json`.
3. Starts a loopback fake for the collections and third-party subgraphs,
   which answers the collections subgraph's `accounts`, `collections` and
   `items` fields with the `seed.json` `chain` rows their `where` argument
   selects (by `id`, `id_in`, `creator`, `managers_contains`,
   `minters_contains`, `collection` or `isCommitteeMember`; a collection
   carries its items) and the third-party subgraph's fields with an empty
   list. It answers the catalyst's active-entities read with no entities,
   and points the peer, RPC, forum, warehouse, S3, SNS and OpenSea settings
   at it. Slack is off and the feature flags are set off
   through their `FF_*` overrides.
4. Loads `tooling/builder-server-offline.cjs` into its own process and,
   with `--require`, into the migration process: proxy variables are
   removed, and a TCP connection to anything but localhost or a loopback
   address is refused, as is a `dns.lookup` of any other hostname. The
   capture fails if anything was refused, if upstream asked the fake for
   another subgraph field or filtered by a `where` key the fake doesn't
   apply, or if it called a fake route other than the subgraphs and the
   active-entities read. A case
   that needs another answer adds it to the fake first. The guard covers
   `net` connections and `dns.lookup` on the main thread of the capture
   process and on every thread of the migration process. It
   doesn't cover worker threads the capture process starts, DNS queries sent
   through `dns.resolve*` or `dns.promises` (a TCP connection they lead to is
   still refused), UDP, other child processes or native code.
5. Fixes the clock at `2026-01-01T00:00:00.000Z`, so timestamps and
   signatures are the same on every run. Values the database clock sets are
   listed in the case's `volatile` paths. A volatile path must end in a
   field named in `parity.json` `volatile_fields` and match a recorded
   value; the replay test holds committed captures to the same rule. The
   capture fails if upstream's answer or the request already holds a
   marker, and records which volatile values were equal before masking.
6. Signs a case with a `signer` over the timestamped payload and its
   `metadata` (`{}` by default), with the one defect its `sign` names:
   `offset` moves the timestamp by that many milliseconds, `path` or
   `method` signs another one, `sentMetadata` sends other metadata than was
   signed, and `untimestamped` signs only the method and the path below
   `/v1`, with no timestamp or metadata header, or with both when
   `withHeaders` is set. The capture keeps `sign`, with the signed
   `metadata` for a timestamped case. A case's `headers` sends literal
   headers and marks the capture `"auth": "recorded"`. The wallets are
   derived from the public labels
   `dcl-one builder conformance <role>` and `... <role> ephemeral`
   (keccak-256 of the label is the private key). `seed.json` records each
   role's address and both the script and the replay check it. Anyone can
   derive these keys: never configure the addresses as an admin or committee
   member anywhere.
7. Writes the captures to a staging folder beside the output folder and
   moves them into it only after every check passed, so a failed run leaves
   the output untouched.

To capture, install upstream's locked dependencies in a scratch checkout
and write into an empty folder:

```sh
git clone --no-checkout https://github.com/decentraland/builder-server.git "$SCRATCH/builder-server"
git -C "$SCRATCH/builder-server" checkout --detach 98897af692f26063a088aebb2ea5bb3f683168a2
(cd "$SCRATCH/builder-server" && npm ci --ignore-scripts)
CATALYST_TEST_DATABASE_URL=<disposable-cluster-url> \
  node tooling/builder-server-capture.cjs "$SCRATCH/builder-server" "$SCRATCH/out"
diff -r "$SCRATCH/out" services/catalyst/tests/fixtures/conformance/builder/source
```

A run reproduces the committed captures byte for byte. Compare its output
with `source/` before replacing anything.

The script's own tests run on any Node version, in the `Social source
captures` workflow:

```sh
node --test tooling/tests/builder-server-capture.test.cjs
```

### Adding a capture

A new case goes through the script, the ledger and the replay in that
order.

1. Add the case to `CASES` in `tooling/builder-server-capture.cjs`: a
   `name` with the next number prefix, a `description`, `method`, `path`,
   optional `query`, `signer` (a `seed.json` role), `metadata`, `sign`,
   `headers`, `body` and `volatile`.
   Add any rows it needs to `seed.json`, and teach both seeders (the
   script's `seedDatabase` and `SEED_KEYS`, the test's `Seed` and `seed`)
   the new field. If upstream asks the fake for a new subgraph field, add
   its answer to the fake.
2. Run the capture and copy the new file into `source/`.
3. Run the tests. The ledger test names the uncited capture and the replay
   test prints the notes it produces.
4. Cite the capture in its operation's `evidence` with a `scope` and the
   `replay` notes, then set the status: `reviewed` when the notes are empty
   and the operation's cases are covered, `known_gap` with a `gaps` entry for
   each difference otherwise.

### Marking a gap

Record a difference where the replay shows it.

1. Copy the replay's notes into the capture's `replay` list unchanged.
2. Set the operation's `status` to `known_gap` and state each difference in
   `gaps`: what upstream answers and what the node answers. A difference the
   node keeps on purpose is stated in `deliberate_gaps` and referenced as
   `deliberate:<name>`.
3. When a change makes the node match, the replay test fails on that
   capture; empty its `replay` list and update the status in the same
   change.

## Dated captures of the public service

A capture of `builder-api.decentraland.org` goes in `live/` in the same
format, with `captured_from` set to the full URL and `captured_at` to the
UTC time of the request. Capture only unsigned reads of public data, one
request at a time with a pause between them, and replace every address and
name that identifies a person with a synthetic one before committing. Mark
fields that depend on production data as volatile. The replay test reads
`live/` like `source/`, so each dated capture is cited by one operation
with its `replay` notes.
