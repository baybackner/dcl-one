CREATE TEMP TABLE fresh_collection_curation ON COMMIT DROP AS
SELECT s.* FROM stage s
WHERE NOT EXISTS (SELECT 1 FROM dcl_migration.builder_imported m WHERE m.step = 'collection_curation' AND m.id = s.id::text);

INSERT INTO builder.collection_curations AS k
    (id, collection_id, status, assignee, reviewed_by, rejection_reasons, rejection_message, created_at, updated_at)
SELECT DISTINCT ON (s.collection_id, s.created_at)
       s.id, s.collection_id, s.status,
       CASE WHEN lower(s.assignee) ~ '^0x[0-9a-f]{40}$' THEN lower(s.assignee) END,
       CASE WHEN lower(s.reviewed_by) ~ '^0x[0-9a-f]{40}$' OR s.reviewed_by = 'validator' THEN lower(s.reviewed_by) END,
       CASE WHEN s.rejection_reasons <@ ARRAY['clipping', 'thumbnail', 'category_hides', 'rigging',
                 'triangle_count', 'emote', 'file_size', 'textures_materials', 'reversed_faces',
                 'smart_wearable_files', 'content_policy_ip', 'other']
            THEN NULLIF(s.rejection_reasons, '{}') END,
       CASE WHEN char_length(btrim(s.rejection_message)) <= 3000 THEN NULLIF(btrim(s.rejection_message), '') END,
       s.created_at AT TIME ZONE 'UTC',
       GREATEST(s.updated_at, s.created_at) AT TIME ZONE 'UTC'
FROM fresh_collection_curation s
JOIN builder.collections c ON c.id = s.collection_id
WHERE s.status IN ('pending', 'approved', 'rejected')
  AND NOT EXISTS (SELECT 1 FROM builder.collection_curations o
                  WHERE o.collection_id = s.collection_id AND o.created_at = s.created_at AT TIME ZONE 'UTC'
                    AND o.id <> s.id)
ORDER BY s.collection_id, s.created_at, s.updated_at DESC, s.id DESC
ON CONFLICT (id) DO NOTHING;

INSERT INTO dcl_migration.builder_imported (step, id)
SELECT 'collection_curation', f.id::text FROM fresh_collection_curation f JOIN builder.collection_curations t ON t.id = f.id
ON CONFLICT DO NOTHING;
