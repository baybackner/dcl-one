-- Every App Store transaction upstream credited (each credit row survives a
-- refund, `iap_credits_survive_refund`). Its Credits are already in the
-- ledger step's grants, so the node only needs the id.
SELECT apple_transaction_id AS transaction_id,
       COALESCE(NULLIF(apple_product_id, ''), 'unknown') AS product_id
FROM iap_credits
