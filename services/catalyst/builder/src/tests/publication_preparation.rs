use serde_json::json;
use uuid::Uuid;

use crate::drafts::{CollectionDraft, ItemDraft};

use super::support::{cleanup, scratch_component, unique};

const OWNER: &str = "0x1111111111111111111111111111111111111111";
const OTHER: &str = "0x2222222222222222222222222222222222222222";

#[tokio::test]
async fn publication_uses_owned_persisted_files_and_stable_item_order() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let id = Uuid::new_v4();
    let collection: CollectionDraft =
        serde_json::from_value(json!({"id": id, "name": "Publication"})).unwrap();
    store
        .save_collection_draft(OWNER, &collection)
        .await
        .unwrap();
    assert!(store.prepare_publication(OWNER, id).await.is_err());
    let mut item: ItemDraft = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "name": "Hat", "type": "wearable", "collection_id": id,
        "data": {"category": "hat", "representations": [{"mainFile": "model.glb", "contents": ["model.glb"],
        "bodyShapes": ["urn:decentraland:off-chain:base-avatars:BaseMale"]}]},
        "price": "0", "rarity": "rare", "thumbnail": "thumbnail.png"
    })).unwrap();
    store.save_item_draft(OWNER, &item).await.unwrap();
    assert!(store.prepare_publication(OWNER, id).await.is_err());
    let files = store
        .save_item_files(
            OWNER,
            item.id,
            vec![
                ("model.glb".into(), unique("model")),
                ("thumbnail.png".into(), unique("image")),
            ],
        )
        .await
        .unwrap();
    let hashes: Vec<String> = files.values().cloned().collect();
    item.contents = files.clone();
    store.save_item_draft(OWNER, &item).await.unwrap();
    let first = store.prepare_publication(OWNER, id).await.unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].metadata, "1:w:Hat::hat:BaseMale");
    assert!(store.prepare_publication(OTHER, id).await.is_err());
    assert_eq!(
        first.revision,
        store.prepare_publication(OWNER, id).await.unwrap().revision
    );
    let second_id = Uuid::new_v4();
    let first_id = item.id;
    item.id = second_id;
    store.save_item_draft(OWNER, &item).await.unwrap();
    sqlx::query("UPDATE builder.items SET created_at='2026-01-01T00:00:00' WHERE collection_id=$1")
        .bind(id)
        .execute(store.pool())
        .await
        .unwrap();
    let prepared = store.prepare_publication(OWNER, id).await.unwrap();
    let mut expected = [first_id.to_string(), second_id.to_string()];
    expected.sort();
    assert_eq!(
        prepared
            .items
            .iter()
            .map(|i| i.id.clone())
            .collect::<Vec<_>>(),
        expected,
        "equal created_at falls back to the id order"
    );
    assert_ne!(prepared.revision, first.revision);
    let refused = sqlx::query("DELETE FROM builder.uploaded_contents WHERE hash=$1")
        .bind(&files["model.glb"])
        .execute(store.pool())
        .await;
    assert!(
        refused.is_err(),
        "a draft still names the upload, so the FK refuses the delete: the old missing_content case cannot arise"
    );
    assert!(store.prepare_publication(OWNER, id).await.is_ok());
    assert!(
        !store
            .collection_by_id(&id)
            .await
            .unwrap()
            .unwrap()
            .is_published,
        "preparation does not claim on-chain publication"
    );
    cleanup(&store, &[id], &hashes).await;
}

async fn published_draft(store: &crate::items::ItemsComponent, linked: bool) -> (Uuid, Uuid) {
    let id = Uuid::new_v4();
    let urn = format!(
        "urn:decentraland:matic:collections-thirdparty:guard{}:c",
        id.simple()
    );
    let mut draft = json!({ "id": id, "name": "Guarded" });
    if linked {
        draft["urn"] = json!(urn);
    }
    store
        .save_collection_draft(OWNER, &serde_json::from_value(draft).unwrap())
        .await
        .unwrap();
    let item: ItemDraft = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "name": "Hat", "type": "wearable", "collection_id": id,
        "data": {"category": "hat"}, "price": "0", "rarity": "rare"
    }))
    .unwrap();
    store.save_item_draft(OWNER, &item).await.unwrap();
    (id, item.id)
}

