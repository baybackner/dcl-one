CREATE OR REPLACE FUNCTION pg_temp.builder_item_curation_fingerprint(k builder.item_curations) RETURNS text
LANGUAGE sql STABLE AS $$
  SELECT md5(json_build_array(k.item_id, k.status, k.content_hash, k.is_mapping_complete, k.created_at,
                              k.updated_at)::text)
$$;

CREATE TEMP TABLE incoming_item_curation ON COMMIT DROP AS
SELECT s.id, s.item_id, s.status, lower(s.content_hash) AS content_hash, s.is_mapping_complete,
       s.created_at AT TIME ZONE 'UTC' AS created_at,
       GREATEST(s.updated_at, s.created_at) AT TIME ZONE 'UTC' AS updated_at,
       s.updated_at AS upstream_updated_at,
       m.id IS NOT NULL AS imported
FROM stage s
LEFT JOIN dcl_migration.builder_imported m ON m.step = 'item_curation' AND m.id = s.id::text
WHERE s.status IN ('pending', 'approved', 'rejected')
  AND lower(s.content_hash) ~ '^[0-9a-f]{64}$';

WITH refreshed AS (
  UPDATE builder.item_curations AS k
     SET (status, content_hash, is_mapping_complete, updated_at)
       = (s.status, s.content_hash, s.is_mapping_complete, s.updated_at)
    FROM incoming_item_curation s
    JOIN dcl_migration.builder_imported m ON m.step = 'item_curation' AND m.id = s.id::text
   WHERE k.id = s.id
     AND m.fingerprint = pg_temp.builder_item_curation_fingerprint(k)
     AND (k.status, k.content_hash, k.is_mapping_complete, k.updated_at)
         IS DISTINCT FROM (s.status, s.content_hash, s.is_mapping_complete, s.updated_at)
  RETURNING k.id, pg_temp.builder_item_curation_fingerprint(k) AS fingerprint)
INSERT INTO dcl_migration.builder_imported (step, id, fingerprint)
SELECT 'item_curation', id::text, fingerprint FROM refreshed
ON CONFLICT (step, id) DO UPDATE SET fingerprint = EXCLUDED.fingerprint;

INSERT INTO builder.item_curations (id, item_id, status, content_hash, is_mapping_complete, created_at, updated_at)
SELECT DISTINCT ON (s.item_id, s.created_at)
       s.id, s.item_id, s.status, s.content_hash, s.is_mapping_complete, s.created_at, s.updated_at
FROM incoming_item_curation s
JOIN builder.items i ON i.id = s.item_id
WHERE NOT s.imported
  AND NOT EXISTS (SELECT 1 FROM builder.item_curations o
                  WHERE o.item_id = s.item_id AND o.created_at = s.created_at AND o.id <> s.id)
ORDER BY s.item_id, s.created_at, s.upstream_updated_at DESC, s.id DESC
ON CONFLICT (id) DO NOTHING;

INSERT INTO dcl_migration.builder_imported (step, id, fingerprint)
SELECT 'item_curation', k.id::text, pg_temp.builder_item_curation_fingerprint(k)
FROM incoming_item_curation s JOIN builder.item_curations k ON k.id = s.id
WHERE NOT s.imported
ON CONFLICT DO NOTHING;
