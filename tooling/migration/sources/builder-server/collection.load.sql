INSERT INTO builder.collections AS c
    (id, name, eth_address, salt, contract_address, urn_suffix, third_party_id,
     publication_pending, created_at, updated_at, linked_contract_address, linked_contract_network)
SELECT id, btrim(name), lower(eth_address), salt, contract_address, urn_suffix, third_party_id,
       false,
       created_at AT TIME ZONE 'UTC', updated_at AT TIME ZONE 'UTC',
       lower(linked_contract_address), linked_contract_network
FROM stage
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
    linked_contract_network = EXCLUDED.linked_contract_network
WHERE (c.name, c.eth_address, c.salt, c.contract_address, c.urn_suffix, c.third_party_id,
       c.created_at, c.updated_at, c.linked_contract_address, c.linked_contract_network)
      IS DISTINCT FROM
      (EXCLUDED.name, EXCLUDED.eth_address, EXCLUDED.salt, EXCLUDED.contract_address, EXCLUDED.urn_suffix,
       EXCLUDED.third_party_id, EXCLUDED.created_at, EXCLUDED.updated_at,
       EXCLUDED.linked_contract_address, EXCLUDED.linked_contract_network)
  AND c.updated_at <= EXCLUDED.updated_at;
