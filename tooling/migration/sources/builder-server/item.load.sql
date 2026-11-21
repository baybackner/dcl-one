DO $release$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns
              WHERE table_schema = 'builder' AND table_name = 'items'
                AND column_name = 'data' AND data_type = 'jsonb') THEN
    RAISE EXCEPTION 'builder-server/item: builder.items.data is jsonb, which loses the saved key order; run schemas/releases/builder-item-documents-and-uploaders.sql first';
  END IF; END $release$;

DO $taken$ DECLARE n bigint; ids text; BEGIN
  SELECT count(*), string_agg(t.id::text, ', ' ORDER BY t.id) FILTER (WHERE t.rn <= 10) INTO n, ids
    FROM (SELECT x.id, row_number() OVER (ORDER BY x.id) AS rn
            FROM stage s JOIN builder.items x ON x.id = s.id
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
FROM stage s
LEFT JOIN builder.collections c ON c.id = s.collection_id
ON CONFLICT (id) DO UPDATE SET
    collection_id = EXCLUDED.collection_id,
    eth_address = EXCLUDED.eth_address,
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    thumbnail = EXCLUDED.thumbnail,
    video = EXCLUDED.video,
    video_named_by = CASE WHEN i.video IS NOT DISTINCT FROM EXCLUDED.video THEN i.video_named_by
                          ELSE EXCLUDED.video_named_by END,
    type = EXCLUDED.type,
    rarity = EXCLUDED.rarity,
    price = EXCLUDED.price,
    beneficiary = EXCLUDED.beneficiary,
    data = EXCLUDED.data,
    metrics = EXCLUDED.metrics,
    utility = EXCLUDED.utility,
    mappings = EXCLUDED.mappings,
    urn_suffix = EXCLUDED.urn_suffix,
    blockchain_item_id = EXCLUDED.blockchain_item_id,
    is_published = EXCLUDED.is_published,
    total_supply = EXCLUDED.total_supply,
    local_content_hash = EXCLUDED.local_content_hash,
    content_hash = EXCLUDED.content_hash,
    created_at = EXCLUDED.created_at,
    updated_at = EXCLUDED.updated_at
WHERE (i.collection_id, i.eth_address, i.name, i.description, i.thumbnail, i.video, i.type, i.rarity,
       i.price, i.beneficiary, i.data::text, i.metrics::text, i.utility, i.mappings, i.urn_suffix,
       i.blockchain_item_id, i.is_published, i.total_supply,
       i.local_content_hash, i.content_hash, i.created_at, i.updated_at)
      IS DISTINCT FROM
      (EXCLUDED.collection_id, EXCLUDED.eth_address, EXCLUDED.name, EXCLUDED.description,
       EXCLUDED.thumbnail, EXCLUDED.video, EXCLUDED.type, EXCLUDED.rarity, EXCLUDED.price,
       EXCLUDED.beneficiary, EXCLUDED.data::text, EXCLUDED.metrics::text, EXCLUDED.utility, EXCLUDED.mappings,
       EXCLUDED.urn_suffix, EXCLUDED.blockchain_item_id, EXCLUDED.is_published,
       EXCLUDED.total_supply, EXCLUDED.local_content_hash,
       EXCLUDED.content_hash, EXCLUDED.created_at, EXCLUDED.updated_at)
  AND i.updated_at <= EXCLUDED.updated_at;
