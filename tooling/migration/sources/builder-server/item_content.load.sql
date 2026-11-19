DO $missing$ DECLARE n bigint; BEGIN
  SELECT count(DISTINCT s.hash) INTO n FROM stage s
   WHERE NOT EXISTS (SELECT 1 FROM dcl_migration.blob_inventory b
                      WHERE b.source = 'builder-server' AND b.object_key = s.hash);
  IF n > 0 THEN RAISE EXCEPTION
    'builder-server/item_content: % content hashes are not in dcl_migration.blob_inventory; copy the blobs first', n;
  ELSE NULL; END IF; END $missing$;

INSERT INTO builder.uploaded_contents (hash, size, uploaded_by)
SELECT DISTINCT s.hash, b.size, lower(s.owner)
FROM stage s
JOIN dcl_migration.blob_inventory b ON b.source = 'builder-server' AND b.object_key = s.hash
ON CONFLICT (hash, uploaded_by) DO NOTHING;

INSERT INTO builder.item_contents AS c (item_id, file, hash, position, named_by)
SELECT s.item_id, s.file, s.hash, s.position, lower(s.owner)
FROM stage s
ON CONFLICT (item_id, file) DO UPDATE SET hash = EXCLUDED.hash, position = EXCLUDED.position,
    named_by = CASE WHEN c.hash = EXCLUDED.hash THEN c.named_by ELSE EXCLUDED.named_by END
WHERE (c.hash, c.position) IS DISTINCT FROM (EXCLUDED.hash, EXCLUDED.position);
