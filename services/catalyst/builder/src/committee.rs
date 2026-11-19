//! The current curation committee: the accounts the Polygon collections
//! subgraph marks `isCommitteeMember`, as builder-server reads them for
//! `isCommitteeMember` and `GET /v1/committee`. The read goes through the
//! shared subgraph client (`chain_index::graph::graph_query`: the SSRF guard,
//! the body cap and the retry ladder) under an overall [`FETCH_TIMEOUT`].
//!
//! One good read is kept [`MEMBERS_TTL`], so a wallet removed from the
//! committee keeps the role for up to that long. A read that fails, or a node
//! with no Polygon collections subgraph, can't tell who is on the committee:
//! that answer is kept [`FAILURE_TTL`] and is a 503, never an empty committee
//! ([`crate::kept`]: one read at a time, kept even when its callers went
//! away). [`Committee::members`] waits for it; [`Committee::known_member`]
//! never does.

use std::sync::Arc;
use std::time::Duration;

use dcl_auth::is_eth_address;
use serde::Serialize;
use serde_json::{Value, json};

use crate::chain_index::ChainIndex;
use crate::chain_index::graph::{THE_GRAPH_PAGE_SIZE, graph_query};
use crate::http::ApiError;
use crate::kept::Kept;
pub use crate::kept::{FAILURE_TTL, FETCH_TIMEOUT};

/// How long a successful read answers (builder-server's hour).
pub const MEMBERS_TTL: Duration = Duration::from_secs(60 * 60);

/// One member as `GET /v1/committee` answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Member {
    pub id: String,
    pub address: String,
}

pub type Members = Arc<Vec<Member>>;

/// The committee read and its cache.
pub struct Committee {
    kept: Kept<Members>,
}

impl Default for Committee {
    fn default() -> Self {
        Self {
            kept: Kept::new(MEMBERS_TTL),
        }
    }
}

impl Committee {
    /// The current members, waiting for a read when none is kept; 503 when
    /// the subgraph can't tell.
    pub async fn members(&self, market: Option<&Arc<ChainIndex>>) -> Result<Members, ApiError> {
        self.kept
            .get(|| fetch(market.cloned()))
            .await
            .ok_or_else(unavailable)
    }

    /// Whether `address` is a current member; 503 when the subgraph can't
    /// tell.
    pub async fn is_member(
        &self,
        market: Option<&Arc<ChainIndex>>,
        address: &str,
    ) -> Result<bool, ApiError> {
        Ok(contains(&self.members(market).await?, address))
    }

    /// Whether a good read shows `address` on the committee, without
    /// waiting ([`Kept::known`]); false when no good read is at hand (none
    /// yet, or the source failing). Starts the read that is due.
    pub fn known_member(&self, market: Option<&Arc<ChainIndex>>, address: &str) -> bool {
        self.kept
            .known(|| fetch(market.cloned()))
            .is_some_and(|members| contains(&members, address))
    }

    #[cfg(test)]
    pub(crate) async fn refresh_with(
        &self,
        read: impl std::future::Future<Output = Option<Members>> + Send + 'static,
    ) {
        self.kept.refresh_with(read).await;
    }

    #[cfg(test)]
    pub(crate) fn backdate(&self, by: Duration) {
        self.kept.backdate(by);
    }
}

fn contains(members: &Members, address: &str) -> bool {
    members
        .iter()
        .any(|member| member.address.eq_ignore_ascii_case(address))
}

fn unavailable() -> ApiError {
    ApiError::service_unavailable("The curation committee is not available. Try again later.")
}

const QUERY: &str = "query committee($idFrom: ID!, $first: Int!) {\
  accounts(first: $first, where: { isCommitteeMember: true, id_gt: $idFrom }, orderBy: id, orderDirection: asc) { id address }\
}";
const MAX_PAGES: usize = 10;

async fn fetch(market: Option<Arc<ChainIndex>>) -> Option<Members> {
    let market = market?;
    let url = market.settings.matic_collections_subgraph_url.as_deref()?;
    let error = match tokio::time::timeout(FETCH_TIMEOUT, walk(&market, url)).await {
        Ok(Ok(members)) => return Some(Arc::new(members)),
        Ok(Err(error)) => error,
        Err(_) => "timed out".to_string(),
    };
    tracing::warn!(error = %error, "builder committee read failed; committee decisions answer 503");
    None
}

