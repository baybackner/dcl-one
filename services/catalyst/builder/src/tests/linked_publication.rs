use crate::access::granted;
use alloy::signers::{SignerSync, local::PrivateKeySigner};
use alloy_primitives::{B256, U256, address};
use alloy_sol_types::{SolStruct, eip712_domain, sol};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::drafts::CollectionDraft;
use crate::items::ItemsComponent;
use crate::linked_publication::LinkedPublicationCheque;

use super::support::{
    OWNER_KEY, cleanup, contents_of, save_item, scratch_component, unique, upload,
};

const OWNER: &str = "0x19e7e376e7c213b7e7e7e46cc70a5dd086daff2a";
const OTHER: &str = "0x2222222222222222222222222222222222222222";
const PROVIDER: &str = "urn:decentraland:matic:collections-thirdparty:provider";

sol! {
    struct ConsumeSlots {
        string thirdPartyId;
        uint256 qty;
        bytes32 salt;
    }
}

fn authorization(salt: &str) -> LinkedPublicationCheque {
    let signer: PrivateKeySigner = OWNER_KEY.parse().unwrap();
    let domain = eip712_domain! {
        name: "Decentraland Third Party Registry",
        version: "1",
        verifying_contract: address!("1C436C1EFb4608dFfDC8bace99d2B03c314f3348"),
        salt: B256::from(U256::from(137).to_be_bytes::<32>()),
    };
    let data = ConsumeSlots {
        thirdPartyId: PROVIDER.into(),
        qty: U256::from(1),
        salt: salt.parse().unwrap(),
    };
    LinkedPublicationCheque {
        qty: 1,
        salt: salt.into(),
        signature: signer
            .sign_hash_sync(&data.eip712_signing_hash(&domain))
            .unwrap()
            .to_string(),
    }
}

