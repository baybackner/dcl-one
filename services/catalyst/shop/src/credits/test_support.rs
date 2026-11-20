//! Credits test helpers shared with other crates' tests.

use std::collections::HashMap;

use async_trait::async_trait;

use super::errors::ApiError;
use super::escrow::{
    Broker, BrokerError, BuyRequest, ManaPaymentVerification, NoBroker, TransferBlock,
};

/// A fresh, well-formed transaction hash.
pub fn fresh_tx_hash() -> String {
    format!("0x{}", uuid::Uuid::new_v4().simple().to_string().repeat(2))
}

/// A confirmed 100 MANA transfer from `from` to the top-up address, `confirmations` deep.
pub fn mana_payment_from(from: &str, confirmations: u64) -> ManaPaymentVerification {
    ManaPaymentVerification::Confirmed {
        from: from.to_string(),
        to: "0x1111111111111111111111111111111111111111".into(),
        value_wei: "100000000000000000000".into(),
        block: TransferBlock {
            number: 1_000,
            timestamp: 978_307_210,
            confirmations,
        },
    }
}

/// A configured broker that answers top-up verifications from `verifications` (an unknown hash is
/// rejected); every other call is the unconfigured broker's answer.
#[derive(Default)]
pub struct TopupBroker {
    pub verifications: HashMap<String, ManaPaymentVerification>,
}

#[async_trait]
impl Broker for TopupBroker {
    fn configured(&self) -> bool {
        true
    }

    fn escrow_address(&self) -> Option<String> {
        None
    }

    async fn buy(&self, req: &BuyRequest) -> Result<String, BrokerError> {
        NoBroker.buy(req).await
    }

    async fn resume(&self, idempotency_key: &str) -> Result<String, BrokerError> {
        NoBroker.resume(idempotency_key).await
    }

    async fn released_buy_mined(&self, idempotency_key: &str) -> Result<bool, ApiError> {
        NoBroker.released_buy_mined(idempotency_key).await
    }

    async fn reclaim(
        &self,
        collection: &str,
        token_id: &str,
        idempotency_key: &str,
    ) -> Result<String, ApiError> {
        NoBroker
            .reclaim(collection, token_id, idempotency_key)
            .await
    }

    async fn release(
        &self,
        collection: &str,
        token_id: &str,
        buyer: &str,
        idempotency_key: &str,
    ) -> Result<String, ApiError> {
        NoBroker
            .release(collection, token_id, buyer, idempotency_key)
            .await
    }

    async fn verify_mana_payment(
        &self,
        tx_hash: &str,
    ) -> Result<ManaPaymentVerification, ApiError> {
        self.verifications
            .get(tx_hash)
            .cloned()
            .ok_or_else(|| ApiError::bad_request("economy rejected the txHash"))
    }
}
