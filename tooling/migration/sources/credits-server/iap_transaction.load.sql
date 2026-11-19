-- An upstream transaction is kept as a `refunded` row without a wallet, so
-- the node never credits it again (a device's or a notification's later
-- delivery answers `refunded_transaction`). A later Apple refund of it is
-- not charged back: USD credits arrive as plain ledger grants and MANA ones
-- were never granted on the node (README, known limitations). A row the
-- node already holds is left alone.
INSERT INTO credits.iap_transactions
    (transaction_id, product_id, status, refund_ref, refund_reason, refunded_at)
SELECT transaction_id, product_id, 'refunded', 'credits-server', 'credited upstream',
       timezone('UTC', now())
FROM stage
ON CONFLICT (transaction_id) DO NOTHING;
