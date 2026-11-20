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
FROM stage s
JOIN builder.collections c ON c.id = s.collection_id
WHERE s.status IN ('pending', 'approved', 'rejected')
  AND NOT EXISTS (SELECT 1 FROM builder.collection_curations o
                  WHERE o.collection_id = s.collection_id AND o.created_at = s.created_at AT TIME ZONE 'UTC'
                    AND o.id <> s.id)
ORDER BY s.collection_id, s.created_at, s.updated_at DESC, s.id DESC
ON CONFLICT (id) DO UPDATE SET
    collection_id = EXCLUDED.collection_id,
    status = EXCLUDED.status,
    assignee = EXCLUDED.assignee,
    reviewed_by = EXCLUDED.reviewed_by,
    rejection_reasons = EXCLUDED.rejection_reasons,
    rejection_message = EXCLUDED.rejection_message,
    created_at = EXCLUDED.created_at,
    updated_at = EXCLUDED.updated_at
WHERE (k.collection_id, k.status, k.assignee, k.reviewed_by, k.rejection_reasons, k.rejection_message,
       k.created_at, k.updated_at)
      IS DISTINCT FROM
      (EXCLUDED.collection_id, EXCLUDED.status, EXCLUDED.assignee, EXCLUDED.reviewed_by, EXCLUDED.rejection_reasons,
       EXCLUDED.rejection_message, EXCLUDED.created_at, EXCLUDED.updated_at)
  AND k.updated_at <= EXCLUDED.updated_at;