async fn publication_rows(store: &crate::items::ItemsComponent, id: Uuid) -> (i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM builder.collection_publications WHERE collection_id = $1), \
                (SELECT count(*) FROM builder.linked_collection_publications WHERE collection_id = $1)",
    )
    .bind(id)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

fn already_published(err: crate::http::ApiError) {
    assert_eq!(
        (err.status(), err.to_string()),
        (400, "Collection has already been published".to_string())
    );
}

/// A collection published by the copy of the subgraph or by a minted item
/// can't start another publication in either flow, and a published receipt
/// keeps the standard flow on the publication it recorded.
#[tokio::test]
async fn a_published_collection_cannot_begin_another_publication() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let mut created = Vec::new();
    for linked in [false, true] {
        for signal in ["copy", "minted"] {
            if linked && signal == "copy" {
                continue;
            }
            let (id, item) = published_draft(&store, linked).await;
            created.push(id);
            let contract = if signal == "copy" {
                Some(super::support::on_chain(store.pool(), id, false).await)
            } else {
                sqlx::query("UPDATE builder.items SET blockchain_item_id = '0' WHERE id = $1")
                    .bind(item)
                    .execute(store.pool())
                    .await
                    .unwrap();
                None
            };
            if linked {
                already_published(
                    store
                        .prepare_linked_publication(OWNER, id)
                        .await
                        .unwrap_err(),
                );
                already_published(
                    store
                        .begin_linked_publication(OWNER, id, "any revision")
                        .await
                        .unwrap_err(),
                );
            } else {
                already_published(store.prepare_publication(OWNER, id).await.unwrap_err());
                already_published(
                    store
                        .begin_publication(OWNER, id, "any revision")
                        .await
                        .unwrap_err(),
                );
            }
            assert_eq!(
                publication_rows(&store, id).await,
                (0, 0),
                "linked={linked} {signal}: nothing begun"
            );
            if let Some(contract) = contract {
                super::support::off_chain(store.pool(), &contract).await;
            }
        }
    }
    let (id, _) = published_draft(&store, false).await;
    created.push(id);
    sqlx::query(
        "INSERT INTO builder.collection_publications (collection_id, preparation, status, tx_hash, contract_address) \
         VALUES ($1, $4, 'published', $2, $3)",
    )
    .bind(id)
    .bind(format!("0x{}", Uuid::new_v4().simple()))
    .bind(format!("0x{}", Uuid::new_v4().simple().to_string()[..20].repeat(2)))
    .bind(json!({
        "id": id, "revision": "recorded", "chain_id": 137, "manager": OWNER, "factory": OWNER,
        "forwarder": OWNER, "salt": "0x00", "name": "Guarded", "symbol": "DCL-GRDD",
        "base_uri": "https://example.org/", "creator": OWNER, "items": []
    }))
    .execute(store.pool())
    .await
    .unwrap();
    assert_eq!(
        store
            .begin_publication(OWNER, id, "another revision")
            .await
            .unwrap_err()
            .status(),
        409,
        "a published receipt keeps the collection on its recorded publication"
    );
    let status: String = sqlx::query_scalar(
        "SELECT status FROM builder.collection_publications WHERE collection_id = $1",
    )
    .bind(id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(status, "published");
    let (id, _) = published_draft(&store, true).await;
    created.push(id);
    sqlx::query(
        "INSERT INTO builder.linked_collection_publications (collection_id, preparation, snapshot, salt, status, cheque) \
         VALUES ($1, $3, '{}', $2, 'submitted', $4)",
    )
    .bind(id)
    .bind(format!("0x{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()))
    .bind(json!({
        "id": id, "revision": "recorded", "name": "Guarded", "creator": OWNER,
        "urn": "urn:decentraland:matic:collections-thirdparty:guard:c",
        "third_party_id": "urn:decentraland:matic:collections-thirdparty:guard", "item_ids": []
    }))
    .bind(json!({ "qty": 1, "salt": "0x00", "signature": "0x00" }))
    .execute(store.pool())
    .await
    .unwrap();
    assert_eq!(
        store
            .begin_linked_publication(OWNER, id, "another revision")
            .await
            .unwrap_err()
            .status(),
        409,
        "a submitted linked publication keeps the collection on it"
    );
    assert_eq!(publication_rows(&store, id).await, (0, 1));
    cleanup(&store, &created, &[]).await;
}
