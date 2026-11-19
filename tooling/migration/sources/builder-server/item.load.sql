DO $release$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns
              WHERE table_schema = 'builder' AND table_name = 'items'
                AND column_name = 'data' AND data_type = 'jsonb') THEN
    RAISE EXCEPTION 'builder-server/item: builder.items.data is jsonb, which loses the saved key order; run schemas/releases/builder-item-documents-and-uploaders.sql first';
  END IF; END $release$;

INSERT INTO builder.items AS i
    (id, collection_id, eth_address, name, description, thumbnail, video, type, rarity,
     price, beneficiary, data, metrics, utility, mappings, urn_suffix, blockchain_item_id,
     is_published, curation_status, is_approved, total_supply, local_content_hash, content_hash,
     created_at, updated_at)
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
       s.curation_status,
       s.curation_status = 'approved',
       s.total_supply,
       s.local_content_hash,
       s.content_hash,
       s.created_at AT TIME ZONE 'UTC',
       s.updated_at AT TIME ZONE 'UTC'
FROM stage s
LEFT JOIN builder.collections c ON c.id = s.collection_id
ON CONFLICT (id) DO UPDATE SET
    collection_id = EXCLUDED.collection_id,
    eth_address = EXCLUDED.eth_address,
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    thumbnail = EXCLUDED.thumbnail,
    video = EXCLUDED.video,
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
    curation_status = EXCLUDED.curation_status,
    is_approved = EXCLUDED.is_approved,
    total_supply = EXCLUDED.total_supply,
    local_content_hash = EXCLUDED.local_content_hash,
    content_hash = EXCLUDED.content_hash,
    created_at = EXCLUDED.created_at,
    updated_at = EXCLUDED.updated_at
WHERE (i.collection_id, i.eth_address, i.name, i.description, i.thumbnail, i.video, i.type, i.rarity,
       i.price, i.beneficiary, i.data::text, i.metrics::text, i.utility, i.mappings, i.urn_suffix,
       i.blockchain_item_id, i.is_published, i.curation_status, i.total_supply,
       i.local_content_hash, i.content_hash, i.created_at, i.updated_at)
      IS DISTINCT FROM
      (EXCLUDED.collection_id, EXCLUDED.eth_address, EXCLUDED.name, EXCLUDED.description,
       EXCLUDED.thumbnail, EXCLUDED.video, EXCLUDED.type, EXCLUDED.rarity, EXCLUDED.price,
       EXCLUDED.beneficiary, EXCLUDED.data::text, EXCLUDED.metrics::text, EXCLUDED.utility, EXCLUDED.mappings,
       EXCLUDED.urn_suffix, EXCLUDED.blockchain_item_id, EXCLUDED.is_published,
       EXCLUDED.curation_status, EXCLUDED.total_supply, EXCLUDED.local_content_hash,
       EXCLUDED.content_hash, EXCLUDED.created_at, EXCLUDED.updated_at)
  AND i.updated_at <= EXCLUDED.updated_at;
