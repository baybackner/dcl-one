//! Pulse, the realm-wide real-time layer (port of decentraland/Pulse): ENet/UDP
//! and WebTransport sessions, the handshake and hardening policies, the
//! per-peer snapshot boards, area-of-interest simulation, peer clustering and
//! the cluster feed archipelago's ws-connector consumes.
//! It is its own crate above `catalyst-comms`, so worlds and social never wait
//! on it to compile; only the root crate links it. The loop is latency-sensitive
//! and owns a UDP socket, so it runs as `catalyst pulse` in its own process
//! ([`env::run_from_env`]), with the `PULSE_*` environment contract the old
//! executable documented ([`env::ENV_DOCS`]). Pulse persists nothing and
//! imports nothing from the gatekeeper: the admission proof of the
//! application relay is minted and verified by comms from the claim/proof
//! types in [`crate::protocol::room_authority`].
//!
//! External runtimes stay separate processes: the NATS broker carrying the
//! cluster feed (`PULSE_NATS_URL`; blank leaves the tracker in stats-only
//! mode). The application relay implementation and room-authority boundary
//! remain available internally; production has no relay activation or admission capability.

// Production routes go through `shared::routing` (pulse/clippy.toml); test routers may use axum's.
#![cfg_attr(not(test), deny(clippy::disallowed_methods))]
#![cfg_attr(test, allow(clippy::disallowed_methods))]

// The crates this one builds on, under their module names (AGENTS.md).
pub(crate) use catalyst_comms as comms;
#[cfg(test)]
pub(crate) use catalyst_comms::gatekeeper;
pub(crate) use catalyst_comms::interfaces;
pub(crate) use catalyst_comms::protocol;
pub(crate) use catalyst_comms::statistics;
#[cfg(test)]
pub(crate) use catalyst_comms::stats;
pub(crate) use catalyst_comms::topology;
pub(crate) use catalyst_shared as shared;
pub(crate) use catalyst_shared::identity::Address;
pub(crate) use dcl_auth as auth;
pub(crate) use dcl_http as http;

pub mod application_relay;
pub mod bans;
pub mod batch;
pub mod cluster;
pub mod env;
pub mod flags;
#[cfg(test)]
pub mod fuzz;
pub mod handshake;
pub mod hardening;
pub mod interest;
pub mod ip_limiter;
pub mod metrics;
pub use crate::protocol::pulse::{messages, quantize};
pub mod realm_grids;
pub mod server;
pub mod simulation;
pub mod snapshot;
pub mod transport;

pub use crate::protocol::pulse::decentraland;
pub use crate::protocol::pulse::decentraland::pulse::PeerClusterChange;
pub use cluster::{ClusterOptions, ClusterPass, ClusterSession, ClusterTracker};
pub use env::run_from_env;
pub use handshake::{HandshakeError, VerifiedHandshake, verify_handshake};
pub use realm_grids::RealmSpatialGrids;
pub use server::PulseServer;

/// Production routes stay on `shared::routing` (see `shared::routing::guard`).
#[cfg(test)]
#[test]
fn production_routes_use_shared_routing() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = crate::shared::routing::guard::direct_axum_routes(&src);
    assert!(
        found.is_empty(),
        "use crate::shared::routing instead of axum's method routers at {found:?}"
    );
}