async fn walk(market: &ChainIndex, url: &str) -> Result<Vec<Member>, String> {
    let mut members = Vec::new();
    let mut id_from = String::new();
    for _ in 0..MAX_PAGES {
        let data = graph_query(
            market,
            url,
            QUERY,
            json!({ "idFrom": id_from, "first": THE_GRAPH_PAGE_SIZE }),
        )
        .await?;
        let page = data
            .get("accounts")
            .and_then(Value::as_array)
            .ok_or("subgraph answered no accounts")?;
        for account in page {
            let (Some(id), Some(address)) = (
                account.get("id").and_then(Value::as_str),
                account.get("address").and_then(Value::as_str),
            ) else {
                return Err("subgraph answered an account without id or address".into());
            };
            if !is_eth_address(address) {
                return Err("subgraph answered an account that is not an address".into());
            }
            members.push(Member {
                id: id.to_string(),
                address: address.to_ascii_lowercase(),
            });
        }
        if page.len() < THE_GRAPH_PAGE_SIZE as usize {
            return Ok(members);
        }
        id_from = members.last().map(|m| m.id.clone()).unwrap_or_default();
    }
    Err(format!("committee longer than {MAX_PAGES} pages"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::support::{FakeCommittee, committee_index, market_without_db};

    const MEMBER: &str = "0x00000000000000000000000000000000000000c1";
    const OTHER: &str = "0x00000000000000000000000000000000000000c2";

    #[tokio::test]
    async fn the_current_committee_is_read_once_and_kept_an_hour() {
        let fake = FakeCommittee::with(&[MEMBER, OTHER]);
        let index = committee_index(&fake).await;
        let committee = Committee::default();
        assert_eq!(
            committee.members(Some(&index)).await.unwrap().as_ref(),
            &vec![
                Member {
                    id: MEMBER.into(),
                    address: MEMBER.into()
                },
                Member {
                    id: OTHER.into(),
                    address: OTHER.into()
                },
            ]
        );
        assert!(
            committee
                .is_member(Some(&index), &MEMBER.to_uppercase().replace("0X", "0x"))
                .await
                .unwrap()
        );
        assert!(
            !committee
                .is_member(Some(&index), "0x00000000000000000000000000000000000000c3")
                .await
                .unwrap()
        );
        assert_eq!(fake.hits(), 1, "one read answers for the hour");
        fake.set(Some(&[OTHER]));
        committee.backdate(MEMBERS_TTL - Duration::from_secs(1));
        assert!(committee.is_member(Some(&index), MEMBER).await.unwrap());
        committee.backdate(Duration::from_secs(2));
        assert!(
            !committee.is_member(Some(&index), MEMBER).await.unwrap(),
            "a removed member loses the role after the hour"
        );
        assert_eq!(fake.hits(), 2);
    }

    #[tokio::test]
    async fn a_failed_read_cannot_tell_and_waits_before_the_next_one() {
        let fake = FakeCommittee::down();
        let index = committee_index(&fake).await;
        let committee = Committee::default();
        for _ in 0..3 {
            assert_eq!(
                committee
                    .is_member(Some(&index), MEMBER)
                    .await
                    .unwrap_err()
                    .status(),
                503
            );
        }
        assert_eq!(
            fake.hits(),
            1,
            "an outage is not one outbound call per request"
        );
        fake.set(Some(&[MEMBER]));
        committee.backdate(FAILURE_TTL + Duration::from_secs(1));
        assert!(committee.is_member(Some(&index), MEMBER).await.unwrap());
        assert_eq!(fake.hits(), 2);
    }

    async fn concurrent_reads(committee: &Arc<Committee>, index: &Arc<ChainIndex>) {
        let mut reads = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let (committee, index) = (committee.clone(), index.clone());
            reads.spawn(async move { committee.is_member(Some(&index), MEMBER).await.ok() });
        }
        while reads.join_next().await.is_some() {}
    }

    #[tokio::test]
    async fn concurrent_reads_at_expiry_or_in_an_outage_share_one_outbound_call() {
        let fake = FakeCommittee::with(&[MEMBER]);
        fake.slow(Duration::from_millis(300));
        let index = committee_index(&fake).await;
        let committee = Arc::new(Committee::default());
        concurrent_reads(&committee, &index).await;
        assert_eq!(fake.hits(), 1, "the first reads share one call");
        committee.backdate(MEMBERS_TTL + Duration::from_secs(1));
        concurrent_reads(&committee, &index).await;
        assert_eq!(fake.hits(), 2, "an expiry refreshes once");
        fake.set(None);
        committee.backdate(MEMBERS_TTL + Duration::from_secs(1));
        concurrent_reads(&committee, &index).await;
        assert_eq!(
            fake.hits(),
            3,
            "an outage costs one call, not one per request"
        );
    }

    #[tokio::test]
    async fn a_read_its_callers_abandon_still_completes_once_and_is_kept() {
        let fake = FakeCommittee::with(&[MEMBER]);
        fake.slow(Duration::from_millis(800));
        let index = committee_index(&fake).await;
        let committee = Committee::default();
        for _ in 0..10 {
            let abandoned =
                tokio::time::timeout(Duration::from_millis(50), committee.members(Some(&index)))
                    .await;
            assert!(abandoned.is_err(), "the caller gave up first");
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(fake.hits(), 1, "ten abandoned callers cost one read");
        let started = std::time::Instant::now();
        assert!(committee.is_member(Some(&index), MEMBER).await.unwrap());
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "the read was kept"
        );
        assert_eq!(fake.hits(), 1);
    }

    #[tokio::test]
    async fn a_refresh_that_panics_leaves_the_failure_kept() {
        let fake = FakeCommittee::with(&[MEMBER]);
        let index = committee_index(&fake).await;
        let committee = Committee::default();
        committee
            .refresh_with(async { panic!("the read failed in a way nobody expected") })
            .await;
        let answer = tokio::time::timeout(Duration::from_secs(1), committee.members(Some(&index)))
            .await
            .expect("never stuck in flight");
        assert_eq!(answer.unwrap_err().status(), 503);
        assert_eq!(fake.hits(), 0, "the failure is kept for its window");
    }

    #[tokio::test]
    async fn the_widening_question_never_waits_on_the_source() {
        let fake = FakeCommittee::with(&[MEMBER]);
        fake.slow(Duration::from_millis(800));
        let index = committee_index(&fake).await;
        let committee = Committee::default();
        let quick = |committee: &Committee| {
            let started = std::time::Instant::now();
            let known = committee.known_member(Some(&index), MEMBER);
            assert!(started.elapsed() < Duration::from_millis(50));
            known
        };
        assert!(!quick(&committee), "cold start: nobody is known yet");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(fake.hits(), 1, "the question started the one read");
        assert!(quick(&committee), "a good read answers");
        committee.backdate(MEMBERS_TTL + Duration::from_secs(1));
        assert!(
            quick(&committee),
            "an expired read answers while its refresh runs"
        );
        assert!(quick(&committee));
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(fake.hits(), 2, "one refresh at expiry");
        fake.set(None);
        committee.backdate(MEMBERS_TTL + Duration::from_secs(1));
        assert!(
            quick(&committee),
            "the refresh in flight keeps the last good read"
        );
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            !quick(&committee),
            "a failing source: not known to be on it"
        );
        assert_eq!(fake.hits(), 3, "no refresh during the failure window");
    }

    #[tokio::test]
    async fn an_expired_read_widens_only_during_the_one_refresh_after_its_expiry() {
        let fake = FakeCommittee::with(&[MEMBER]);
        let index = committee_index(&fake).await;
        let committee = Committee::default();
        assert!(committee.is_member(Some(&index), MEMBER).await.unwrap());
        fake.set(None);
        fake.slow(Duration::from_millis(300));
        committee.backdate(Duration::from_secs(7 * 24 * 60 * 60));
        assert!(
            committee.known_member(Some(&index), MEMBER),
            "the refresh that follows the expiry may still use the read"
        );
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            !committee.known_member(Some(&index), MEMBER),
            "after the failed refresh"
        );
        for round in 2..4 {
            committee.backdate(FAILURE_TTL + Duration::from_secs(1));
            assert!(
                !committee.known_member(Some(&index), MEMBER),
                "round {round}: a later refresh never revives the expired read"
            );
            tokio::time::sleep(Duration::from_millis(600)).await;
            assert!(
                !committee.known_member(Some(&index), MEMBER),
                "round {round}: after it"
            );
            assert_eq!(
                fake.hits(),
                round + 1,
                "the first read, then one refresh per round"
            );
        }
    }

    #[tokio::test]
    async fn a_fresh_read_survives_a_failed_refresh_and_a_good_one_replaces_it() {
        let fake = FakeCommittee::with(&[MEMBER]);
        let index = committee_index(&fake).await;
        let committee = Committee::default();
        assert!(committee.is_member(Some(&index), MEMBER).await.unwrap());
        committee.refresh_with(async { None }).await;
        assert!(committee.known_member(Some(&index), MEMBER), "still fresh");
        let replaced = Arc::new(vec![Member {
            id: OTHER.into(),
            address: OTHER.into(),
        }]);
        committee.refresh_with(async move { Some(replaced) }).await;
        assert!(!committee.known_member(Some(&index), MEMBER));
        assert!(committee.known_member(Some(&index), OTHER));
    }

    #[tokio::test]
    async fn a_malformed_account_cannot_tell() {
        let fake = FakeCommittee::with(&["not an address"]);
        let index = committee_index(&fake).await;
        assert_eq!(
            Committee::default()
                .members(Some(&index))
                .await
                .unwrap_err()
                .status(),
            503
        );
    }

    #[tokio::test]
    async fn every_page_of_the_committee_is_read() {
        let addresses: Vec<String> = (0..1001).map(|i| format!("0x{i:040x}")).collect();
        let refs: Vec<&str> = addresses.iter().map(String::as_str).collect();
        let fake = FakeCommittee::with(&refs);
        let index = committee_index(&fake).await;
        let members = Committee::default().members(Some(&index)).await.unwrap();
        assert_eq!(members.len(), 1001);
        assert_eq!(members[1000].address, addresses[1000]);
        assert_eq!(fake.hits(), 2);
    }

    #[tokio::test]
    async fn without_a_polygon_collections_subgraph_the_committee_cannot_tell() {
        let committee = Committee::default();
        assert_eq!(committee.members(None).await.unwrap_err().status(), 503);
        let index = market_without_db(crate::chain_index::IndexSettings::for_tests());
        assert_eq!(
            Committee::default()
                .members(Some(&index))
                .await
                .unwrap_err()
                .status(),
            503
        );
    }
}
