DO $missing$ DECLARE n bigint; BEGIN
  SELECT count(DISTINCT h.hash) INTO n
    FROM (SELECT s.hash FROM stage s
          UNION SELECT i.video FROM builder.items i
           WHERE i.video IS NOT NULL AND i.id IN (SELECT item_id FROM stage)) h
   WHERE NOT EXISTS (SELECT 1 FROM dcl_migration.blob_inventory b
                      WHERE b.source = 'builder-server' AND b.object_key = h.hash);
  IF n > 0 THEN RAISE EXCEPTION
    'builder-server/item_content: % content hashes are not in dcl_migration.blob_inventory; copy the blobs first', n;
  ELSE NULL; END IF; END $missing$;

DO $oversized$ DECLARE n bigint; BEGIN
  SELECT count(DISTINCT b.object_key) INTO n
    FROM dcl_migration.blob_inventory b
   WHERE b.source = 'builder-server' AND b.size > 250000000
     AND b.object_key IN (SELECT s.hash FROM stage s
                          UNION SELECT i.video FROM builder.items i
                           WHERE i.video IS NOT NULL AND i.id IN (SELECT item_id FROM stage));
  IF n > 0 THEN RAISE EXCEPTION
    'builder-server/item_content: % content hashes are over 250,000,000 bytes, the largest file the node keeps', n;
  ELSE NULL; END IF; END $oversized$;

CREATE TEMP TABLE current_stage ON COMMIT DROP AS
SELECT s.* FROM stage s
JOIN builder.items i ON i.id = s.item_id AND i.updated_at = s.item_updated_at AT TIME ZONE 'UTC';

INSERT INTO builder.uploaded_contents (hash, size, uploaded_by)
SELECT DISTINCT s.hash, b.size, lower(s.owner)
FROM current_stage s
JOIN dcl_migration.blob_inventory b ON b.source = 'builder-server' AND b.object_key = s.hash
ON CONFLICT (hash, uploaded_by) DO NOTHING;

INSERT INTO builder.uploaded_contents (hash, size, uploaded_by)
SELECT DISTINCT i.video, b.size, i.video_named_by
FROM builder.items i
JOIN dcl_migration.blob_inventory b ON b.source = 'builder-server' AND b.object_key = i.video
WHERE i.video_named_by IS NOT NULL AND i.id IN (SELECT item_id FROM current_stage)
ON CONFLICT (hash, uploaded_by) DO NOTHING;

INSERT INTO builder.item_contents AS c (item_id, file, hash, position, named_by)
SELECT s.item_id, s.file, s.hash, s.position, lower(s.owner)
FROM current_stage s
ON CONFLICT (item_id, file) DO UPDATE SET hash = EXCLUDED.hash, position = EXCLUDED.position,
    named_by = CASE WHEN c.hash = EXCLUDED.hash THEN c.named_by ELSE EXCLUDED.named_by END
WHERE (c.hash, c.position) IS DISTINCT FROM (EXCLUDED.hash, EXCLUDED.position);
