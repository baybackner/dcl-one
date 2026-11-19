//! The route handlers, one file per old handler module; `ping` inlined.

pub mod collections;
pub mod curation;
pub mod drafts;
pub mod linked_publication;
pub mod newsletter;
pub mod publication;
pub mod small_reads;
pub mod storage;
pub mod svg;

pub async fn ping() -> &'static str {
    "pong"
}
