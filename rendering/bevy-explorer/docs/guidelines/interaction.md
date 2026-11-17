# Interaction

How the pointer and the scene raycasts decide what a scene can reach, and how a test drives them.
Paths are relative to `source/`; the pointer code is in `crates/scene_runner/src/update_scene/`.

## Distances

| What | Value | Where |
| --- | --- | --- |
| Default reach of a pointer entry | 10 m from the player, when the entry sets neither `max_distance` nor `max_camera_distance` | `pointer_results.rs`, `passes_distance_check` |
| `max_player_distance` | A deprecated alias of `max_distance`; when both are set the larger wins | `pointer_results.rs`, `max_player_distance` |
| `max_camera_distance` | Compared with the distance from the camera to the hit point. With `max_distance` set too, either one passing is enough (OR) | `pointer_results.rs`, `passes_distance_check` |
| Proximity reach | 3 m from the player, for a PROXIMITY entry that sets no distance | `PROXIMITY_DEFAULT_MAX_DISTANCE` |
| Proximity cone | 120 degrees wide, horizontal, around the body's forward direction | `PROXIMITY_FOV_ANGLE_DEGREES` |
| Scenes considered | The scenes within 16 m of the player (`get_area(player, PARCEL_SIZE)`) | `pointer_results.rs`, `update_pointer_target` |
| Cursor ray | Unbounded: it ends at the first hit in the scenes considered, and the entry's distances decide whether that hit is in reach | `pointer_results.rs`, `update_pointer_target` |

## Scene raycasts

Scene raycast requests run in two tiers each frame (`update_scene/raycast_budget.rs`,
`raycast_result.rs`).

- **Uncapped.** Requests of the system scene (`SuperUserScene`), every portable and the scenes the
  player stands in (`get_area(player, PLAYER_COLLIDER_RADIUS)`) all run every frame they are due.
  Scenes drive movement from their results (`movement-scene` casts continuous `RS_AVATAR` rays for
  ground and walls), so these never wait. They cast together, on the compute pool once eight or
  more are due (see [scheduling](scheduling.md)).
- **Capped.** Requests of every other scene run nearest scene first (`RendererSceneContext::priority`),
  then the one that ran longest ago (never-run first), then entity id, while the time spent is
  under the budget. A cast that starts under budget finishes, so the last one may cross the line.
  The rest wait for a later frame. When the player walks into a capped scene it becomes uncapped,
  so its requests run then.
- **Budget.** A tenth of the target frame: `SceneUpdates::frame_target`, which `run_scene_loop`
  sets from the vsync refresh rate or the fps target (with the session override). That is 1.67 ms
  at 60 fps, 0.69 ms at 144 and 0.42 ms at 240. It follows the target, not the measured frame, so a
  slow frame does not raise the next one's budget. Raycasts run before the scene loop, so they use
  the previous frame's target. With an uncapped frame rate (fps 0) it is a tenth of a 60 fps frame.
- **`/raycast_budget [ms|auto]`.** Without an argument it answers
  `raycast budget: <ms> ms (auto|override)`. A value from 0 to 10 overrides the capped budget for
  the session (0 defers every capped request); `auto` returns it to the frame share. Anything else
  fails with ``raycast budget must be `auto` or between 0 and 10 ms``. It is not a setting.
- **`/raycast_stats`.** JSON: `budgetMs`, and for the capped tier's last frame with a request to
  cast `ran`, `deferred` and `spentMs`; for the uncapped tier's last such frame `uncappedRan` and
  `uncappedSpentMs`. A frame with nothing due in a tier (most frames between scene ticks) leaves
  that tier's values as they were.
- **Debug panel.** The "Raycasts" section shows those values, has a number input that overrides
  the capped budget (0 to 10) and an "Automatic budget" action that sends `/raycast_budget auto`.

### Unity reference

unity-explorer casts one 100 m camera ray per frame, caps scene raycasts at 3 ms, runs them for the
current scene only, and defers the ones that do not fit. Ours differ on purpose: see
[Decisions](decisions.md).

