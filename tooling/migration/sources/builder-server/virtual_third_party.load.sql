CREATE TEMP TABLE fresh_virtual_third_party ON COMMIT DROP AS
SELECT s.* FROM stage s
WHERE NOT EXISTS (SELECT 1 FROM dcl_migration.builder_imported m WHERE m.step = 'virtual_third_party' AND m.id = s.id::text);

INSERT INTO builder.virtual_third_parties AS v
    (id, managers, raw_metadata, is_programmatic, created_at, updated_at)
SELECT id, lower(managers::text)::text[], raw_metadata, is_programmatic,
       created_at AT TIME ZONE 'UTC', updated_at AT TIME ZONE 'UTC'
FROM fresh_virtual_third_party
WHERE char_length(id) <= 512
ON CONFLICT (id) DO NOTHING;

INSERT INTO dcl_migration.builder_imported (step, id)
SELECT 'virtual_third_party', f.id::text FROM fresh_virtual_third_party f JOIN builder.virtual_third_parties t ON t.id = f.id
ON CONFLICT DO NOTHING;
