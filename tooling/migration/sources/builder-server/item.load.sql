DO $release$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns
              WHERE table_schema = 'builder' AND table_name = 'items'
                AND column_name = 'data' AND data_type = 'jsonb') THEN
    RAISE EXCEPTION 'builder-server/item: builder.items.data is jsonb, which loses the saved key order; run schemas/releases/builder-item-documents-and-uploaders.sql first';
  END IF; END $release$;

CREATE OR REPLACE FUNCTION pg_temp.builder_item_fingerprint(i builder.items) RETURNS text
LANGUAGE sql STABLE AS $$
  SELECT md5(json_build_array(i.collection_id, i.eth_address, i.name, i.description, i.thumbnail, i.video, i.type,
                              i.rarity, i.price, i.beneficiary, i.data, i.metrics, i.utility, i.mappings, i.urn_suffix,
                              i.blockchain_item_id, i.is_published, i.in_catalyst, i.total_supply,
                              i.local_content_hash, i.content_hash, i.catalyst_content_hash, i.created_at,
                              i.updated_at, i.video_named_by)::text)
$$;

CREATE TEMP TABLE incoming_item ON COMMIT DROP AS
SELECT s.id, c.id AS collection_id, lower(s.eth_address) AS eth_address, btrim(s.name) AS name, s.description,
       s.thumbnail, s.video, s.type, lower(NULLIF(s.rarity, '')) AS rarity, NULLIF(s.price, '') AS price,
       lower(NULLIF(s.beneficiary, '')) AS beneficiary, s.data, s.metrics, s.utility, s.mappings, s.urn_suffix,
       s.blockchain_item_id, s.is_published, s.total_supply, s.local_content_hash, s.content_hash,
       s.created_at AT TIME ZONE 'UTC' AS created_at, s.updated_at AT TIME ZONE 'UTC' AS updated_at,
       CASE WHEN s.video IS NOT NULL THEN lower(s.eth_address) END AS video_named_by,
       m.id IS NOT NULL AS imported
FROM stage s
LEFT JOIN builder.collections c ON c.id = s.collection_id
LEFT JOIN dcl_migration.builder_imported m ON m.step = 'item' AND m.id = s.id::text;

DO $taken$ DECLARE n bigint; ids text; BEGIN
  SELECT count(*), string_agg(t.id::text, ', ' ORDER BY t.id) FILTER (WHERE t.rn <= 10) INTO n, ids
    FROM (SELECT x.id, row_number() OVER (ORDER BY x.id) AS rn
            FROM incoming_item s JOIN builder.items x ON x.id = s.id
           WHERE NOT s.imported AND x.created_at IS DISTINCT FROM s.created_at) t;
  IF n > 0 THEN RAISE EXCEPTION
    'builder-server/item: the node holds % ids the import did not write (another creation time), first: %; resolve them before importing', n, ids;
  ELSE NULL; END IF; END $taken$;

WITH refreshed AS (
  UPDATE builder.items AS i
     SET (collection_id, eth_address, name, description, thumbnail, video, type, rarity, price, beneficiary,
          data, metrics, utility, mappings, urn_suffix, blockchain_item_id, is_published, total_supply,
          local_content_hash, content_hash, updated_at, video_named_by)
       = (s.collection_id, s.eth_address, s.name, s.description, s.thumbnail, s.video, s.type, s.rarity, s.price,
          s.beneficiary, s.data, s.metrics, s.utility, s.mappings, s.urn_suffix, s.blockchain_item_id,
          s.is_published, s.total_supply, s.local_content_hash, s.content_hash, s.updated_at, s.video_named_by)
    FROM incoming_item s
    JOIN dcl_migration.builder_imported m ON m.step = 'item' AND m.id = s.id::text
   WHERE i.id = s.id
     AND m.fingerprint = pg_temp.builder_item_fingerprint(i)
     AND (i.collection_id, i.eth_address, i.name, i.description, i.thumbnail, i.video, i.type, i.rarity, i.price,
          i.beneficiary, i.data::text, i.metrics::text, i.utility, i.mappings, i.urn_suffix, i.blockchain_item_id,
          i.is_published, i.total_supply, i.local_content_hash, i.content_hash, i.updated_at, i.video_named_by)
         IS DISTINCT FROM
         (s.collection_id, s.eth_address, s.name, s.description, s.thumbnail, s.video, s.type, s.rarity, s.price,
          s.beneficiary, s.data::text, s.metrics::text, s.utility, s.mappings, s.urn_suffix, s.blockchain_item_id,
          s.is_published, s.total_supply, s.local_content_hash, s.content_hash, s.updated_at, s.video_named_by)
  RETURNING i.id, pg_temp.builder_item_fingerprint(i) AS fingerprint)
INSERT INTO dcl_migration.builder_imported (step, id, fingerprint)
SELECT 'item', id::text, fingerprint FROM refreshed
ON CONFLICT (step, id) DO UPDATE SET fingerprint = EXCLUDED.fingerprint;

INSERT INTO builder.items
    (id, collection_id, eth_address, name, description, thumbnail, video, type, rarity,
     price, beneficiary, data, metrics, utility, mappings, urn_suffix, blockchain_item_id,
     is_published, total_supply, local_content_hash, content_hash,
     created_at, updated_at, video_named_by)
SELECT id, collection_id, eth_address, name, description, thumbnail, video, type, rarity,
       price, beneficiary, data, metrics, utility, mappings, urn_suffix, blockchain_item_id,
       is_published, total_supply, local_content_hash, content_hash,
       created_at, updated_at, video_named_by
FROM incoming_item
WHERE NOT imported
ON CONFLICT (id) DO NOTHING;

INSERT INTO dcl_migration.builder_imported (step, id, fingerprint)
SELECT 'item', i.id::text, pg_temp.builder_item_fingerprint(i)
FROM incoming_item s JOIN builder.items i ON i.id = s.id
WHERE NOT s.imported
ON CONFLICT DO NOTHING;
