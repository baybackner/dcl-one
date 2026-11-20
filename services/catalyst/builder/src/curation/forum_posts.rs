//! A collection's one forum topic and the committee's replies in it.
//!
//! A topic is made once per collection. A request first claims it: in one
//! transaction that locks the collection row and decides access again, the
//! collection must be published and hold no link, and no other claim may be
//! younger than [`CLAIM_HOLDS`]; it records `forum_claimed_at` and commits.
//! Then, holding no connection, it asks the forum, within [`ATTEMPT`], to
//! create the topic named by the collection's id (`external_id`), and stores
//! the link and the first post's id in a second transaction, which clears
//! the claim. A request that finds a claim within [`CLAIM_HOLDS`] is told to
//! retry; one that finds an older claim takes it over and first asks the
//! forum for the topic by that id, storing it when the earlier attempt made
//! it. A failed attempt keeps its claim, so the forum is asked at most once
//! per collection per [`CLAIM_HOLDS`].

use std::time::Duration;

use chrono::TimeDelta;
use serde_json::{Value, json};
use uuid::Uuid;

use super::forum::{Created, Failure, Forum, Topic};
use super::forum_text::{self, PostCollection, PostItem};
use super::slack::shorten;
use crate::State;
use crate::access::{self, CollectionManage, ForumReply};
use crate::chain_state;
use crate::http::ApiError;

/// How long a claim keeps other requests from asking the forum.
pub const CLAIM_HOLDS: TimeDelta = TimeDelta::seconds(60);
/// How long one attempt may spend on the forum.
pub const ATTEMPT: Duration = Duration::from_secs(45);
/// The longest reply, in UTF-16 code units as builder-server counts.
pub const MAX_REPLY: usize = 10_000;

fn unavailable() -> ApiError {
    ApiError::service_unavailable("The forum is not available. Try again later.")
}

struct Claim {
    collection: PostCollection,
    third_party: Option<String>,
    owner: Option<String>,
    resumed: bool,
}

#[derive(sqlx::FromRow)]
struct ClaimRow {
    name: String,
    third_party_id: Option<String>,
    forum_link: Option<String>,
    fresh: Option<bool>,
}

