use crate::access::granted;
use serde_json::json;
use uuid::Uuid;

use crate::drafts::{CollectionDraft, ItemDraft};
use crate::items::ItemsComponent;
use dcl_auth::{hash_bytes_v1, verify_hash};

use super::support::{cleanup, off_chain, on_chain, save_collection, scratch_component, unique};

const OWNER: &str = "0x1111111111111111111111111111111111111111";
const OTHER: &str = "0x2222222222222222222222222222222222222222";

#[tokio::test]
async fn draft_delete_preserves_shared_uploads_and_serializes_with_publication() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let collection = Uuid::new_v4();
    save_collection(
        &store,
        OWNER,
        json!({ "id": collection, "name": "Delete fixture" }),
    )
    .await
    .unwrap();
    let mut item: ItemDraft = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "name": "Delete me", "type": "wearable", "collection_id": collection,
        "data": { "category": "upper_body" },
    }))
    .unwrap();
    store
        .save_item_draft(&granted::save(OWNER, item.id, item.collection_id), &item)
        .await
        .unwrap();
    let bytes = unique("shared model");
    let files = store
        .save_item_files(
            &granted::change(OWNER, item.id),
            vec![("model.glb".into(), bytes.clone())],
        )
        .await
        .unwrap();
    let deleted_id = item.id;
    item.id = Uuid::new_v4();
    item.contents = files.clone();
    store
        .save_item_draft(&granted::save(OWNER, item.id, item.collection_id), &item)
        .await
        .unwrap();
    let survivor = store.item_draft(&granted::item(item.id)).await.unwrap();
    assert_eq!(
        store
            .delete_item_draft(&granted::change(OTHER, deleted_id))
            .await
            .unwrap_err()
            .status(),
        401
    );
    let before = store.item_draft(&granted::item(deleted_id)).await.unwrap();

    let mut publishing = store.pool().begin().await.unwrap();
    let publisher: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *publishing)
        .await
        .unwrap();
    sqlx::query("UPDATE builder.collections SET publication_pending=true WHERE id=$1")
        .bind(collection)
        .execute(&mut *publishing)
        .await
        .unwrap();
    let change = granted::change(OWNER, deleted_id);
    let delete = store.delete_item_draft(&change);
    tokio::pin!(delete);
    tokio::select! {
        biased;
        _ = &mut delete => panic!("delete must wait for the publishing parent"),
        queued = crate::shared::test_wait::until(async || {
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
            )
            .bind(publisher)
            .fetch_one(store.pool())
            .await
            .unwrap()
        }) => assert!(queued, "delete never queued behind the publishing parent"),
    }
    publishing.commit().await.unwrap();
    assert_eq!(delete.await.unwrap_err().status(), 423);
    assert_eq!(
        store.item_draft(&granted::item(deleted_id)).await.unwrap(),
        before
    );

    sqlx::query("UPDATE builder.collections SET publication_pending=false WHERE id=$1")
        .bind(collection)
        .execute(store.pool())
        .await
        .unwrap();
    let contract = on_chain(store.pool(), collection, false).await;
    assert_eq!(
        store
            .delete_item_draft(&granted::change(OWNER, deleted_id))
            .await
            .unwrap_err()
            .status(),
        409
    );
    off_chain(store.pool(), &contract).await;
    sqlx::query("UPDATE builder.items SET is_published=true, urn_suffix='0', blockchain_item_id='0' WHERE id=$1")
        .bind(deleted_id).execute(store.pool()).await.unwrap();
    assert_eq!(
        store
            .delete_item_draft(&granted::change(OWNER, deleted_id))
            .await
            .unwrap_err()
            .status(),
        409
    );
    sqlx::query("UPDATE builder.items SET is_published=false, urn_suffix=NULL, blockchain_item_id=NULL WHERE id=$1")
        .bind(deleted_id)
        .execute(store.pool())
        .await
        .unwrap();

    store
        .delete_item_draft(&granted::change(OWNER, deleted_id))
        .await
        .unwrap();
    assert_eq!(
        store
            .item_draft(&granted::item(deleted_id))
            .await
            .unwrap_err()
            .status(),
        404
    );
    assert_eq!(
        store
            .delete_item_draft(&granted::change(OWNER, deleted_id))
            .await
            .unwrap_err()
            .status(),
        404
    );
    assert_eq!(
        store.item_draft(&granted::item(item.id)).await.unwrap(),
        survivor
    );
    assert_eq!(
        store
            .uploaded_content(&files["model.glb"])
            .await
            .unwrap()
            .as_deref(),
        Some(bytes.as_slice())
    );
    let links: i64 =
        sqlx::query_scalar("SELECT count(*) FROM builder.item_contents WHERE item_id=$1")
            .bind(deleted_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(links, 0);
    cleanup(&store, &[collection], &[files["model.glb"].clone()]).await;
}