## Driving the pointer

`/hover_entity` and `/click_entity` (`src/automation/pointer.rs`, `src/automation/pointer/machine.rs`)
hover and click a scene entity of the active scene, by id or by a world point, through the
production pointer pipeline, on native and web alike. The scene is the one `/set_scene` picked,
else the player's. A command sets a synthetic aim (`SyntheticPointer`, `synthetic_pointer.rs`):
the pointer ray goes from the camera through the entity's anchor point, or through the world
point, whatever the cursor does, and runs through the same casts, distance checks and result
writers as a real cursor. A click presses the key or mouse
button bound to the action through the input script, so the press reaches the scene as a
person's would.

On the web these commands reach the engine through `engine_console_command` and write Bevy
input events. Unlike `/press_key` and `/click_at`, they do not go through the page's trusted
input (`window.__dclTrustedInput`), so they need no DevTools client.

### Options

`/hover_entity [<entity>] [--at x y z] [--bounds] [--frames N | --seconds S] [--timeout-frames N |
--timeout-seconds S]` and `/hover_entity --clear`:

| Option | Meaning | Default |
| --- | --- | --- |
| `<entity>` | An id as `/scene_entities` lists it, or `root`, `player`, `camera` | an entity or `--at`, unless `--clear` |
| `--at x y z` | Aim through this world point, in Decentraland world coordinates (as `/world_state` reports the position; z runs north): the job takes what the ray reaches there (see [Aiming at a point](#aiming-at-a-point)). With `<entity>` too, that entity must be what is there | the entity's anchor |
| `--bounds` | Aim at the centre of the bounds of the visible meshes of the entity and its descendants; the origin when it has none. Ignored with `--at` | the entity's origin |
| `--frames`, `--seconds` | How long to hover; seconds count in frames at the measured frame rate; at most 1800 frames (more is refused) | 30 frames |
| `--timeout-frames`, `--timeout-seconds` | How long the job may take in all. A hover's deadline is `max(timeout, frames + 3)`, so the default never cuts a long hover short | 600 frames |
| `--clear` | End any aim and release a held press, instead of hovering | off |

`/click_entity [<entity>] [--at x y z] [--bounds] [--button pointer|primary|secondary] [--event
click|down|up] [--timeout-frames N | --timeout-seconds S]`:

| Option | Meaning | Default |
| --- | --- | --- |
| `--button` | The action to press, by its binding | `pointer` |
| `--event` | `click` presses, then releases on a later scene tick; `down` presses and holds; `up` releases a held press (by the held entity's id, or by `--at` alone) | `click` |
| `--timeout-frames`, `--timeout-seconds` | How long the click may take in all | 600 frames |

### Aiming at a point

With `--at`, the ray goes from the camera through the point and the job takes the first collider
it reaches, as a cursor over that point would. That hit is *at the point* when it is within 1 m
of it, or when the point lies inside the hit entity's visible bounds (a box aimed at its centre).
A hit nearer the camera than that stands in front of the point: the job ends with the `blocked by`
diagnosis and presses nothing. Nothing at the point (no hit, or a hit beyond it, so the point is in
empty air) refuses the command without pressing anything, after two frames of looking:
`{"refused": "nothing at the point: ...", "cause": {"crdtEntityId", "hitPoint", "rootBroadcast":
false}}`. A press is never sent without a hit, so a miss never reaches the scene root.

### The reply

```json
{"hit": {"entity": 512, "container": 512, "scene": "pointer-targets", "playerDistance": 4.0, "cameraDistance": 6.2},
 "crdtEntityId": 512, "hitPoint": {"x": 8.0, "y": 1.0, "z": 7.5}, "distance": 4.0, "hoverText": "Near",
 "blockedByUi": false, "rootBroadcast": false, "button": "pointer",
 "entries": [{"button": "IaPointer", "event": "PetDown", "hoverText": "Near", "enabled": true, "tooFar": false}],
 "events": [{"event": "PetDown", "button": "IaPointer", "tick": 31}],
 "diagnosis": null}
```

- `hit`: what the pointer pipeline targeted, or `null`. `entity` is the hit entity and
  `container` the entity that owns the collider. A hit that is no entity of the scene (an avatar,
  another scene) reports id 4294967295 for both. `scene` is the hit scene's title.
  `playerDistance` is from the avatar to the nearest point of the collider, `cameraDistance`
  from the camera along the ray.
- `crdtEntityId`, `hitPoint`, `distance`: the hit entity's id (`null` without a hit, or for an
  avatar or another scene), where the ray hit it in Decentraland world coordinates, and
  `playerDistance` again, at the top level.
- `hoverText`: the hover text of the hit's entries, an enabled entry's first.
- `blockedByUi`: scene UI covering the aim. Not checked yet: always `false`.
- `rootBroadcast`: whether the scene root received a pointer result while the job ran, which a
  press reaches when no entity takes it (the hit entity declares no entry for the button and
  event). Read from the results written, not assumed. A press the root received has landed: the
  click goes on to its release on a later tick, and the `warning` names the missing entry.
- `button` (`/click_entity`): the action pressed, as `--button` names it.
- `warning` (`/click_entity`, only when there is one): the hit entity declares no cursor entry for
  the button and an event the click sends, one part per edge, e.g. `press: the entity declares no
  PointerEvents entry for PetDown/IaPointer; it listens for: PetDown/IaPrimary`. The press still
  reaches the entity's results; no handler bound to a declared entry runs on it.
- `entries`: the cursor pointer-event entries of the hit container, each with `button`, `event`,
  `hoverText`, `enabled` and `tooFar` (the distance check failed).
- `events`: the pointer events the scene received for the entity or the hit entity, each with
  `event`, `button` and the scene `tick` it was written for.
- `diagnosis`: `null` when the entity was hovered or clicked, otherwise one of the strings below.

### Diagnoses

A diagnosis is a resolved reply with `diagnosis` set, not a failure: read it from the JSON.

| `diagnosis` | Meaning |
| --- | --- |
| `blocked by entity <id>` | The ray hit another entity's collider first, or, with `--at`, one in front of the point; `<id>` is that container |
| `blocked by an avatar or another scene's entity` | The same, for a collider that is no entity of the scene |
| `out of reach: the entity's entry for the button is past its distance limit` | `/click_entity`: the hit entity declares an entry for the button, and none is in reach. Nothing is pressed: the press would go past the entity to the scene root |
| `out of reach: the entity's hover entries are past their distance limit` | `/hover_entity`: the hit entity declares a `PetHoverEnter` entry, and none is in reach. The aim ends at once and nothing is entered |
| `entity not live` | The scene has no live entity for the id, or it was replaced while the job ran |
| `input held by the HUD` | The HUD holds the bound input, so the press would not reach the scene |
| `timed out waiting for <stage>` | The job passed its deadline; `<stage>` is `target` (the ray hit nothing before the deadline: no collider, or one still loading), `hover` (a hover did not finish), `tick` (neither the entity nor the scene root got the press) or `release` (the release was never played) |

### Refusals

A refusal fails the command (the web call rejects, the loopback answers `ok: false`):

- a pointer command is still running, or its release has not been played yet: wait for its reply
  or send `/hover_entity --clear`;
- a press is held by an earlier `--event down`: send `/click_entity <id> --event up` or
  `/hover_entity --clear`; an `up` with nothing held fails with `no press is held`;
- `--frames` over 1800, an unknown `--event` or `--button`, a button with no binding, or a
  button bound to a gamepad input;
- neither an entity, `--at` nor `--clear`, or an id that does not parse;
- `--at`: nothing at the point (above), or, with an entity too, another entity of the scene at
  the point: `the point holds entity <hit>, not entity <id>`. These two are JSON
  (`{"refused", "cause"}`) and nothing was pressed.

### The open press

`--event down` presses and keeps the aim open on the entity (no expiry) once the scene has logged
the press. The press stays held until `--event up` on the same entity (or with `--at` alone) or
`/hover_entity --clear`;
other pointer commands are refused meanwhile. A hover or a click ends its aim when it replies.
When a press is still held at the end (`--clear`, or a click that failed after pressing), the
release is played on the aimed entity first: the reply, and the end of the aim, wait until it has
been played and the pointer pipeline has run after it, and other pointer commands are refused
until then.

### A parked UI hover

`/ui_hover <crdtId>` keeps an open aim on a scene-UI element across commands. A pointer command
sent meanwhile (`/hover_entity`, `/click_entity`, `/press_input`, `/sweep_pointer`, and the `/ui_`
gestures) waits until that hover has ended: the aim is dropped, the element's leave is sent to
the scene and drained (two scene ticks), then the command runs and its reply carries
`hoverEnded` (`sdk crdtId N`). `/click_at` is real input: while a hover is parked the pointer
ray follows the aim, so end it with `/ui_hover --leave` before clicking.

### Pressing an input action

`/press_input <action> [--hold S] [--entity <id>] [--at x y z] [--timeout-frames N |
--timeout-seconds S]` (`src/automation/pointer/press.rs`) presses a scene input action by its SDK
name, through the key or mouse button bound to it, as a pointer job: it shares the click's
machine, refusals, diagnoses and reply.

| Option | Meaning | Default |
| --- | --- | --- |
| `<action>` | Lowercase: `pointer`, `primary`, `secondary`, `jump`, `forward`, `backward`, `right`, `left`, `action_3` to `action_6`, `walk`, `modifier`. Any other name, `PRIMARY` included, is refused with the list | required |
| `--hold` | Seconds to keep the press down, counted in frames at the measured frame rate (at most 1800 frames); the release still waits for a later scene tick than the press | a click |
| `--entity`, `--at` | Aim as `/click_entity` aims (by id, through a world point, or both). Nothing at the point refuses without pressing; an entry out of reach is the `out of reach` diagnosis | no aim |
| `--timeout-frames`, `--timeout-seconds` | How long the press may take to land and release, on top of the hold | 600 frames |

Without `--entity` or `--at` the press has no aim: the pointer ray stays the cursor's (the screen
centre while the cursor is locked) and the press lands as a person's would. An entity under the
ray that takes the action in reach gets it (or one whose proximity entry wins the press); otherwise the pipeline writes it to the scene root's
`PointerEventsResult` (entity 0, no hit) of every scene the player is in. An entity that takes the
press suppresses the root write for its scene that tick. This holds for every action, movement
and jump included: an unaimed `forward` over an entity with an `IaForward` entry is entity-bound,
and the key still moves the avatar. An unaimed press is refused while an aim is active. The
pipeline logs its results while the press runs (`PointerEventLog::unaimed`), so the reply says
where it went.

The reply is the [click's](#the-reply), without `button`, plus:

```json
{"action": "primary", "entityBound": false, "aimed": false, "holdSeconds": 0.0, "holdFrames": 0,
 "rootEvents": [{"event": "PetDown", "button": "IaPrimary", "tick": 412}, {"event": "PetUp", "button": "IaPrimary", "tick": 413}],
 "hint": "no entity of the scene was under the ray, so the scene root got the press (rootEvents); ..."}
```

- `entityBound`: an entity took the press: its `PetDown` for the action is in `events`. False when
  the root got it, or nothing did (then `diagnosis` says why).
- `aimed`: the press was aimed (`--entity`, `--at`).
- `holdSeconds`, `holdFrames`: the hold asked for, and the frames it was counted in.
- `rootEvents`: the results the scene root got while the press ran, with their ticks;
  `rootBroadcast` is whether there are any.
- `hint` (only when the root got the press): why, naming the entity under the ray when there was
  one. An aimed press whose entity declares no entry for the action also carries the click's
  `warning`; an unaimed one that another entity took than the one under the ray (its proximity
  entry won) carries a `warning` naming both, and `events` holds that entity's results.