async fn claim(state: &State, manage: &CollectionManage) -> Result<Claim, ApiError> {
    let id = manage.id();
    let mut tx = state.items.pool().begin().await?;
    let view = chain_state::for_decision(&mut tx).await?;
    let row: Option<ClaimRow> = sqlx::query_as(
        "SELECT name, third_party_id, forum_link, forum_claimed_at > timezone('UTC', clock_timestamp()) - $2::interval AS fresh \
         FROM builder.collections WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .bind(CLAIM_HOLDS)
    .fetch_optional(&mut *tx)
    .await?;
    let managed = access::confirm_collection_manage(&mut tx, &view, manage).await?;
    let Some(ClaimRow {
        name,
        third_party_id: third_party,
        forum_link: link,
        fresh,
    }) = row
    else {
        return Err(ApiError::internal(format!(
            "collection {id} vanished under its lock"
        )));
    };
    if !managed.published {
        return Err(
            ApiError::conflict("The collection is not published").with_data(json!({ "id": id }))
        );
    }
    if let Some(link) = link {
        return Err(ApiError::conflict("Forum post already exists")
            .with_data(json!({ "id": id, "forum_link": link })));
    }
    if state.settings.forum.is_none() {
        return Err(unavailable());
    }
    if fresh == Some(true) {
        return Err(ApiError::service_unavailable(
            "The forum post is being created. Try again later.",
        )
        .with_data(json!({ "id": id })));
    }
    let items: Vec<PostItem> =
        sqlx::query_as(
            "SELECT i.id, i.name, i.description, i.rarity, i.data->>'category' AS category, \
               (SELECT ic.hash FROM builder.item_contents ic WHERE ic.item_id = i.id AND ic.file = i.thumbnail) AS thumbnail_hash \
             FROM builder.items i WHERE i.collection_id = $1 ORDER BY i.created_at, i.id LIMIT $2",
        )
        .bind(id)
        .bind(forum_text::MAX_ITEMS as i64)
        .fetch_all(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE builder.collections SET forum_claimed_at = timezone('UTC', clock_timestamp()) WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Claim {
        collection: PostCollection {
            id,
            name,
            contract: managed.contract,
            items,
        },
        third_party,
        owner: managed.owner,
        resumed: fresh.is_some(),
    })
}

async fn claimed_name(state: &State, address: &str) -> Result<Option<String>, ApiError> {
    let avatar: Option<Value> = sqlx::query_scalar(
        "SELECT COALESCE(d.entity_metadata::jsonb -> 'v', d.entity_metadata::jsonb) #> '{avatars,0}' \
         FROM content.active_pointers ap JOIN content.deployments d ON d.entity_id = ap.entity_id \
         WHERE ap.pointer = $1 AND d.entity_type = 'profile'",
    )
    .bind(address.to_ascii_lowercase())
    .fetch_optional(state.items.pool())
    .await?
    .flatten();
    Ok(avatar.and_then(|avatar| {
        (avatar["hasClaimedName"] == true)
            .then(|| avatar["name"].as_str().map(str::to_string))
            .flatten()
            .filter(|name| !name.is_empty())
    }))
}

async fn make(
    state: &State,
    forum: &Forum,
    claim: &Claim,
    title: String,
) -> Result<Topic, Failure> {
    let id = claim.collection.id.to_string();
    if claim.resumed
        && let Some(topic) = forum.find(&state.foundation, &id).await?
    {
        return Ok(topic);
    }
    let raw = forum_text::body(
        &claim.collection,
        &state.settings.links,
        &state.settings.builder_server_url,
    );
    let first = forum_text::remove_emojis(&title);
    match forum.create(&state.foundation, &first, &raw, &id).await? {
        Created::Topic(topic) => Ok(topic),
        Created::DuplicateTitle => match (&claim.third_party, &claim.collection.contract) {
            (None, Some(contract)) => {
                let retried =
                    forum_text::remove_emojis(&forum_text::retried_title(&title, contract));
                match forum.create(&state.foundation, &retried, &raw, &id).await? {
                    Created::Topic(topic) => Ok(topic),
                    Created::DuplicateTitle => {
                        Err(Failure::Refused(reqwest::StatusCode::UNPROCESSABLE_ENTITY))
                    }
                }
            }
            _ => Err(Failure::Refused(reqwest::StatusCode::UNPROCESSABLE_ENTITY)),
        },
    }
}

/// `POST /v1/collections/:id/post`: makes the collection's forum topic and
/// answers its link.
pub async fn post(state: &State, manage: &CollectionManage) -> Result<String, ApiError> {
    let claim = claim(state, manage).await?;
    let forum = state.settings.forum.as_ref().ok_or_else(unavailable)?;
    let title = match (&claim.third_party, &claim.owner) {
        (Some(provider), _) => format!(
            "Third Party collection {} with URN: {provider}",
            claim.collection.name
        ),
        (None, owner) => {
            let owner = owner.as_deref().unwrap_or_default();
            let creator = match claimed_name(state, owner).await? {
                Some(name) => name,
                None => shorten(owner),
            };
            forum_text::title(&claim.collection.name, &creator)
        }
    };
    let id = claim.collection.id;
    let topic = tokio::time::timeout(ATTEMPT, make(state, forum, &claim, title))
        .await
        .unwrap_or(Err(Failure::Unreachable))
        .map_err(|failure| failure.api_error(id))?;
    let link = forum.link(&topic);
    let stored: Option<Option<String>> = sqlx::query_scalar(
        "WITH kept AS (UPDATE builder.collections SET forum_link = $2, forum_id = $3, forum_claimed_at = NULL \
           WHERE id = $1 AND forum_link IS NULL RETURNING forum_link) \
         SELECT COALESCE((SELECT forum_link FROM kept), (SELECT forum_link FROM builder.collections WHERE id = $1))",
    )
    .bind(id)
    .bind(&link)
    .bind(topic.post)
    .fetch_optional(state.items.pool())
    .await?;
    Ok(stored.flatten().unwrap_or(link))
}

fn reply_text(raw: Option<&str>, id: Uuid) -> Result<String, ApiError> {
    raw.filter(|raw| (1..=MAX_REPLY).contains(&raw.encode_utf16().count()))
        .map(|raw| raw.replace('@', "@\u{200b}"))
        .ok_or_else(|| ApiError::bad_request("Invalid forum post").with_data(json!({ "id": id })))
}

fn not_replied(id: Uuid) -> ApiError {
    ApiError::http(200, "Error creating forum post").with_data(json!({ "id": id }))
}

/// `POST /v1/collections/:id/curation/post`: replies `raw` in the
/// collection's topic, which the forum names from the topic's first post.
/// An `@` is followed by a zero-width space, so a reply mentions nobody.
pub async fn reply(state: &State, grant: &ForumReply, raw: Option<&str>) -> Result<(), ApiError> {
    let id = grant.collection();
    let post: Option<Option<i32>> =
        sqlx::query_scalar("SELECT forum_id FROM builder.collections WHERE id = $1")
            .bind(id)
            .fetch_optional(state.items.pool())
            .await?;
    let Some(post) = post else {
        return Err(ApiError::not_found("Collection not found").with_data(json!({ "id": id })));
    };
    let Some(post) = post else {
        return Err(
            ApiError::conflict("The collection does not have a forum post yet")
                .with_data(json!({ "id": id })),
        );
    };
    let text = reply_text(raw, id)?;
    let forum = state.settings.forum.as_ref().ok_or_else(unavailable)?;
    let replied = tokio::time::timeout(ATTEMPT, async {
        let Some(topic) = forum.topic_of(&state.foundation, post).await? else {
            return Ok(false);
        };
        forum.reply(&state.foundation, topic, &text).await?;
        Ok::<_, Failure>(true)
    })
    .await
    .unwrap_or(Err(Failure::Unreachable));
    match replied {
        Ok(true) => Ok(()),
        Ok(false) => Err(ApiError::conflict("The collection forum post has no topic")
            .with_data(json!({ "id": id, "forumId": post }))),
        Err(_) => {
            tracing::warn!(collection = %id, member = grant.member(), "a curation reply did not reach the forum");
            Err(not_replied(id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_one_to_ten_thousand_utf16_units_and_mentions_nobody() {
        let id = Uuid::nil();
        assert!(reply_text(None, id).is_err());
        assert!(reply_text(Some(""), id).is_err());
        assert_eq!(
            reply_text(Some(&"a".repeat(MAX_REPLY)), id).unwrap().len(),
            MAX_REPLY
        );
        assert!(reply_text(Some(&"a".repeat(MAX_REPLY + 1)), id).is_err());
        assert!(reply_text(Some(&"😀".repeat(MAX_REPLY / 2 + 1)), id).is_err());
        assert_eq!(
            reply_text(Some("The collection has been assigned to @admins"), id).unwrap(),
            "The collection has been assigned to @\u{200b}admins"
        );
        let assigned = "The collection has been assigned to <a target=\"_blank\" href=\"https://market.example.org/accounts/0x01eba3f0e992f53debaf51ed3738dc2f23e09aee\">Name</a>";
        assert_eq!(reply_text(Some(assigned), id).unwrap(), assigned);
    }
}