#[tokio::test]
async fn drafts_and_files_persist_with_owner_and_publication_guards() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let collection = Uuid::new_v4();
    let draft = json!({ "id": collection, "name": "Collection draft" });
    let saved = save_collection(&store, OWNER, draft.clone()).await.unwrap();
    assert_eq!(saved["name"], "Collection draft");
    assert_eq!(saved["is_published"], false);
    assert_eq!(
        saved["salt"],
        format!(
            "{:#x}",
            crate::collections::contract::salt(&collection.to_string())
        ),
        "the salt is computed from the id"
    );
    assert!(
        saved["contract_address"]
            .as_str()
            .is_some_and(dcl_auth::is_eth_address)
    );
    assert!(save_collection(&store, OTHER, draft.clone()).await.is_err());
    assert!(
        store
            .collection_drafts(&granted::wallet(OTHER))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .collection_drafts(&granted::wallet(OWNER))
            .await
            .unwrap()
            .iter()
            .any(|c| c.collection.id == collection.to_string())
    );

    let id = Uuid::new_v4();
    let mut item: ItemDraft = serde_json::from_value(json!({
        "id": id, "name": "My wearable", "collection_id": collection,
        "type": "wearable", "data": { "category": "upper_body" }, "rarity": "common", "price": "0",
        "metrics": { "triangles": 42, "meshes": 1 }
    }))
    .unwrap();
    store
        .save_item_draft(&granted::save(OWNER, item.id, item.collection_id), &item)
        .await
        .unwrap();
    assert!(
        store
            .save_item_draft(&granted::save(OTHER, item.id, item.collection_id), &item)
            .await
            .is_err()
    );
    let bytes = unique("actual model bytes");
    let files = store
        .save_item_files(
            &granted::change(OWNER, id),
            vec![("model.glb".into(), bytes.clone())],
        )
        .await
        .unwrap();
    let hash = files["model.glb"].clone();
    assert!(verify_hash(&bytes, &hash));
    assert_eq!(
        store.uploaded_content(&hash).await.unwrap().as_deref(),
        Some(bytes.as_slice()),
        "the upload is served back from the ONE blob store"
    );
    assert!(store.uploaded_content_exists(&hash).await.unwrap());
    let (size, uploaded_by): (i64, String) =
        sqlx::query_as("SELECT size, uploaded_by FROM builder.uploaded_contents WHERE hash=$1")
            .bind(&hash)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(
        (size as usize, uploaded_by.as_str()),
        (bytes.len(), OWNER),
        "the acceptance record"
    );
    assert!(
        store
            .save_item_files(
                &granted::change(OTHER, id),
                vec![("model.glb".into(), unique("overwrite"))]
            )
            .await
            .is_err()
    );
    item.name = "Edited wearable".into();
    item.metrics = None;
    item.contents = files;
    store
        .save_item_draft(&granted::save(OWNER, item.id, item.collection_id), &item)
        .await
        .unwrap();
    let reopened = ItemsComponent::new(store.pool().clone(), store.blobs.clone())
        .item_draft(&granted::item(id))
        .await
        .unwrap();
    assert!(
        store
            .item_drafts(&granted::wallet(OWNER))
            .await
            .unwrap()
            .iter()
            .any(|d| d["id"] == id.to_string()),
        "the draft is listed among the owner's item drafts"
    );
    assert!(
        store
            .item_drafts(&granted::wallet(OTHER))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(reopened["name"], "Edited wearable");
    assert_eq!(reopened["metrics"], json!({ "triangles": 42, "meshes": 1 }));
    assert!(
        is_iso(reopened["created_at"].as_str().unwrap()),
        "{}",
        reopened["created_at"]
    );
    item.metrics = Some(json!({ "triangles": -1 }));
    assert!(
        store
            .save_item_draft(&granted::save(OWNER, item.id, item.collection_id), &item)
            .await
            .is_err()
    );
    item.metrics = None;
    assert_eq!(reopened["data"]["category"], "upper_body");
    assert_eq!(reopened["contents"]["model.glb"], hash);

    item.contents
        .insert("missing.glb".into(), hash_bytes_v1(&unique("not uploaded")));
    let refused = store
        .save_item_draft(&granted::save(OWNER, item.id, item.collection_id), &item)
        .await
        .unwrap_err();
    assert_eq!(
        refused.status(),
        400,
        "a hash this node never accepted is refused by the FK as the old probe did: {refused}"
    );
    assert_eq!(
        store.item_draft(&granted::item(id)).await.unwrap(),
        reopened,
        "invalid writes roll back metadata and content links"
    );
    item.contents.remove("missing.glb");
    let contract = on_chain(store.pool(), collection, true).await;
    assert!(save_collection(&store, OWNER, draft.clone()).await.is_err());
    assert!(
        store
            .save_item_files(
                &granted::change(OWNER, id),
                vec![("model.glb".into(), unique("overwrite"))]
            )
            .await
            .is_err()
    );
    item.collection_id = None;
    assert!(
        store
            .save_item_draft(&granted::save(OWNER, item.id, item.collection_id), &item)
            .await
            .is_err(),
        "cannot detach an item to bypass publication protection"
    );
    assert_eq!(
        store.uploaded_content(&hash).await.unwrap().as_deref(),
        Some(bytes.as_slice())
    );
    let linked_id = Uuid::new_v4();
    let provider = "urn:decentraland:matic:collections-thirdparty:example";
    let urn = format!("{provider}:{linked_id}");
    let mut linked = json!({ "id": linked_id, "name": "Linked draft", "urn": urn });
    let saved = save_collection(&store, OWNER, linked.clone())
        .await
        .unwrap();
    assert_eq!(saved["urn"], urn.as_str());
    assert_eq!(
        saved["eth_address"], "",
        "a third-party collection has no owner"
    );
    assert_eq!(saved["is_published"], false);
    linked["name"] = json!("Renamed linked draft");
    assert_eq!(
        save_collection(&store, OWNER, linked.clone())
            .await
            .unwrap()["name"],
        "Renamed linked draft"
    );
    assert!(
        save_collection(&store, OTHER, linked.clone())
            .await
            .is_err()
    );
    linked["urn"] = json!(null);
    assert!(
        save_collection(&store, OWNER, linked.clone())
            .await
            .is_err()
    );
    linked["urn"] = json!(format!(
        "urn:decentraland:matic:collections-thirdparty:other:{linked_id}"
    ));
    assert!(
        save_collection(&store, OWNER, linked.clone())
            .await
            .is_err()
    );
    let stored: Option<String> =
        sqlx::query_scalar("SELECT urn_suffix FROM builder.collections WHERE id = $1")
            .bind(linked_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some(urn.as_str()));
    let mut linked: CollectionDraft = serde_json::from_value(linked).unwrap();
    linked.urn = Some("urn:decentraland:matic:collections-v2:forged".into());
    assert!(linked.validate(linked_id).is_err());
    linked.urn = Some(format!("{provider}:bad suffix"));
    assert!(linked.validate(linked_id).is_err());
    off_chain(store.pool(), &contract).await;
    cleanup(&store, &[collection, linked_id], &[hash]).await;
}

