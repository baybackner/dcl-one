CREATE OR REPLACE FUNCTION pg_temp.builder_collection_fingerprint(c builder.collections) RETURNS text
LANGUAGE sql STABLE AS $$
  SELECT md5(json_build_array(c.name, c.eth_address, c.salt, c.contract_address, c.urn_suffix, c.third_party_id,
                              c.publication_pending, c.created_at, c.updated_at, c.linked_contract_address,
                              c.linked_contract_network, c.lock, c.forum_link, c.forum_id, c.forum_claimed_at)::text)
$$;

CREATE OR REPLACE FUNCTION pg_temp.builder_item_fingerprint(i builder.items) RETURNS text
LANGUAGE sql STABLE AS $$
  SELECT md5(json_build_array(i.collection_id, i.eth_address, i.name, i.description, i.thumbnail, i.video, i.type,
                              i.rarity, i.price, i.beneficiary, i.data, i.metrics, i.utility, i.mappings, i.urn_suffix,
                              i.blockchain_item_id, i.is_published, i.in_catalyst, i.total_supply,
                              i.local_content_hash, i.content_hash, i.catalyst_content_hash, i.created_at,
                              i.updated_at, i.video_named_by)::text)
$$;

CREATE OR REPLACE FUNCTION pg_temp.builder_collection_curation_fingerprint(k builder.collection_curations) RETURNS text
LANGUAGE sql STABLE AS $$
  SELECT md5(json_build_array(k.collection_id, k.status, k.assignee, k.reviewed_by, k.rejection_reasons,
                              k.rejection_message, k.created_at, k.updated_at)::text)
$$;

CREATE OR REPLACE FUNCTION pg_temp.builder_item_curation_fingerprint(k builder.item_curations) RETURNS text
LANGUAGE sql STABLE AS $$
  SELECT md5(json_build_array(k.item_id, k.status, k.content_hash, k.is_mapping_complete, k.created_at,
                              k.updated_at)::text)
$$;

CREATE OR REPLACE FUNCTION pg_temp.builder_lock_units(unit_collections uuid[], unit_items uuid[]) RETURNS void
LANGUAGE plpgsql AS $$ BEGIN
  PERFORM 1 FROM builder.collections c WHERE c.id = ANY(unit_collections) ORDER BY c.id FOR UPDATE;
  PERFORM 1 FROM builder.items i WHERE i.collection_id = ANY(unit_collections) OR i.id = ANY(unit_items)
    ORDER BY i.id FOR UPDATE;
END $$;

CREATE OR REPLACE FUNCTION pg_temp.builder_unit_reviewed(unit_collection uuid, unit_item uuid) RETURNS boolean
LANGUAGE sql STABLE AS $$
  SELECT EXISTS (
            SELECT 1 FROM builder.collection_curations k
              LEFT JOIN dcl_migration.builder_imported m ON m.step = 'collection_curation' AND m.id = k.id::text
             WHERE k.collection_id = unit_collection
               AND m.fingerprint IS DISTINCT FROM pg_temp.builder_collection_curation_fingerprint(k))
      OR EXISTS (
            SELECT 1 FROM builder.item_curations k
              LEFT JOIN dcl_migration.builder_imported m ON m.step = 'item_curation' AND m.id = k.id::text
             WHERE k.item_id IN (SELECT x.id FROM builder.items x WHERE x.collection_id = unit_collection
                                 UNION ALL SELECT unit_item)
               AND m.fingerprint IS DISTINCT FROM pg_temp.builder_item_curation_fingerprint(k))
$$;

CREATE OR REPLACE FUNCTION pg_temp.builder_unit_untouched(unit_collection uuid, unit_item uuid) RETURNS boolean
LANGUAGE sql STABLE AS $$
  SELECT (unit_collection IS NULL OR EXISTS (
            SELECT 1 FROM builder.collections c
              JOIN dcl_migration.builder_imported m ON m.step = 'collection' AND m.id = c.id::text
             WHERE c.id = unit_collection AND m.fingerprint = pg_temp.builder_collection_fingerprint(c)))
     AND NOT EXISTS (
            SELECT 1 FROM builder.items i
              LEFT JOIN dcl_migration.builder_imported m ON m.step = 'item' AND m.id = i.id::text
             WHERE i.id IN (SELECT x.id FROM builder.items x WHERE x.collection_id = unit_collection
                            UNION ALL SELECT unit_item)
               AND m.fingerprint IS DISTINCT FROM pg_temp.builder_item_fingerprint(i))
     AND NOT pg_temp.builder_unit_reviewed(unit_collection, unit_item)
$$;

CREATE TEMP TABLE incoming_slot_usage_cheque ON COMMIT DROP AS
SELECT DISTINCT ON (s.third_party_id, lower(s.salt))
       s.id, s.collection_id, s.third_party_id, lower(s.signature) AS signature, lower(s.salt) AS salt, s.qty,
       s.created_at AT TIME ZONE 'UTC' AS created_at, GREATEST(s.updated_at, s.created_at) AT TIME ZONE 'UTC' AS updated_at
FROM stage s
JOIN builder.collections c ON c.id = s.collection_id AND c.third_party_id = s.third_party_id
JOIN dcl_migration.builder_imported m ON m.step = 'collection' AND m.id = c.id::text
WHERE lower(s.salt) ~ '^0x[0-9a-f]{64}$'
  AND lower(s.signature) ~ '^0x[0-9a-f]{130}$'
  AND s.qty > 0
  AND NOT EXISTS (SELECT 1 FROM builder.slot_usage_cheques o
                  WHERE o.third_party_id = s.third_party_id AND o.salt = lower(s.salt))
ORDER BY s.third_party_id, lower(s.salt), s.created_at, s.id;

DO $lock$ BEGIN
  PERFORM pg_temp.builder_lock_units(ARRAY(SELECT DISTINCT collection_id FROM incoming_slot_usage_cheque), '{}');
END $lock$;

CREATE TEMP TABLE slot_usage_cheque_reviewed ON COMMIT DROP AS
WITH u AS MATERIALIZED (SELECT DISTINCT collection_id FROM incoming_slot_usage_cheque)
SELECT u.collection_id FROM u WHERE pg_temp.builder_unit_reviewed(u.collection_id, NULL);

DO $kept$ DECLARE n bigint; ids text; BEGIN
  SELECT count(*), string_agg(id::text, ', ' ORDER BY id) FILTER (WHERE rn <= 10) INTO n, ids
    FROM (SELECT id, row_number() OVER (ORDER BY id) AS rn FROM incoming_slot_usage_cheque
           WHERE collection_id IN (SELECT collection_id FROM slot_usage_cheque_reviewed)) k;
  IF n > 0 THEN RAISE NOTICE
    'builder-server/slot_usage_cheque: % new cheques of collections the node reviewed are left out, first: %', n, ids;
  END IF; END $kept$;

INSERT INTO builder.slot_usage_cheques (id, collection_id, third_party_id, signature, salt, qty, created_at, updated_at)
SELECT id, collection_id, third_party_id, signature, salt, qty, created_at, updated_at
FROM incoming_slot_usage_cheque
WHERE collection_id NOT IN (SELECT collection_id FROM slot_usage_cheque_reviewed)
ON CONFLICT (id) DO NOTHING;
