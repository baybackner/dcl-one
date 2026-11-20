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
      FROM stage) s
ON CONFLICT (id) DO UPDATE SET
    name = EXCLUDED.name,
    eth_address = EXCLUDED.eth_address,
    salt = EXCLUDED.salt,
    contract_address = EXCLUDED.contract_address,
    urn_suffix = EXCLUDED.urn_suffix,
    third_party_id = EXCLUDED.third_party_id,
    created_at = EXCLUDED.created_at,
    updated_at = EXCLUDED.updated_at,
    linked_contract_address = EXCLUDED.linked_contract_address,
    linked_contract_network = EXCLUDED.linked_contract_network,
    lock = EXCLUDED.lock,
    forum_link = EXCLUDED.forum_link,
    forum_id = EXCLUDED.forum_id,
    forum_claimed_at = CASE WHEN EXCLUDED.forum_link IS NOT NULL THEN NULL ELSE c.forum_claimed_at END
WHERE (c.name, c.eth_address, c.salt, c.contract_address, c.urn_suffix, c.third_party_id,
       c.created_at, c.updated_at, c.linked_contract_address, c.linked_contract_network, c.lock,
       c.forum_link, c.forum_id)
      IS DISTINCT FROM
      (EXCLUDED.name, EXCLUDED.eth_address, EXCLUDED.salt, EXCLUDED.contract_address, EXCLUDED.urn_suffix,
       EXCLUDED.third_party_id, EXCLUDED.created_at, EXCLUDED.updated_at,
       EXCLUDED.linked_contract_address, EXCLUDED.linked_contract_network, EXCLUDED.lock,
       EXCLUDED.forum_link, EXCLUDED.forum_id)
  AND c.updated_at <= EXCLUDED.updated_at;