#[tokio::test]
async fn an_item_draft_answers_iso_timestamps() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let collection = Uuid::new_v4();
    let saved = save_collection(
        &store,
        OWNER,
        json!({ "id": collection, "name": "Timestamps" }),
    )
    .await
    .unwrap();
    let item: ItemDraft = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "name": "Stamped", "type": "wearable", "collection_id": collection,
        "data": { "category": "upper_body" },
    }))
    .unwrap();
    let written = store
        .save_item_draft(&granted::save(OWNER, item.id, item.collection_id), &item)
        .await
        .unwrap();
    let read = store.item_draft(&granted::item(item.id)).await.unwrap();
    for (what, body) in [
        ("collection", &saved),
        ("saved item", &written),
        ("read item", &read),
    ] {
        for field in ["created_at", "updated_at"] {
            let value = body[field].as_str().unwrap_or_default();
            assert!(is_iso(value), "{what} {field}: {}", body[field]);
        }
    }
    cleanup(&store, &[collection], &[]).await;
}

fn is_iso(value: &str) -> bool {
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.3fZ").is_ok()
        && value.len() == "2026-01-01T00:00:00.000Z".len()
}

#[tokio::test]
async fn a_name_another_save_is_claiming_is_refused_once_that_save_commits() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let name = format!("Claimed {}", &first.simple().to_string()[..8]);
    let mut claim = store.pool().begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('builder.collections name:' || lower($1), 0))",
    )
    .bind(&name)
        .execute(&mut *claim)
        .await
        .unwrap();
    let waiting = {
        let pool = store.pool().clone();
        let name = name.to_uppercase();
        tokio::spawn(async move {
            let store = ItemsComponent::new(pool, super::support::temp_storage().await);
            save_collection(&store, OTHER, json!({ "id": second, "name": name })).await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "the second save waits for the name");
    sqlx::query("INSERT INTO builder.collections (id, name, eth_address) VALUES ($1, $2, $3)")
        .bind(first)
        .bind(&name)
        .bind(OWNER)
        .execute(&mut *claim)
        .await
        .unwrap();
    claim.commit().await.unwrap();
    let refused = waiting.await.unwrap().unwrap_err();
    assert_eq!(
        (refused.status(), refused.to_string().as_str()),
        (409, "Name already in use")
    );
    cleanup(&store, &[first, second], &[]).await;
}

#[tokio::test]
async fn a_submitted_third_party_collection_keeps_its_urn_but_takes_other_edits() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let id = Uuid::new_v4();
    let provider = format!(
        "urn:decentraland:matic:collections-thirdparty:kept{}",
        &id.simple().to_string()[..8]
    );
    let urn = format!("{provider}:hats");
    save_collection(
        &store,
        OWNER,
        json!({ "id": id, "name": "Kept", "urn": urn }),
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO builder.linked_collection_publications (collection_id, preparation, snapshot, salt, status, cheque) \
         VALUES ($1, '{\"revision\":\"r\"}', '{}', $2, 'submitted', '{}')",
    )
    .bind(id)
    .bind(format!("0x{}", "1".repeat(64)))
    .execute(store.pool())
    .await
    .unwrap();
    let renamed = save_collection(
        &store,
        OWNER,
        json!({ "id": id, "name": "Renamed", "urn": urn }),
    )
    .await
    .unwrap();
    assert_eq!(renamed["name"], "Renamed");
    let moved = save_collection(
        &store,
        OWNER,
        json!({ "id": id, "name": "Renamed", "urn": format!("{provider}:caps") }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (moved.status(), moved.to_string().as_str()),
        (
            409,
            "The third party collection already has published items. It can't be updated or inserted."
        )
    );
    cleanup(&store, &[id], &[]).await;
}

#[tokio::test]
async fn a_provider_links_a_contract_to_one_collection_and_never_unlinks_it() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let provider = format!(
        "urn:decentraland:matic:collections-thirdparty:linked{}",
        &first.simple().to_string()[..8]
    );
    let linked = |id: Uuid, suffix: &str, address: Option<&str>| {
        json!({ "id": id, "name": format!("Linked {suffix}"), "urn": format!("{provider}:{suffix}"),
            "linked_contract_address": address, "linked_contract_network": address.map(|_| "mainnet") })
    };
    let contract = format!("0x{}", "Ab".repeat(20));
    save_collection(&store, OWNER, linked(first, "one", Some(&contract)))
        .await
        .unwrap();
    let taken = save_collection(&store, OWNER, linked(second, "two", Some(&contract)))
        .await
        .unwrap_err();
    assert_eq!(
        (taken.status(), taken.to_string().as_str()),
        (
            200,
            "duplicate key value violates unique constraint \"linkedContract_linkedNetwork_thirdPartyId_unique\""
        )
    );
    let cleared = save_collection(&store, OWNER, linked(first, "one", None))
        .await
        .unwrap_err();
    assert_eq!(
        (cleared.status(), cleared.to_string().as_str()),
        (
            200,
            "Columns linked_contract_address, linked_contract_network cannot be set to null if they were previously not null"
        )
    );
    let kept: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT linked_contract_address, linked_contract_network FROM builder.collections WHERE id = $1",
    )
    .bind(first)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        kept,
        (Some(contract.to_lowercase()), Some("mainnet".into()))
    );
    save_collection(&store, OWNER, linked(second, "two", None))
        .await
        .unwrap();
    cleanup(&store, &[first, second], &[]).await;
}
