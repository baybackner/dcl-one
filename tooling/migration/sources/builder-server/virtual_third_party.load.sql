INSERT INTO builder.virtual_third_parties AS v
    (id, managers, raw_metadata, is_programmatic, created_at, updated_at)
SELECT id, lower(managers::text)::text[], raw_metadata, is_programmatic,
       created_at AT TIME ZONE 'UTC', updated_at AT TIME ZONE 'UTC'
FROM stage
WHERE char_length(id) <= 512
ON CONFLICT (id) DO UPDATE SET
    managers = EXCLUDED.managers,
    raw_metadata = EXCLUDED.raw_metadata,
    is_programmatic = EXCLUDED.is_programmatic,
    created_at = EXCLUDED.created_at,
    updated_at = EXCLUDED.updated_at
WHERE (v.managers, v.raw_metadata, v.is_programmatic, v.created_at, v.updated_at)
      IS DISTINCT FROM
      (EXCLUDED.managers, EXCLUDED.raw_metadata, EXCLUDED.is_programmatic, EXCLUDED.created_at, EXCLUDED.updated_at)
  AND v.updated_at <= EXCLUDED.updated_at;