#[tokio::test]
async fn linked_publication_freezes_content_and_recovers_the_same_authorization() {
    let Some(store) = scratch_component().await else {
        return;
    };
    let id = Uuid::new_v4();
    let mut collection: CollectionDraft = serde_json::from_value(json!({
        "id": id, "name": "Linked hats", "urn": format!("{PROVIDER}:{id}")
    }))
    .unwrap();
    super::support::save_collection(
        &store,
        OWNER,
        serde_json::json!({"id": collection.id, "name": collection.name, "urn": collection.urn}),
    )
    .await
    .unwrap();
    assert!(
        store
            .prepare_linked_publication(&granted::publisher(OWNER, id))
            .await
            .is_err()
    );
    let (model, thumbnail) = (unique("model"), unique("thumbnail"));
    let mut item = json!({
        "id": Uuid::new_v4(), "name": "Hat", "type": "wearable", "collection_id": id,
        "thumbnail": "thumbnail.png", "data": {"category": "hat", "representations": [{
            "mainFile": "hat.glb", "contents": ["hat.glb"], "bodyShapes": ["urn:decentraland:off-chain:base-avatars:BaseMale"]
        }]}, "metrics": {"meshes": 1, "bodies": 1, "materials": 1, "textures": 1, "triangles": 100, "entities": 1}
    });
    let item_id = Uuid::parse_str(item["id"].as_str().unwrap()).unwrap();
    save_item(&store, OWNER, &item).await.unwrap();
    assert!(
        store
            .prepare_linked_publication(&granted::publisher(OWNER, id))
            .await
            .is_err()
    );
    item["contents"] = contents_of(&[("hat.glb", &model), ("thumbnail.png", &thumbnail)]);
    save_item(&store, OWNER, &item).await.unwrap();
    upload(&store, OWNER, item_id, &[&model, &thumbnail])
        .await
        .unwrap();
    let hashes: Vec<String> = item["contents"]
        .as_object()
        .unwrap()
        .values()
        .map(|h| h.as_str().unwrap().to_string())
        .collect();
    let stale = store
        .prepare_linked_publication(&granted::publisher(OWNER, id))
        .await
        .unwrap();
    collection.name = "Updated hats".into();
    super::support::save_collection(
        &store,
        OWNER,
        serde_json::json!({"id": collection.id, "name": collection.name, "urn": collection.urn}),
    )
    .await
    .unwrap();
    assert!(
        store
            .begin_linked_publication(&granted::publisher(OWNER, id), &stale.revision)
            .await
            .is_err()
    );
    assert!(
        store
            .prepare_linked_publication(&granted::publisher(OTHER, id))
            .await
            .is_err()
    );
    assert!(
        store
            .prepare_publication(&granted::publisher(OWNER, id))
            .await
            .is_err()
    );
    let prepared = store
        .prepare_linked_publication(&granted::publisher(OWNER, id))
        .await
        .unwrap();
    assert_eq!(prepared.item_ids, vec![item_id.to_string()]);
    let (one, two) = (granted::publisher(OWNER, id), granted::publisher(OWNER, id));
    let (a, b) = tokio::join!(
        store.begin_linked_publication(&one, &prepared.revision),
        store.begin_linked_publication(&two, &prepared.revision)
    );
    let first = a.unwrap();
    assert_eq!(first.salt, b.unwrap().salt);
    assert_eq!(first.status, "prepared");
    assert!(
        super::support::save_collection(
            &store,
            OWNER,
            serde_json::json!({"id": collection.id, "name": collection.name, "urn": collection.urn})
        )
        .await
        .is_err()
    );
    assert_eq!(
        save_item(&store, OWNER, &item).await.unwrap_err().status(),
        423
    );
    assert!(
        upload(&store, OWNER, item_id, &[&unique("changed")])
            .await
            .is_err()
    );
    store
        .cancel_publication(&granted::publisher(OWNER, id))
        .await
        .unwrap();
    assert!(
        save_item(&store, OWNER, &item).await.is_err(),
        "standard cancellation must not unlock linked publication"
    );
    assert!(
        store
            .cancel_linked_publication(&granted::publisher(OTHER, id))
            .await
            .is_err()
    );
    store
        .cancel_linked_publication(&granted::publisher(OWNER, id))
        .await
        .unwrap();
    save_item(&store, OWNER, &item).await.unwrap();
    let prepared = store
        .prepare_linked_publication(&granted::publisher(OWNER, id))
        .await
        .unwrap();
    let second = store
        .begin_linked_publication(&granted::publisher(OWNER, id), &prepared.revision)
        .await
        .unwrap();
    assert_ne!(first.salt, second.salt);
    let cheque = authorization(&second.salt);
    assert!(
        store
            .authorize_linked_publication(&granted::publisher(OWNER, id), &cheque)
            .await
            .is_err()
    );
    let claimed = store
        .claim_linked_publication(&granted::publisher(OWNER, id), &prepared.revision)
        .await
        .unwrap();
    assert_eq!(claimed.status, "signing");
    assert_eq!(
        store
            .claim_linked_publication(&granted::publisher(OWNER, id), &prepared.revision)
            .await
            .unwrap()
            .salt,
        second.salt
    );
    assert!(
        store
            .claim_linked_publication(&granted::publisher(OTHER, id), &prepared.revision)
            .await
            .is_err()
    );
    assert!(
        store
            .claim_linked_publication(&granted::publisher(OWNER, id), "other revision")
            .await
            .is_err()
    );
    assert!(
        store
            .cancel_linked_publication(&granted::publisher(OWNER, id))
            .await
            .is_err()
    );
    let mut invalid = cheque.clone();
    invalid.qty = 2;
    assert!(
        store
            .authorize_linked_publication(&granted::publisher(OWNER, id), &invalid)
            .await
            .is_err()
    );
    invalid = cheque.clone();
    invalid.salt = first.salt;
    assert!(
        store
            .authorize_linked_publication(&granted::publisher(OWNER, id), &invalid)
            .await
            .is_err()
    );
    invalid = cheque.clone();
    invalid.signature = format!("0x{}", "00".repeat(65));
    assert!(
        store
            .authorize_linked_publication(&granted::publisher(OWNER, id), &invalid)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .linked_publication_state(&granted::publisher(OWNER, id))
            .await
            .unwrap()
            .unwrap()
            .status,
        "signing"
    );
    assert!(
        store
            .authorize_linked_publication(&granted::publisher(OTHER, id), &cheque)
            .await
            .is_err()
    );
    let authorized = store
        .authorize_linked_publication(&granted::publisher(OWNER, id), &cheque)
        .await
        .unwrap();
    assert_eq!(authorized.status, "authorized");
    let reopened = ItemsComponent::new(store.pool().clone(), store.blobs.clone());
    let persisted = reopened
        .linked_publication_state(&granted::publisher(OWNER, id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.cheque, Some(cheque.clone()));
    assert_eq!(persisted.preparation.revision, prepared.revision);
    assert_eq!(persisted.salt, second.salt);
    assert!(
        reopened
            .linked_publication_state(&granted::publisher(OTHER, id))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reopened
            .authorize_linked_publication(&granted::publisher(OWNER, id), &cheque)
            .await
            .unwrap()
            .status,
        "authorized"
    );
    assert!(
        reopened
            .cancel_linked_publication(&granted::publisher(OWNER, id))
            .await
            .is_err()
    );
    assert!(save_item(&reopened, OWNER, &item).await.is_err());
    let snapshot: Value = sqlx::query_scalar(
        "SELECT snapshot FROM builder.linked_collection_publications WHERE collection_id=$1",
    )
    .bind(id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(snapshot["items"][0]["contents"], item["contents"]);
    assert_eq!(snapshot["collection"]["name"], "Updated hats");
    let published = store
        .collection_by_id(&granted::read(id))
        .await
        .unwrap()
        .unwrap()
        .is_published;
    assert!(!published, "authorization is not proof of publication");
    cleanup(&store, &[id], &hashes).await;
}

/// Without `BUILDER_FOUNDATION_URL` (every posture but mirror) a signed
/// verification answers 503 before anything is sent; an unsigned one is
/// refused first, whatever is configured.
#[tokio::test]
async fn verify_answers_503_while_no_foundation_is_named() {
    use tower::ServiceExt;
    let id = Uuid::new_v4();
    let path = format!("/v1/collections/{id}/linked-publication/verify");
    let status = |state: crate::SharedState, signed: bool| {
        let path = path.clone();
        async move {
            let mut request =
                axum::http::Request::post(&path).header("content-type", "application/json");
            if signed {
                for (name, value) in &crate::shared::signed_fetch::test_support::signed_headers(
                    "post", &path, "{}", true,
                )
                .await
                {
                    request = request.header(name, value);
                }
            }
            crate::router(state)
                .oneshot(
                    request
                        .body(axum::body::Body::from(
                            r#"{"collection":{},"items":{},"curations":{}}"#,
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };
    let open = crate::Settings {
        writes_enabled: true,
        ..crate::Settings::default()
    };
    assert_eq!(
        status(
            super::support::state_with_settings(open.clone()).await,
            true
        )
        .await,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        status(
            super::support::state_with_settings(open.clone()).await,
            false
        )
        .await,
        axum::http::StatusCode::UNAUTHORIZED
    );
    let named = crate::Settings {
        foundation_url: Some("https://foundation.example.org".into()),
        ..open
    };
    assert_eq!(
        status(super::support::state_with_settings(named).await, false).await,
        axum::http::StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_linked_publication_takes_only_files_the_items_owner_uploaded() {
    use super::publication_preparation::{Uploader, name_files};
    let Some(store) = scratch_component().await else {
        return;
    };
    for uploaded in [Uploader::Nobody, Uploader::AnotherWallet, Uploader::Owner] {
        let id = Uuid::new_v4();
        super::support::save_collection(
            &store,
            OWNER,
            json!({"id": id, "name": "Linked possession", "urn": format!("{PROVIDER}:{id}")}),
        )
        .await
        .unwrap();
        let mut item = json!({
            "id": Uuid::new_v4(), "name": "Hat", "type": "wearable", "collection_id": id,
            "thumbnail": "thumbnail.png", "data": {"category": "hat", "representations": [{
                "mainFile": "hat.glb", "contents": ["hat.glb"], "bodyShapes": ["urn:decentraland:off-chain:base-avatars:BaseMale"]
            }]}, "metrics": {"meshes": 1, "bodies": 1, "materials": 1, "textures": 1, "triangles": 100, "entities": 1}
        });
        let (model, thumbnail) = (
            unique("linked possession model"),
            unique("linked possession thumbnail"),
        );
        name_files(
            &store,
            OWNER,
            OTHER,
            &mut item,
            [("hat.glb", &model), ("thumbnail.png", &thumbnail)],
            uploaded,
        )
        .await;
        let publisher = granted::publisher(OWNER, id);
        let prepared = store.prepare_linked_publication(&publisher).await;
        if uploaded == Uploader::Owner {
            let revision = prepared.unwrap().revision;
            store
                .begin_linked_publication(&publisher, &revision)
                .await
                .unwrap();
        } else {
            let refused = prepared.unwrap_err();
            assert_eq!(refused.status(), 400, "{uploaded:?}: {refused}");
            assert!(
                refused.to_string().contains("upload its files"),
                "{uploaded:?}: {refused}"
            );
            let begun = store
                .begin_linked_publication(&publisher, "any revision")
                .await
                .unwrap_err();
            assert_eq!(begun.status(), 400, "{uploaded:?}: {begun}");
            assert!(
                begun.to_string().contains("upload its files"),
                "{uploaded:?}: {begun}"
            );
            assert_eq!(
                store
                    .claim_linked_publication(&publisher, "any revision")
                    .await
                    .unwrap_err()
                    .status(),
                409,
                "{uploaded:?}: claimed"
            );
        }
        let hashes = [
            dcl_auth::hash_bytes_v1(&model),
            dcl_auth::hash_bytes_v1(&thumbnail),
        ];
        cleanup(&store, &[id], &hashes).await;
    }
}
