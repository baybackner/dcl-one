"""Builder, camera-reel, badges and events imports against the schema."""
import contextlib
import io

from tooling.tests import migration_harness as harness

COLLECTION = '11111111-1111-4111-8111-111111111111'
ITEM = '22222222-2222-4222-8222-222222222222'
PHOTO = '55555555-5555-4555-8555-555555555555'
OWNER = '0x' + 'a' * 40


class ContentMigrationCases:
    def content_load(self, db, source, step_name, rows):
        """Stages `rows` (SQL value tuples, in manifest column order) and runs the step's load."""
        step = next(s for s in harness.migrate.load_source(harness.migrate.SOURCES / source).steps
                    if s.name == step_name)
        defs = ', '.join(f'"{n}" {t}' for n, t in step.columns)
        values = ', '.join(f'({row})' for row in rows)
        variables = "\\set camera_api_url 'https://camera.example.test'\n" if source == 'camera-reel-service' else ''
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.sql(db, variables + f'BEGIN;\nCREATE TEMP TABLE stage ({defs}) ON COMMIT DROP;\n'
                         f'INSERT INTO stage VALUES {values};\n{step.load.read_text()}\nCOMMIT;\n')
        return err.getvalue()

    def content_db(self, name):
        db = self.catalyst_db(name)
        self.sql(db, harness.migrate.CHECKPOINT_DDL)
        return db

    @staticmethod
    def builder_rows(name, updated, contract='NULL'):
        collection = (f"'{COLLECTION}', '{name}', '{OWNER}', NULL, {contract}, NULL, NULL, "
                      f"'2026-01-01Z', '{updated}', NULL, NULL")
        item = (f"'{ITEM}', '{COLLECTION}', '{OWNER}', '{name}', 'd', NULL, NULL, 'wearable', 'Common', '', '', "
                f"'{{}}', NULL, NULL, NULL, NULL, NULL, false, 'pending', 0, NULL, NULL, '2026-01-01Z', '{updated}'")
        return collection, item

    def import_builder(self, db, name, updated, contract='NULL'):
        collection, item = self.builder_rows(name, updated, contract)
        self.content_load(db, 'builder-server', 'collection', [collection])
        self.content_load(db, 'builder-server', 'item', [item])

    def builder_names(self, db):
        return self.sql(db, f"""SELECT (SELECT name FROM builder.collections WHERE id = '{COLLECTION}'),
                                       (SELECT name FROM builder.items WHERE id = '{ITEM}');\n""").strip()

    def test_when_planning_the_builder_import_it_should_carry_no_scene_step(self):
        source = harness.migrate.load_source(harness.migrate.SOURCES / 'builder-server')
        self.assertEqual([s.name for s in source.steps], ['collection', 'item', 'item_content'])
        self.assertFalse(any(t.startswith('builder.project') or t.startswith('builder.asset')
                             for s in source.steps for t in s.targets))

    def test_when_importing_builder_rows_it_should_land_them_with_normalized_values(self):
        db = self.content_db('content_builder_import')
        self.import_builder(db, 'Upstream', '2026-02-01Z')
        self.assertEqual(self.builder_names(db), 'Upstream\tUpstream')
        self.assertEqual(self.sql(db, f"SELECT rarity, price IS NULL, beneficiary IS NULL, is_approved "
                                      f"FROM builder.items WHERE id = '{ITEM}';\n").strip(), 'common\tt\tt\tf')

    def test_when_importing_a_standard_collection_it_should_keep_its_contract_and_store_no_published_state(self):
        db = self.content_db('content_builder_contract')
        contract = '0x' + 'c' * 40
        self.import_builder(db, 'Upstream', '2026-02-01Z', f"'{contract}'")
        self.assertEqual(self.sql(db, f"SELECT contract_address, publication_pending FROM builder.collections "
                                      f"WHERE id = '{COLLECTION}';\n").strip(), f'{contract}\tf')
        self.assertEqual(self.sql(db, "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'builder' "
                                      "AND table_name = 'collections' AND column_name IN ('is_published', 'is_approved');\n"
                                      ).strip(), '0')

    def test_when_importing_a_third_party_collection_it_should_keep_its_linked_contract(self):
        db = self.content_db('content_builder_linked')
        provider = 'urn:decentraland:matic:collections-thirdparty:tp'
        self.content_load(db, 'builder-server', 'collection', [
            f"'{COLLECTION}', 'Linked', '', NULL, NULL, '{provider}:hats', '{provider}', "
            f"'2026-01-01Z', '2026-02-01Z', '0x{'AB' * 20}', 'mainnet'"])
        self.assertEqual(self.sql(db, f"SELECT linked_contract_address, linked_contract_network FROM builder.collections "
                                      f"WHERE id = '{COLLECTION}';\n").strip(), f"0x{'ab' * 20}\tmainnet")

    def test_when_importing_an_item_it_should_keep_its_documents_and_contents_in_the_saved_order(self):
        db = self.content_db('content_builder_item_order')
        collection, _ = self.builder_rows('Ordered', '2026-02-01Z')
        self.content_load(db, 'builder-server', 'collection', [collection])
        item = (f"'{ITEM}', '{COLLECTION}', '{OWNER}', 'Ordered', 'd', NULL, NULL, 'wearable', 'common', NULL, NULL, "
                f"'{{\"tags\": [], \"category\": \"hat\"}}', '{{\"triangles\": 1, \"bodies\": 1}}', NULL, NULL, NULL, NULL, "
                f"false, 'pending', 0, NULL, NULL, '2026-01-01Z', '2026-02-01Z'")
        self.content_load(db, 'builder-server', 'item', [item])
        self.assertEqual(self.sql(db, f"SELECT data::text, metrics::text FROM builder.items WHERE id = '{ITEM}';\n").strip(),
                         '{"tags": [], "category": "hat"}\t{"triangles": 1, "bodies": 1}')
        self.sql(db, "INSERT INTO dcl_migration.blob_inventory (source, object_key, hash, size, content_type) VALUES "
                     "('builder-server', 'bafkzthumb', 'bafkzthumb', 3, 'image/png'),"
                     "('builder-server', 'bafkamodel', 'bafkamodel', 5, 'model/gltf-binary');\n")
        self.content_load(db, 'builder-server', 'item_content', [
            f"'{ITEM}', 'thumbnail.png', 'bafkzthumb', '{OWNER}', 0",
            f"'{ITEM}', 'model.glb', 'bafkamodel', '{OWNER}', 1",
        ])
        self.assertEqual(self.sql(db, f"SELECT string_agg(file, ',' ORDER BY position) FROM builder.item_contents "
                                      f"WHERE item_id = '{ITEM}';\n").strip(), 'thumbnail.png,model.glb')
        self.assertEqual(self.sql(db, "SELECT string_agg(hash || ' ' || uploaded_by, ',' ORDER BY hash) "
                                      "FROM builder.uploaded_contents;\n").strip(),
                         f'bafkamodel {OWNER},bafkzthumb {OWNER}')

    def test_when_importing_an_items_contents_it_should_attribute_each_name_to_the_items_owner(self):
        db = self.content_db('content_builder_named_by')
        collection, item = self.builder_rows('Named', '2026-02-01Z')
        self.content_load(db, 'builder-server', 'collection', [collection])
        self.content_load(db, 'builder-server', 'item', [item])
        self.sql(db, "INSERT INTO dcl_migration.blob_inventory (source, object_key, hash, size, content_type) VALUES "
                     "('builder-server', 'bafkzthumb', 'bafkzthumb', 3, 'image/png'),"
                     "('builder-server', 'bafkamodel', 'bafkamodel', 5, 'model/gltf-binary'),"
                     "('builder-server', 'bafkbmodel', 'bafkbmodel', 5, 'model/gltf-binary');\n")
        rows = [f"'{ITEM}', 'thumbnail.png', 'bafkzthumb', '{OWNER.upper().replace('0X', '0x')}', 0",
                f"'{ITEM}', 'model.glb', 'bafkamodel', '{OWNER}', 1"]
        self.content_load(db, 'builder-server', 'item_content', rows)
        writers = f"SELECT string_agg(file || ' ' || named_by, ',' ORDER BY file) FROM builder.item_contents WHERE item_id = '{ITEM}';\n"
        self.assertEqual(self.sql(db, writers).strip(), f'model.glb {OWNER.lower()},thumbnail.png {OWNER.lower()}')
        other = '0x' + 'b' * 40
        self.sql(db, f"UPDATE builder.item_contents SET named_by = '{other}' WHERE item_id = '{ITEM}';\n")
        self.content_load(db, 'builder-server', 'item_content', [
            f"'{ITEM}', 'thumbnail.png', 'bafkzthumb', '{OWNER}', 1",
            f"'{ITEM}', 'model.glb', 'bafkbmodel', '{OWNER}', 0"])
        self.assertEqual(self.sql(db, writers).strip(), f'model.glb {OWNER.lower()},thumbnail.png {other}')

    def test_when_the_item_documents_release_has_not_run_it_should_refuse_to_import_items(self):
        db = self.content_db('content_builder_before_release')
        collection, item = self.builder_rows('Early', '2026-02-01Z')
        self.content_load(db, 'builder-server', 'collection', [collection])
        self.sql(db, "ALTER TABLE builder.items DROP CONSTRAINT items_data_object_check;\n"
                     "ALTER TABLE builder.items ALTER COLUMN data DROP DEFAULT;\n"
                     "ALTER TABLE builder.items ALTER COLUMN data TYPE jsonb USING data::jsonb;\n")
        with self.assertRaisesRegex(harness.migrate.MigrationError,
                                    'schemas/releases/builder-item-documents-and-uploaders.sql'):
            self.content_load(db, 'builder-server', 'item', [item])
        self.assertEqual(self.sql(db, 'SELECT count(*) FROM builder.items;\n').strip(), '0')

    def test_when_the_node_wrote_builder_rows_after_upstream_it_should_keep_them_on_reimport(self):
        db = self.content_db('content_builder_node_wins')
        self.import_builder(db, 'Upstream', '2026-02-01Z')
        self.sql(db, f"""UPDATE builder.collections SET name = 'Node', updated_at = '2026-03-01' WHERE id = '{COLLECTION}';
                         UPDATE builder.items SET name = 'Node', updated_at = '2026-03-01' WHERE id = '{ITEM}';\n""")
        self.import_builder(db, 'Upstream again', '2026-02-15Z')
        self.assertEqual(self.builder_names(db), 'Node\tNode')
        self.import_builder(db, 'Upstream later', '2026-04-01Z')
        self.assertEqual(self.builder_names(db), 'Upstream later\tUpstream later')

    def camera_rows(self, is_public):
        return [f"'{PHOTO}', '{OWNER}', 'https://camera.example.test/api/images/{PHOTO}-photo.png', "
                f"'https://camera.example.test/api/images/{PHOTO}-thumbnail-photo.png', {is_public}, "
                f"'{{\"userAddress\": \"{OWNER}\"}}', '2026-01-01Z'"]

    def camera_db(self, name):
        db = self.content_db(name)
        self.sql(db, "INSERT INTO dcl_migration.blob_inventory (source, object_key, hash, size, content_type) VALUES "
                     f"('camera-reel-service', '{PHOTO}-photo.png', 'bafkreiimage', 10, 'image/png'),"
                     f"('camera-reel-service', '{PHOTO}-thumbnail-photo.png', 'bafkreithumb', 5, 'image/png');\n")
        return db

    def is_public(self, db):
        return self.sql(db, f"SELECT is_public FROM camera_reel.images WHERE id = '{PHOTO}';\n").strip()

    def test_when_importing_a_photo_it_should_name_its_blob_and_follow_upstream_visibility(self):
        db = self.camera_db('content_camera_import')
        self.content_load(db, 'camera-reel-service', 'image', self.camera_rows('false'))
        self.assertEqual(self.sql(db, f"SELECT user_address, image_hash, thumbnail_hash, is_public, review_status "
                                      f"FROM camera_reel.images WHERE id = '{PHOTO}';\n").strip(),
                         f'{OWNER}\tbafkreiimage\tbafkreithumb\tf\tok')
        self.assertEqual(self.sql(db, 'SELECT id FROM camera_reel.imported_images;\n').strip(), PHOTO)
        self.content_load(db, 'camera-reel-service', 'image', self.camera_rows('true'))
        self.assertEqual(self.is_public(db), 't')

    def test_when_the_owner_toggled_visibility_on_the_node_it_should_keep_it_on_reimport(self):
        db = self.camera_db('content_camera_node_wins')
        self.content_load(db, 'camera-reel-service', 'image', self.camera_rows('false'))
        self.sql(db, f"UPDATE camera_reel.images SET is_public = true WHERE id = '{PHOTO}';\n")
        self.content_load(db, 'camera-reel-service', 'image', self.camera_rows('false'))
        self.assertEqual(self.is_public(db), 't')

    @staticmethod
    def schedule_row(name, updated):
        return (f"'s1', '{name}', NULL, NULL, NULL, '[]', NULL, NULL, true, '2026-01-01Z', '{updated}'")

    def test_when_importing_schedules_it_should_keep_a_newer_node_row(self):
        db = self.content_db('content_schedules')
        self.content_load(db, 'events', 'schedule', [self.schedule_row('Upstream', '2026-02-01Z')])
        self.sql(db, "UPDATE explore.schedules_local SET name = 'Node', updated_at = '2026-03-01' WHERE id = 's1';\n")
        self.content_load(db, 'events', 'schedule', [self.schedule_row('Upstream again', '2026-02-15Z')])
        name = "SELECT name FROM explore.schedules_local WHERE id = 's1';\n"
        self.assertEqual(self.sql(db, name).strip(), 'Node')
        self.content_load(db, 'events', 'schedule', [self.schedule_row('Upstream later', '2026-04-01Z')])
        self.assertEqual(self.sql(db, name).strip(), 'Upstream later')

    def test_when_importing_profile_permissions_it_should_grant_them_and_report_the_count(self):
        db = self.content_db('content_profile_permissions')
        row = ("'{user}', NULL, false, NULL, NULL, true, false, false, '{perms}', '2026-01-01Z', '2026-01-01Z'")
        notices = self.content_load(db, 'events', 'profile_settings', [
            row.format(user=OWNER, perms='["approve_any_event", "edit_any_profile"]'),
            row.format(user='0x' + 'b' * 40, perms='[]')])
        self.assertIn('importing 2 permission grants (approve_any_event, edit_any_profile)', notices)
        self.assertEqual(self.sql(db, 'SELECT "user", permissions_granted FROM explore.event_profile_settings '
                                      'ORDER BY 1;\n').splitlines(),
                         [f'{OWNER}\tt', f'0x{"b" * 40}\tf'])

    def test_when_a_badge_was_edited_on_the_node_it_should_keep_it_on_reimport(self):
        src, tgt = self.badges_dbs()
        run = ('run', 'badges', '--step', 'catalog')
        self.assertEqual(self.migrate(*run, sources_dir=harness.migrate.SOURCES)[0], 0)
        self.sql(tgt, "UPDATE social.badge_definitions SET name = 'Node name' WHERE id = 'dclcitizen';\n"
                      "UPDATE social.badge_tiers SET tier_name = 'Node tier' WHERE tier_id = 'emotionista-bronze';\n")
        self.sql(src, "UPDATE badge_definitions SET description = 'Upstream edit' "
                      "WHERE id IN ('dclcitizen', 'emotionista', 'open_for_business');\n")
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code, out = self.migrate(*run, sources_dir=harness.migrate.SOURCES)
        self.assertEqual(code, 0, out)
        self.assertIn('kept badge(s) dclcitizen, emotionista as edited on the node', err.getvalue())
        self.assertEqual(self.sql(tgt, "SELECT id, name, description FROM social.badge_definitions WHERE id IN "
                                       "('dclcitizen', 'emotionista', 'open_for_business') ORDER BY id;\n")
                         .splitlines(),
                         ['dclcitizen\tNode name\tLanded', 'emotionista\tEmotionista\tEmotes purchased',
                          'open_for_business\tOpen for Business\tUpstream edit'])
        self.assertEqual(self.sql(tgt, "SELECT tier_name FROM social.badge_tiers "
                                       "WHERE tier_id = 'emotionista-bronze';\n").strip(), 'Node tier')
