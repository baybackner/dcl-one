# ADR: A stream key ends when its minter loses the place, and at a redeploy

- Status: Accepted
- Date: 2026-11-20

## Context

A scene stream key (`social.scene_stream_access`) admits an RTMP ingress, Cast streamers and a presentation bot to one scene room. The key recorded the wallet that generated it (`generated_by`), and nothing read that column again. A key worked until its expiry after its minter stopped being an admin, got a platform ban, lost a world permission or was no longer the owner. After a redeploy the scene has a new entity and a new room; the old key still published into the old room, where no scene runs.

## Decision

A key lives only while the wallet that generated it would still get it, for the scene that stands on the place of the key's room.

- The check uses the gates of the stream link: no platform ban, not on the deny list, and the key decision (owner or admin) on the scene deployed on the base parcel of the room's scene. For a world scene admin, the decision includes the wallet that gave the grant. One function decides the ban and the deny list for the routes, for the minter of a key and for a watcher that joins.
- A Genesis scene that this node stores without an active deployment does not stand, so its key ends. A scene that this node does not store keeps its key.
- A key with no recorded minter (an imported one) is kept until its scene is replaced or the key expires.
- The check runs on each publisher token and each stream link, before `GET /scene-stream-access` answers, after `DELETE /scene-admin`, after a platform ban or an operator block, on a database notification ([stream key notifications](2026-11-20-stream-key-notifications.md)) and in a recheck job of its own (`comms-streaming-key-recheck`), apart from the job that expires keys.
- With `SOCIAL_WRITES_ENABLED` off (the default) the social gate refuses the publisher tokens, `DELETE /scene-admin` and the ban routes, so their checks do not run, and `GET /scene-stream-access` revokes nothing: for a key that fails the check it answers as a place with no key. The stream link, the notification listener and the jobs do not read the switch.
- A key that fails the check is revoked like any other: one conditional update of the row, then the cleanup deletes the ingress, removes the publishers and tells the owner. The cleanup runs in a task of its own, so no request waits for LiveKit; a cleanup job tries a failed one again. The join guard refuses a publisher of a revoked key.
- The removal of a place ends its key in the removal's own transaction and sends no key notice.
- A source that cannot be read keeps the key and refuses the token. A refusal ends a key only when its decision had a source for the owner of the place. A LAND index that is not ready, or whose snapshot does not open, on a node with no marketplace reader, and a NAME world on a node with no names reader, refuse every owner. The ownership read says that it had no source, in the same read that refuses, so the check takes no second sample of the source. That refusal is no evidence that the minter lost the place: the key stays and its tokens answer 503.
- One record of the node still ends such a key: an admin grant of the minter (`social.scene_admins`) that was in force when the key was made and was removed after that. A key records its minter and its time, not the authority it was made under. A removed admin who also owns the place loses the key while no source names the owner, and gets a new one with the next stream link when a source does. A world permission that is removed leaves no row, so the key of that grantee stays, with no token, until a source names the owner or the key expires.

## Alternatives

The pinned comms-gatekeeper never reads `generated_by`: a key lives until its expiry, and a removed admin keeps the stream. It keeps the key across a redeploy until the next stream link, so the old key publishes into a room with no scene.

## Consequences

This node is stricter than the pinned comms-gatekeeper on both rules. A streamer whose scene is deployed again asks for a new stream link; the next link is a key for the new room. An owner or admin that loses the place loses the stream at the next check, and the owner of the place gets the revocation notice. The routes, the key format and the room names do not change.
