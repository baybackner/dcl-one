INSERT INTO builder.item_curations AS k
    (id, item_id, status, content_hash, is_mapping_complete, created_at, updated_at)
SELECT DISTINCT ON (s.item_id, s.created_at)
       s.id, s.item_id, s.status, lower(s.content_hash), s.is_mapping_complete,
       s.created_at AT TIME ZONE 'UTC',
       GREATEST(s.updated_at, s.created_at) AT TIME ZONE 'UTC'
FROM stage s
JOIN builder.items i ON i.id = s.item_id
WHERE s.status IN ('pending', 'approved', 'rejected')
  AND lower(s.content_hash) ~ '^[0-9a-f]{64}$'
  AND NOT EXISTS (SELECT 1 FROM builder.item_curations o
                  WHERE o.item_id = s.item_id AND o.created_at = s.created_at AT TIME ZONE 'UTC' AND o.id <> s.id)
ORDER BY s.item_id, s.created_at, s.updated_at DESC, s.id DESC
ON CONFLICT (id) DO UPDATE SET
    item_id = EXCLUDED.item_id,
    status = EXCLUDED.status,
    content_hash = EXCLUDED.content_hash,
    is_mapping_complete = EXCLUDED.is_mapping_complete,
    created_at = EXCLUDED.created_at,
    updated_at = EXCLUDED.updated_at
WHERE (k.item_id, k.status, k.content_hash, k.is_mapping_complete, k.created_at, k.updated_at)
      IS DISTINCT FROM
      (EXCLUDED.item_id, EXCLUDED.status, EXCLUDED.content_hash, EXCLUDED.is_mapping_complete,
       EXCLUDED.created_at, EXCLUDED.updated_at)
  AND k.updated_at <= EXCLUDED.updated_at;
