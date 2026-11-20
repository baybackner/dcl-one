//! The old crate's `tests/` directory, file by file, as lib tests over the
//! process's scratch database (`CATALYST_TEST_DATABASE_URL`; skipped
//! without it) and a throwaway blob store.

pub(crate) mod support;

mod access_matrix;
mod cells;
mod chain_state;
mod curation_auth;
mod curation_auth_fuzz;
mod curation_routes;
mod curations;
mod draft_persistence;
mod forum;
mod item_files;
mod items;
mod lifecycle;
mod publication_preparation;
mod publication_transactions;
mod small_reads;
mod third_party_matrix;
mod third_party_publish;
mod upstream_routes;
mod wire_contract;
mod write_gate;
