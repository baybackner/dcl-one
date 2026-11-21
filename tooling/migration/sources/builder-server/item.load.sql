DO $release$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns
              WHERE table_schema = 'builder' AND table_name = 'items'
                AND column_name = 'data' AND data_type = 'jsonb') THEN
    RAISE EXCEPTION 'builder-server/item: builder.items.data is jsonb, which loses the saved key order; run schemas/releases/builder-item-documents-and-uploaders.sql first';
  END IF; END $release$;

CREATE TEMP TABLE fresh_item ON COMMIT DROP AS
SELECT s.* FROM stage s
WHERE NOT EXISTS (SELECT 1 FROM dcl_migration.builder_imported m WHERE m.step = 'item' AND m.id = s.id::text);

DO $taken$ DECLARE n bigint; ids text; BEGIN
  SELECT count(*), string_agg(t.id::text, ', ' ORDER BY t.id) FILTER (WHERE t.rn <= 10) INTO n, ids
    FROM (SELECT x.id, row_number() OVER (ORDER BY x.id) AS rn
            FROM fresh_item s JOIN builder.items x ON x.id = s.id
           WHERE x.created_at IS DISTINCT FROM s.created_at AT TIME ZONE 'UTC') t;
  IF n > 0 THEN RAISE EXCEPTION
    'builder-server/item: the node holds % ids the import did not write (another creation time), first: %; resolve them before importing', n, ids;
  ELSE NULL; END IF; END $taken$;

INSERT INTO builder.items AS i
    (id, collection_id, eth_address, name, description, thumbnail, video, type, rarity,
     price, beneficiary, data, metrics, utility, mappings, urn_suffix, blockchain_item_id,
     is_published, total_supply, local_content_hash, content_hash,
     created_at, updated_at, video_named_by)
SELECT s.id,
       c.id,
       lower(s.eth_address),
       btrim(s.name),
       s.description,
       s.thumbnail,
       s.video,
       s.type,
       lower(NULLIF(s.rarity, '')),
       NULLIF(s.price, ''),
       lower(NULLIF(s.beneficiary, '')),
       s.data,
       s.metrics,
       s.utility,
       s.mappings,
       s.urn_suffix,
       s.blockchain_item_id,
       s.is_published,
       s.total_supply,
       s.local_content_hash,
       s.content_hash,
       s.created_at AT TIME ZONE 'UTC',
       s.updated_at AT TIME ZONE 'UTC',
       CASE WHEN s.video IS NOT NULL THEN lower(s.eth_address) END
FROM fresh_item s
LEFT JOIN builder.collections c ON c.id = s.collection_id
ON CONFLICT (id) DO NOTHING;

INSERT INTO dcl_migration.builder_imported (step, id)
SELECT 'item', f.id::text FROM fresh_item f JOIN builder.items t ON t.id = f.id
ON CONFLICT DO NOTHING;
