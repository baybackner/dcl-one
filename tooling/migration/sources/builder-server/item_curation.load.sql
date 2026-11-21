CREATE TEMP TABLE fresh_item_curation ON COMMIT DROP AS
SELECT s.* FROM stage s
WHERE NOT EXISTS (SELECT 1 FROM dcl_migration.builder_imported m WHERE m.step = 'item_curation' AND m.id = s.id::text);

INSERT INTO builder.item_curations AS k
    (id, item_id, status, content_hash, is_mapping_complete, created_at, updated_at)
SELECT DISTINCT ON (s.item_id, s.created_at)
       s.id, s.item_id, s.status, lower(s.content_hash), s.is_mapping_complete,
       s.created_at AT TIME ZONE 'UTC',
       GREATEST(s.updated_at, s.created_at) AT TIME ZONE 'UTC'
FROM fresh_item_curation s
JOIN builder.items i ON i.id = s.item_id
WHERE s.status IN ('pending', 'approved', 'rejected')
  AND lower(s.content_hash) ~ '^[0-9a-f]{64}$'
  AND NOT EXISTS (SELECT 1 FROM builder.item_curations o
                  WHERE o.item_id = s.item_id AND o.created_at = s.created_at AT TIME ZONE 'UTC' AND o.id <> s.id)
ORDER BY s.item_id, s.created_at, s.updated_at DESC, s.id DESC
ON CONFLICT (id) DO NOTHING;

INSERT INTO dcl_migration.builder_imported (step, id)
SELECT 'item_curation', f.id::text FROM fresh_item_curation f JOIN builder.item_curations t ON t.id = f.id
ON CONFLICT DO NOTHING;
