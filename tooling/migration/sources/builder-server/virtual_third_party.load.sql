CREATE OR REPLACE FUNCTION pg_temp.builder_virtual_third_party_fingerprint(v builder.virtual_third_parties) RETURNS text
LANGUAGE sql STABLE AS $$
  SELECT md5(json_build_array(v.managers, v.raw_metadata, v.is_programmatic, v.created_at, v.updated_at)::text)
$$;

CREATE TEMP TABLE incoming_virtual_third_party ON COMMIT DROP AS
SELECT s.id, lower(s.managers::text)::text[] AS managers, s.raw_metadata, s.is_programmatic,
       s.created_at AT TIME ZONE 'UTC' AS created_at, s.updated_at AT TIME ZONE 'UTC' AS updated_at,
       m.id IS NOT NULL AS imported
FROM stage s
LEFT JOIN dcl_migration.builder_imported m ON m.step = 'virtual_third_party' AND m.id = s.id::text
WHERE char_length(s.id) <= 512;

DO $taken$ DECLARE n bigint; ids text; BEGIN
  SELECT count(*), string_agg(t.id, ', ' ORDER BY t.id) FILTER (WHERE t.rn <= 10) INTO n, ids
    FROM (SELECT v.id, row_number() OVER (ORDER BY v.id) AS rn
            FROM incoming_virtual_third_party s JOIN builder.virtual_third_parties v ON v.id = s.id
           WHERE NOT s.imported) t;
  IF n > 0 THEN RAISE EXCEPTION
    'builder-server/virtual_third_party: the node holds % ids the import did not write, first: %; resolve them before importing', n, ids;
  ELSE NULL; END IF; END $taken$;

WITH refreshed AS (
  UPDATE builder.virtual_third_parties AS v
     SET (managers, raw_metadata, is_programmatic, updated_at) = (s.managers, s.raw_metadata, s.is_programmatic, s.updated_at)
    FROM incoming_virtual_third_party s
    JOIN dcl_migration.builder_imported m ON m.step = 'virtual_third_party' AND m.id = s.id::text
   WHERE v.id = s.id
     AND m.fingerprint = pg_temp.builder_virtual_third_party_fingerprint(v)
     AND (v.managers, v.raw_metadata, v.is_programmatic, v.updated_at)
         IS DISTINCT FROM (s.managers, s.raw_metadata, s.is_programmatic, s.updated_at)
  RETURNING v.id, pg_temp.builder_virtual_third_party_fingerprint(v) AS fingerprint)
INSERT INTO dcl_migration.builder_imported (step, id, fingerprint)
SELECT 'virtual_third_party', id, fingerprint FROM refreshed
ON CONFLICT (step, id) DO UPDATE SET fingerprint = EXCLUDED.fingerprint;

WITH inserted AS (
  INSERT INTO builder.virtual_third_parties AS v (id, managers, raw_metadata, is_programmatic, created_at, updated_at)
  SELECT id, managers, raw_metadata, is_programmatic, created_at, updated_at
  FROM incoming_virtual_third_party
  WHERE NOT imported
  ON CONFLICT (id) DO NOTHING
  RETURNING v.id, pg_temp.builder_virtual_third_party_fingerprint(v) AS fingerprint)
INSERT INTO dcl_migration.builder_imported (step, id, fingerprint)
SELECT 'virtual_third_party', id, fingerprint FROM inserted
ON CONFLICT DO NOTHING;
