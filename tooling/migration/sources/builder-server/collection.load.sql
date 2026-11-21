CREATE OR REPLACE FUNCTION pg_temp.builder_collection_fingerprint(c builder.collections) RETURNS text
LANGUAGE sql STABLE AS $$
  SELECT md5(json_build_array(c.name, c.eth_address, c.salt, c.contract_address, c.urn_suffix, c.third_party_id,
                              c.publication_pending, c.created_at, c.updated_at, c.linked_contract_address,
                              c.linked_contract_network, c.lock, c.forum_link, c.forum_id, c.forum_claimed_at)::text)
$$;

CREATE TEMP TABLE incoming_collection ON COMMIT DROP AS
SELECT s.id, btrim(s.name) AS name, lower(s.eth_address) AS eth_address, s.salt, s.contract_address, s.urn_suffix,
       s.third_party_id, s.created_at AT TIME ZONE 'UTC' AS created_at, s.updated_at AT TIME ZONE 'UTC' AS updated_at,
       lower(s.linked_contract_address) AS linked_contract_address, s.linked_contract_network, s.lock,
       CASE WHEN s.web_link THEN s.forum_link END AS forum_link,
       CASE WHEN s.web_link AND s.forum_id > 0 THEN s.forum_id END AS forum_id,
       m.id IS NOT NULL AS imported
FROM (SELECT *, COALESCE(forum_link ~ '^https?://[^[:space:]]+$' AND char_length(forum_link) <= 2048
                         AND left(forum_link, char_length(:'forum_url') + 1) = :'forum_url' || '/', false) AS web_link
      FROM stage) s
LEFT JOIN dcl_migration.builder_imported m ON m.step = 'collection' AND m.id = s.id::text;

DO $taken$ DECLARE n bigint; ids text; BEGIN
  SELECT count(*), string_agg(t.id::text, ', ' ORDER BY t.id) FILTER (WHERE t.rn <= 10) INTO n, ids
    FROM (SELECT x.id, row_number() OVER (ORDER BY x.id) AS rn
            FROM incoming_collection s JOIN builder.collections x ON x.id = s.id
           WHERE NOT s.imported AND x.created_at IS DISTINCT FROM s.created_at) t;
  IF n > 0 THEN RAISE EXCEPTION
    'builder-server/collection: the node holds % ids the import did not write (another creation time), first: %; resolve them before importing', n, ids;
  ELSE NULL; END IF; END $taken$;

WITH refreshed AS (
  UPDATE builder.collections AS c
     SET (name, eth_address, salt, contract_address, urn_suffix, third_party_id, updated_at,
          linked_contract_address, linked_contract_network, lock, forum_link, forum_id)
       = (s.name, s.eth_address, s.salt, s.contract_address, s.urn_suffix, s.third_party_id, s.updated_at,
          s.linked_contract_address, s.linked_contract_network, s.lock, s.forum_link, s.forum_id)
    FROM incoming_collection s
    JOIN dcl_migration.builder_imported m ON m.step = 'collection' AND m.id = s.id::text
   WHERE c.id = s.id
     AND m.fingerprint = pg_temp.builder_collection_fingerprint(c)
     AND (c.name, c.eth_address, c.salt, c.contract_address, c.urn_suffix, c.third_party_id, c.updated_at,
          c.linked_contract_address, c.linked_contract_network, c.lock, c.forum_link, c.forum_id)
         IS DISTINCT FROM
         (s.name, s.eth_address, s.salt, s.contract_address, s.urn_suffix, s.third_party_id, s.updated_at,
          s.linked_contract_address, s.linked_contract_network, s.lock, s.forum_link, s.forum_id)
  RETURNING c.id, pg_temp.builder_collection_fingerprint(c) AS fingerprint)
INSERT INTO dcl_migration.builder_imported (step, id, fingerprint)
SELECT 'collection', id::text, fingerprint FROM refreshed
ON CONFLICT (step, id) DO UPDATE SET fingerprint = EXCLUDED.fingerprint;

INSERT INTO builder.collections
    (id, name, eth_address, salt, contract_address, urn_suffix, third_party_id,
     publication_pending, created_at, updated_at, linked_contract_address, linked_contract_network, lock,
     forum_link, forum_id)
SELECT id, name, eth_address, salt, contract_address, urn_suffix, third_party_id,
       false, created_at, updated_at, linked_contract_address, linked_contract_network, lock,
       forum_link, forum_id
FROM incoming_collection
WHERE NOT imported
ON CONFLICT (id) DO NOTHING;

INSERT INTO dcl_migration.builder_imported (step, id, fingerprint)
SELECT 'collection', c.id::text, pg_temp.builder_collection_fingerprint(c)
FROM incoming_collection s JOIN builder.collections c ON c.id = s.id
WHERE NOT s.imported
ON CONFLICT DO NOTHING;
