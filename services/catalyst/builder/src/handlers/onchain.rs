use axum::Json;
use serde::Deserialize;

use crate::SharedState;
use crate::access::Wallet;
use crate::http::{ApiData, ApiError};
use crate::onchain::{self, MARKETPLACE_DOWN, OrphanItemOut};

#[derive(Debug, Default, Deserialize)]
pub struct ItemsParams {
    #[serde(rename = "onlyOrphans")]
    pub only_orphans: Option<bool>,
}

fn reader(state: &SharedState) -> Result<&crate::chain_index::ChainIndex, ApiError> {
    state
        .market
        .as_deref()
        .filter(|m| onchain::configured(Some(m)))
        .ok_or_else(|| ApiError::service_unavailable(MARKETPLACE_DOWN))
}

/// The reader behind `collections::get_address_items`, for the wallet
/// `access::own_address` admitted.
pub async fn get_address_items(
    state: &SharedState,
    wallet: &Wallet,
    params: ItemsParams,
) -> Result<Json<ApiData<Vec<OrphanItemOut>>>, ApiError> {
    let only_orphans = params.only_orphans.unwrap_or(false);
    let market = reader(state)?;
    let data = onchain::items_for_address(market, wallet, only_orphans).await?;
    Ok(Json(ApiData::ok(data)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::wallet;
    use crate::shared::signed_fetch::test_support::signer;
    use crate::tests::support::state_without_db;

    const ADDR: &str = "0x797066a17f83425c1b4c7a8cca52d19095520a52";

    fn own() -> Wallet {
        wallet(&signer(ADDR))
    }

    #[tokio::test]
    async fn items_return_503_when_no_collections_reader_is_configured() {
        let state = state_without_db(None).await;
        let err = get_address_items(&state, &own(), ItemsParams::default())
            .await
            .expect_err("must be an error, not an empty 200");
        assert_eq!(err.status(), 503);
    }

    #[tokio::test]
    async fn a_failing_subgraph_is_a_5xx_not_an_empty_200() {
        let settings = crate::chain_index::IndexSettings {
            matic_collections_subgraph_url: Some("https://subgraph.invalid/graphql".to_string()),
            ..crate::chain_index::IndexSettings::for_tests()
        };
        let market = crate::tests::support::market_without_db(settings);
        let state = state_without_db(Some(market)).await;
        let err = get_address_items(&state, &own(), ItemsParams::default())
            .await
            .unwrap_err();
        assert!(err.status() >= 500, "{err}");
    }
}
