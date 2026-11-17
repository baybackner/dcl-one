# ADR: Pulse is its own crate above Comms

- Status: Accepted
- Date: 2026-11-17

## Context

After Shop and Comms stopped waiting on Social and Explore
([ADR](2026-11-16-comms-and-shop-build-without-explore.md)), a Content edit still compiled
`content → comms → social → camera-reel → catalyst` in series. Social and Worlds start only
once Comms' metadata is written, and that metadata waits for Comms' whole analysis
(type checking, borrow checking and the monomorphization walk). Pulse was a quarter of
Comms' production code but none of its consumers below the root crate use it: Social and
Worlds reach the gatekeeper, the vocabulary and `protocol`, and only `catalyst pulse` and
the root crate's tests name `comms::pulse`. Pulse already imports nothing from the
gatekeeper and runs in its own process.

## Decision

Pulse moves from `comms::pulse` to `services/catalyst/pulse` (`catalyst-pulse`, layer 9,
beside Worlds). It depends on Comms; Comms, Worlds and Social do not depend on it, and the
root crate re-exports it as `catalyst::pulse`.

- **What stays in Comms.** The wire types and the quantized-field codecs of the Pulse
  protocol (`comms::protocol::pulse`, with `messages` and `quantize`), which Pulse
  re-exports under its old paths; the bucket constants of the two Pulse histograms the one
  exporter installer (`comms::metrics`) configures; and the statistics producer state
  (`comms::statistics`), because Comms' in-process bus fake answers producer control with
  it. Only the Pulse process constructs it in production, so Pulse remains the single
  writer of the legacy statistics population; this replaces the placement sentence of the
  [statistics ADR](2026-11-10-pulse-archipelago-statistics.md) for the producer.
- **Visibility.** Two protocol helpers Pulse calls (`SelectionRetirementLookup::valid`,
  `LegacyRoomIntent::position_update`) become public.
- **Tests.** Pulse's tests move with it. The one relay-authority test that drove Pulse's
  application relay against the gatekeeper's authority moves to Pulse, which reaches the
  authority through Comms' `test-support` constructors. Mandatory inventories name
  `catalyst_pulse` for the moved tests.

No process, pool, table, route, NATS subject, metric name or wire format is added or changed.

## Consequences

Worlds and Social start once the smaller Comms is analysed, and Pulse compiles beside them.
Debug builds (18 cores, `CARGO_BUILD_JOBS=5`, no compiler cache), medians of three
interleaved runs against the parent commit:

| Edit | Incremental | Before (median) | After (median) |
|---|---|---|---|
| `content` | off | 42.8 s | 42.1 s |
| `content` | on | 14.1 s | 13.1 s |
| a gatekeeper file | on | 11.1 s | 9.2 s |
| a Pulse file | on | 9.6 s | 3.8 s |

With incremental off, Comms writes its metadata 1.2 s sooner (19.2 s to 18.0 s after the
start of the build) and Social 1.6 s sooner (33.2 s to 31.6 s); Comms' own analysis is 12%
fewer instructions (73.1 G to 64.3 G). The whole-build difference is within the run-to-run
spread. The `content` rows with incremental on are five interleaved runs.

A Pulse edit recompiles only Pulse and the root crate.
A change to the Pulse wire types or codecs still recompiles everything above Comms.
