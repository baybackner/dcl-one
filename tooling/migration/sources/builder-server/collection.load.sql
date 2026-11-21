CREATE TEMP TABLE fresh_collection ON COMMIT DROP AS
SELECT s.* FROM stage s
WHERE NOT EXISTS (SELECT 1 FROM dcl_migration.builder_imported m WHERE m.step = 'collection' AND m.id = s.id::text);

DO $taken$ DECLARE n bigint; ids text; BEGIN
  SELECT count(*), string_agg(t.id::text, ', ' ORDER BY t.id) FILTER (WHERE t.rn <= 10) INTO n, ids
    FROM (SELECT x.id, row_number() OVER (ORDER BY x.id) AS rn
            FROM fresh_collection s JOIN builder.collections x ON x.id = s.id
           WHERE x.created_at IS DISTINCT FROM s.created_at AT TIME ZONE 'UTC') t;
  IF n > 0 THEN RAISE EXCEPTION
    'builder-server/collection: the node holds % ids the import did not write (another creation time), first: %; resolve them before importing', n, ids;
  ELSE NULL; END IF; END $taken$;

INSERT INTO builder.collections AS c
    (id, name, eth_address, salt, contract_address, urn_suffix, third_party_id,
     publication_pending, created_at, updated_at, linked_contract_address, linked_contract_network, lock,
     forum_link, forum_id)
SELECT id, btrim(name), lower(eth_address), salt, contract_address, urn_suffix, third_party_id,
       false,
       created_at AT TIME ZONE 'UTC', updated_at AT TIME ZONE 'UTC',
       lower(linked_contract_address), linked_contract_network, lock,
       CASE WHEN web_link THEN forum_link END,
       CASE WHEN web_link AND forum_id > 0 THEN forum_id END
FROM (SELECT *, COALESCE(forum_link ~ '^https?://[^[:space:]]+$' AND char_length(forum_link) <= 2048
                         AND left(forum_link, char_length(:'forum_url') + 1) = :'forum_url' || '/', false) AS web_link
      FROM fresh_collection) s
ON CONFLICT (id) DO NOTHING;

INSERT INTO dcl_migration.builder_imported (step, id)
SELECT 'collection', f.id::text FROM fresh_collection f JOIN builder.collections t ON t.id = f.id
ON CONFLICT DO NOTHING;
