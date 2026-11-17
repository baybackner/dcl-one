//! `/hover_entity` and `/click_entity`: hover and click a scene entity, by id or by a world point
//! (`--at x y z`), through the production pointer pipeline, on native and web alike. A command
//! sets a synthetic aim (`SyntheticPointer`) at the entity or the point, so the pointer ray goes
//! from the camera through it whatever the cursor does; a click presses the bound key or mouse
//! button through the input script, so the press reaches the scene exactly as a person's would.
//! `drive_pointer_job` follows the job frame by frame with [`ClickMachine`] and answers with what
//! the pipeline did:
//!
//! `{"hit": {"entity", "container", "scene", "playerDistance", "cameraDistance"} | null,
//! "crdtEntityId", "hitPoint": {"x", "y", "z"}, "distance", "hoverText", "blockedByUi",
//! "rootBroadcast", "button", "entries": [HoverEntryReport], "events": [{"event", "button",
//! "tick"}], "diagnosis": string | null, "warning"?}`
//!
//! A diagnosis (the entity is blocked, gone, the HUD holds the input, a stage timed out) still
//! answers with that JSON; a refusal (a press is already held, a gamepad binding, nothing to
//! release, nothing at the aimed point) fails the command, and one decided by the pipeline is
//! `{"refused", "cause"}` (docs/adr/2026-11-17-console-command-conventions.md).

use std::collections::HashSet;

use bevy::{
    diagnostic::FrameCount,
    ecs::system::SystemParam,
    input::{
        keyboard::{Key, NativeKey},
        ButtonState,
    },
    prelude::*,
    render::primitives::Aabb,
};
use bevy_console::ConsoleCommand;
use common::{
    inputs::{CommonInputAction, InputIdentifier, InputMap},
    rpc::{RpcResultReceiver, RpcResultSender},
    sets::SceneSets,
    structs::PrimaryCamera,
};
use console::{
    reply::{self, FrameRate, Span},
    DoAddConsoleCommand, PendingConsoleResponses,
};
use dcl_component::{
    proto_components::sdk::components::common::PointerEventType,
    transform_and_parent::DclTranslation, SceneEntityId,
};
use input_manager::{InputManager, InputPriority};
use scene_inspector::{parse_entity_id, SceneResolver};
use scene_runner::{
    renderer_context::RendererSceneContext,
    update_scene::{
        pointer_results::{PointerTarget, PointerTargetInfo},
        synthetic_pointer::{
            anchor_point, hover_entries, world_bounds, AimAt, Anchor, Hold, HoverEntryReport,
            PointerEventLog, SyntheticAim, SyntheticPointer,
        },
    },
    update_world::pointer_events::PointerEvents,
    ContainerEntity, SceneEntity,
};
use serde::Serialize;

use super::input::{key_from_code, InputScript, Step};

/// Frames a hover lasts without `--frames`.
const DEFAULT_HOVER_FRAMES: u32 = 30;
/// Frames a job may take without `--timeout-frames` or `--timeout-seconds`.
const DEFAULT_TIMEOUT_FRAMES: u32 = 600;
/// How far outside a hit's visible bounds an aimed point may be and still be inside them.
const BOUNDS_MARGIN: f32 = 0.05;

mod machine;
mod press;
pub(crate) mod ui;
pub use machine::{
    binding_error, check_hover_frames, classify_point, press_out_of_reach, refuse_while_held,
    undeclared_warning, ClickKind, ClickMachine, Effect, Observed, TargetSeen,
};
use machine::{ENTITY_NOT_LIVE, FOREIGN_HIT, HUD_HOLDS_INPUT};

pub(crate) struct AutomationPointerPlugin;

impl Plugin for AutomationPointerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PointerJob>()
            .add_console_command::<HoverEntityCommand, _>(hover_entity_cmd)
            .add_console_command::<ClickEntityCommand, _>(click_entity_cmd)
            .add_console_command::<press::PressInputCommand, _>(press::press_input_cmd)
            .add_console_command::<ui::UiClickCommand, _>(ui::ui_click_cmd)
            .init_resource::<ui::UiClickJob>()
            .add_systems(
                Update,
                (settle_release, drive_pointer_job, ui::drive_ui_click)
                    .chain()
                    .after(SceneSets::Input),
            );
    }
}

/// The input script step pressing or releasing `binding`; `None` for a gamepad binding.
pub(crate) fn binding_step(binding: &InputIdentifier, state: ButtonState) -> Option<Step> {
    match *binding {
        InputIdentifier::Key(key_code) => {
            // bevy names key codes as the DOM does, so the press carries the key and text a
            // keyboard would; a key without a DOM name carries none
            let (logical_key, text) = key_from_code(&format!("{key_code:?}"))
                .filter(|(code, ..)| *code == key_code)
                .map(|(_, key, text)| (key, text))
                .unwrap_or((Key::Unidentified(NativeKey::Unidentified), None));
            Some(Step::Key {
                key_code,
                logical_key,
                text,
                state,
            })
        }
        InputIdentifier::Mouse(button) => Some(Step::Button(button, state)),
        InputIdentifier::Gamepad(_) | InputIdentifier::Analog(..) => None,
    }
}

// --- the reply ---

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HitReport {
    entity: u32,
    container: u32,
    scene: String,
    player_distance: f32,
    camera_distance: f32,
    /// where the ray hit, reported at the top level as `hitPoint`
    #[serde(skip)]
    point: Option<Vec3>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EventReport {
    event: String,
    button: String,
    tick: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Point {
    x: f64,
    y: f64,
    z: f64,
}

impl Point {
    /// A bevy world point in Decentraland world coordinates, as `--at` and `/world_state` use them.
    pub(crate) fn from_bevy(v: Vec3) -> Self {
        let [x, y, z] = DclTranslation::from_bevy_translation(v).0;
        Self::from(Vec3::new(x, y, z))
    }
}

impl From<Vec3> for Point {
    fn from(v: Vec3) -> Self {
        Self {
            x: reply::round(v.x),
            y: reply::round(v.y),
            z: reply::round(v.z),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Reply<'a> {
    hit: Option<&'a HitReport>,
    /// the hit entity, `null` without a hit or for an avatar or another scene's entity
    crdt_entity_id: Option<u32>,
    hit_point: Option<Point>,
    /// from the player to the hit
    distance: Option<f64>,
    hover_text: Option<&'a str>,
    /// scene UI covering the aim: not checked yet, so always false
    blocked_by_ui: bool,
    /// whether the scene root got a pointer result while the job ran (no entity took the press)
    root_broadcast: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    button: Option<&'a str>,
    entries: &'a [HoverEntryReport],
    events: &'a [EventReport],
    diagnosis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    #[serde(flatten)]
    press: Option<PressReport<'a>>,
}

/// What a `/press_input` reply adds: the action, whether an entity took the press, the hold and
/// the events the scene root got.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PressReport<'a> {
    action: &'a str,
    /// an entity's entry took the press (its `PetDown` is in `events`); false when the scene root
    /// got it instead, or nothing did
    entity_bound: bool,
    /// whether the job aimed (`--entity`, `--at`) or pressed wherever the cursor was
    aimed: bool,
    hold_seconds: f32,
    hold_frames: u32,
    root_events: &'a [EventReport],
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

/// What a reply says beyond the hit, the entries, the events and the diagnosis.
#[derive(Default)]
struct ReplyExtra<'a> {
    button: Option<&'a str>,
    root_broadcast: bool,
    warning: Option<String>,
    press: Option<PressReport<'a>>,
}

fn reply_json(
    hit: Option<&HitReport>,
    entries: &[HoverEntryReport],
    events: &[EventReport],
    diagnosis: Option<String>,
    extra: ReplyExtra,
) -> String {
    let hover_text = entries
        .iter()
        .filter(|entry| entry.hover_text.is_some())
        .max_by_key(|entry| entry.enabled)
        .and_then(|entry| entry.hover_text.as_deref());
    serde_json::to_string(&Reply {
        hit,
        crdt_entity_id: hit.map(|hit| hit.entity).filter(|&id| id != FOREIGN_HIT),
        hit_point: hit.and_then(|hit| hit.point).map(Point::from_bevy),
        distance: hit.map(|hit| reply::round(hit.player_distance)),
        hover_text,
        blocked_by_ui: false,
        root_broadcast: extra.root_broadcast,
        button: extra.button,
        entries,
        events,
        diagnosis,
        warning: extra.warning,
        press: extra.press,
    })
    .unwrap_or_default()
}

/// A refusal decided by the pipeline: nothing was pressed, and the cause says what the ray found.
fn refusal_json(reason: String, hit: Option<&HitReport>, root_broadcast: bool) -> String {
    reply::refusal(
        reason,
        Some(serde_json::json!({
            "crdtEntityId": hit.map(|hit| hit.entity).filter(|&id| id != FOREIGN_HIT),
            "hitPoint": hit.and_then(|hit| hit.point).map(Point::from_bevy),
            "rootBroadcast": root_broadcast,
        })),
    )
}

// --- the job ---

/// The running job, the press a finished `--event down` left held, and a release still to land
/// before the aim ends.
#[derive(Resource, Default)]
pub struct PointerJob {
    pub job: Option<Job>,
    held: Option<Held>,
    releasing: Option<Releasing>,
}

impl PointerJob {
    /// The refusal while a job runs or its release has not landed yet.
    fn busy(&self) -> Option<String> {
        let what = self.job.as_ref().map(|job| job.what.clone()).or(self
            .releasing
            .as_ref()
            .map(|releasing| releasing.what.clone()))?;
        Some(format!(
            "a pointer command is still running on {what}; wait for its reply or send /hover_entity --clear"
        ))
    }
}

type PendingAnswer = (
    RpcResultSender<Result<String, String>>,
    Result<String, String>,
);

/// A queued release: the aim stays on its entity until the release has been played and the
/// pointer pipeline has run after it, then the aim ends and the replies go out.
struct Releasing {
    /// what the refusal names: `entity 512`, `the point (x, y, z)`
    what: String,
    /// the aim that ends; `None` ends any aim
    target: Option<AimAt>,
    /// the input script step number of the release
    step: u64,
    replies: Vec<PendingAnswer>,
}

/// A press left held by `--event down`, with the aim kept open on its entity or point.
struct Held {
    scene: Entity,
    /// the entity asked for, `None` for a point aim naming none
    entity: Option<SceneEntityId>,
    /// the proto id the press landed on: the one asked for, or the hit at the point
    pressed: u32,
    target: Option<Entity>,
    at: AimAt,
    binding: InputIdentifier,
}

/// What a `/press_input` job reports beyond a click's reply.
#[derive(Debug, Clone, Copy)]
struct PressInput {
    hold_seconds: f32,
    hold_frames: u32,
}

/// The pressed button as the command names it and as the scene's entries name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ButtonNames {
    arg: &'static str,
    action: &'static str,
}

pub struct Job {
    machine: ClickMachine,
    reply: RpcResultSender<Result<String, String>>,
    /// the pressed binding; `None` for a hover
    binding: Option<InputIdentifier>,
    button: Option<ButtonNames>,
    scene: Entity,
    /// the entity asked for, `None` for a point aim naming none
    entity: Option<SceneEntityId>,
    /// its bevy entity, while the scene maps it
    target: Option<Entity>,
    /// the aim; `None` for a press with no aim, which follows the cursor
    at: Option<AimAt>,
    /// what a refusal names: `entity 512`, `the point (x, y, z)`
    what: String,
    started: u32,
    /// the scene root got a pointer result while the job ran
    root_broadcast: bool,
    /// the events the scene root got while the job ran
    root_events: Vec<EventReport>,
    /// a `/press_input`: its action and hold, for the reply
    press: Option<PressInput>,
    // what the frames since the start saw, so an aim that expires loses nothing
    hit: Option<HitReport>,
    entries: Vec<HoverEntryReport>,
    events: Vec<EventReport>,
    seen_events: HashSet<(i32, i32, u32)>,
    /// the hit entity of the latest target, whose events count with the asked entity's
    hit_entity: Option<u32>,
    /// the entity whose `PetDown` the job saw first
    took: Option<u32>,
    /// the input script step number of the release the machine asked for
    release_step: Option<u64>,
}

impl Job {
    fn requested(&self) -> Option<u32> {
        self.entity.map(|id| id.as_proto_u32().unwrap_or_default())
    }

    fn release_binding(&self) -> Option<InputIdentifier> {
        self.binding.filter(|_| self.machine.holds_press())
    }
}

/// Ends the aim `target` (any aim for `None`) and sends `answer`: at once, or, with a press to
/// release, once the release has been played and the pointer pipeline has run after it
/// ([`settle_release`]), so the release lands on the aimed entity and no new command takes the aim
/// before it does.
#[allow(clippy::too_many_arguments)]
fn wind_down(
    jobs: &mut PointerJob,
    synthetic: &mut SyntheticPointer,
    log: &mut PointerEventLog,
    script: &mut InputScript,
    release: Option<InputIdentifier>,
    what: String,
    target: Option<AimAt>,
    answer: PendingAnswer,
) {
    let step = release
        .and_then(|binding| binding_step(&binding, ButtonState::Released))
        .map(|step| script.push(step));
    match (step, jobs.releasing.as_mut()) {
        (step, Some(releasing)) => {
            // a release is already pending: this answer waits for it too
            if let Some(step) = step {
                releasing.step = releasing.step.max(step);
            }
            releasing.replies.push(answer);
        }
        (Some(step), None) => {
            jobs.releasing = Some(Releasing {
                what,
                target,
                step,
                replies: vec![answer],
            });
        }
        (None, None) => {
            end_aim(synthetic, log, target);
            answer.0.send(answer.1);
        }
    }
}

/// Ends the aim if it is `target` (any aim for `None`), and clears the log with it.
fn end_aim(synthetic: &mut SyntheticPointer, log: &mut PointerEventLog, target: Option<AimAt>) {
    let ours = synthetic
        .0
        .as_ref()
        .is_some_and(|aim| target.is_none_or(|target| aim.at == target));
    if ours {
        synthetic.0 = None;
        log.events.clear();
    }
    if log.unaimed {
        log.unaimed = false;
        log.events.clear();
    }
}

/// Ends the job, the held press and the aim, answering `reply` once any release has landed.
fn clear_pointer(
    jobs: &mut PointerJob,
    synthetic: &mut SyntheticPointer,
    log: &mut PointerEventLog,
    script: &mut InputScript,
    reply: RpcResultSender<Result<String, String>>,
) {
    let (release, what) = if let Some(job) = jobs.job.take() {
        let release = job.release_binding();
        job.reply
            .send(Err("cleared by /hover_entity --clear".to_owned()));
        (release, job.what)
    } else {
        let held = jobs.held.take();
        let what = held
            .as_ref()
            .map(|held| format!("entity {}", held.pressed))
            .unwrap_or_default();
        (held.map(|held| held.binding), what)
    };
    jobs.held = None;
    wind_down(
        jobs,
        synthetic,
        log,
        script,
        release,
        what,
        None,
        (reply, Ok("cleared".to_owned())),
    );
}

/// Once the queued release has been played (in this frame's `PreUpdate`, so this `Update`'s
/// pointer pipeline has already run after it), ends the aim and sends the replies that waited.
fn settle_release(
    mut jobs: ResMut<PointerJob>,
    script: Res<InputScript>,
    mut synthetic: ResMut<SyntheticPointer>,
    mut log: ResMut<PointerEventLog>,
) {
    let landed = jobs
        .releasing
        .as_ref()
        .is_some_and(|releasing| script.played() >= releasing.step);
    let Some(releasing) = jobs.releasing.take_if(|_| landed) else {
        return;
    };
    end_aim(&mut synthetic, &mut log, releasing.target);
    for (reply, answer) in releasing.replies {
        reply.send(answer);
    }
}

// --- the commands ---

/// Hover a scene entity of the active scene through the pointer pipeline, by id or by a world
/// point (--at x y z: whatever the ray from the camera reaches there; with an id too, that entity
/// must be at the point), for --frames frames or --seconds seconds (30 frames by default), and
/// answer, as JSON, with the hit, the hover entries, the pointer events the scene got and a
/// diagnosis when the entity was not hovered. --frames is at most 1800, and a hover may always take
/// its frames plus 3 whatever its timeout says. Nothing at the point refuses. --clear ends any aim
/// and releases a held press
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/hover_entity")]
pub struct HoverEntityCommand {
    /// Entity id (u32, as /scene_entities lists it) or alias: root, player, camera
    entity: Option<String>,
    /// Aim at this world point instead of the entity's anchor
    #[arg(long, num_args = 3, value_names = ["x", "y", "z"], allow_negative_numbers = true)]
    at: Option<Vec<f32>>,
    /// Aim at the centre of the bounds of the visible meshes of the entity and its descendants,
    /// rather than at its origin (the origin when it has none)
    #[arg(long)]
    bounds: bool,
    /// How long to hover: --frames (at most 1800) or --seconds
    #[command(flatten)]
    span: Span,
    /// Frames the hover may take in all (600 by default); never fewer than its frames plus 3
    #[arg(long, conflicts_with = "timeout_seconds")]
    timeout_frames: Option<u32>,
    /// Seconds the hover may take in all, instead of --timeout-frames
    #[arg(long)]
    timeout_seconds: Option<f32>,
    /// End any aim, releasing a held press, instead of hovering
    #[arg(long)]
    clear: bool,
}

/// Click a scene entity of the active scene through the pointer pipeline, by id or by a world
/// point (--at x y z: whatever the ray from the camera reaches there; with an id too, that entity
/// must be at the point), pressing the key or mouse button bound to the action, and answer, as
/// JSON, with the hit, the hover entries, the pointer events the scene got and a diagnosis when
/// the click did not land; a `warning` when the hit entity declares no entry for the button and
/// event. Nothing at the point refuses without pressing. `--event down` keeps the press held until
/// `--event up` or /hover_entity --clear
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/click_entity")]
pub struct ClickEntityCommand {
    /// Entity id (u32, as /scene_entities lists it) or alias: root, player, camera
    entity: Option<String>,
    /// Aim at this world point instead of the entity's anchor
    #[arg(long, num_args = 3, value_names = ["x", "y", "z"], allow_negative_numbers = true)]
    at: Option<Vec<f32>>,
    /// Aim at the centre of the bounds of the visible meshes of the entity and its descendants,
    /// rather than at its origin (the origin when it has none)
    #[arg(long)]
    bounds: bool,
    /// The action to press: pointer, primary or secondary
    #[arg(long, default_value = "pointer")]
    button: String,
    /// click (down then up), down or up
    #[arg(long, default_value = "click")]
    event: String,
    /// Frames the click may take in all (600 by default)
    #[arg(long, conflicts_with = "timeout_seconds")]
    timeout_frames: Option<u32>,
    /// Seconds the click may take in all, instead of --timeout-frames
    #[arg(long)]
    timeout_seconds: Option<f32>,
}

/// How a command answers.
enum Answer {
    Failed(String),
    Json(String),
    Pending(RpcResultReceiver<Result<String, String>>),
}

fn answer<T: bevy_console::Command>(
    input: &mut ConsoleCommand<T>,
    pending: &mut PendingConsoleResponses,
    answer: Answer,
) {
    match answer {
        Answer::Failed(message) => input.reply_failed(message),
        Answer::Json(json) => input.reply_ok(json),
        Answer::Pending(rx) => pending.push_receiver(rx, |r| r, input.take_responder()),
    }
}

fn diagnosed(diagnosis: &str) -> Answer {
    Answer::Json(reply_json(
        None,
        &[],
        &[],
        Some(diagnosis.to_owned()),
        ReplyExtra::default(),
    ))
}

/// The world point `--at` names, if it does, in Decentraland world coordinates (z north, as
/// `/world_state` reports the position); clap takes exactly three numbers.
fn at_point(at: &Option<Vec<f32>>) -> Result<Option<Vec3>, String> {
    match at.as_deref() {
        None => Ok(None),
        Some(&[x, y, z]) if x.is_finite() && y.is_finite() && z.is_finite() => {
            Ok(Some(Vec3::new(x, y, z)))
        }
        Some(other) => Err(format!(
            "--at takes three finite numbers x y z; got {other:?}"
        )),
    }
}

/// The frames a job may take: `--timeout-frames`, `--timeout-seconds` or the default.
fn timeout_frames(
    frames: Option<u32>,
    seconds: Option<f32>,
    rate: &FrameRate,
) -> Result<u32, String> {
    Span { frames, seconds }
        .frames(rate)
        .map(|frames| frames.unwrap_or(DEFAULT_TIMEOUT_FRAMES))
}

/// What both commands share: the scene, the aim, the job and the input script.
#[derive(SystemParam)]
pub(crate) struct PointerControl<'w, 's> {
    resolver: SceneResolver<'w, 's>,
    synthetic: ResMut<'w, SyntheticPointer>,
    log: ResMut<'w, PointerEventLog>,
    jobs: ResMut<'w, PointerJob>,
    script: ResMut<'w, InputScript>,
    frame: Res<'w, FrameCount>,
    rate: Res<'w, FrameRate>,
    pending: ResMut<'w, PendingConsoleResponses>,
    transforms: Query<'w, 's, &'static GlobalTransform>,
    meshes: Query<
        'w,
        's,
        (
            &'static Aabb,
            &'static GlobalTransform,
            &'static InheritedVisibility,
        ),
    >,
    children: Query<'w, 's, &'static Children>,
}

/// A job a command asks for.
struct Request<'a> {
    command: &'a str,
    entity: Option<&'a str>,
    at: Option<Vec3>,
    kind: ClickKind,
    bounds: bool,
    timeout: u32,
    /// the binding to press, `None` for a hover and an up (which releases the held one)
    binding: Option<InputIdentifier>,
    button: Option<ButtonNames>,
    /// the HUD holds the binding's input
    hud_holds: bool,
    /// press wherever the cursor is, with no aim (`/press_input` without `--entity` or `--at`)
    unaimed: bool,
    /// a `/press_input`: its hold
    press: Option<PressInput>,
    /// aim at the entity's scene-UI node, not into the world (`/ui_click`)
    ui: bool,
}

impl PointerControl<'_, '_> {
    fn start(&mut self, request: Request) -> Answer {
        let id = match request.entity.map(parse_entity_id).transpose() {
            Ok(id) => id,
            Err(e) => return Answer::Failed(e),
        };
        if id.is_none() && request.at.is_none() && !request.unaimed {
            return Answer::Failed("expected an entity id, or --at x y z".to_owned());
        }
        let requested = id.map(|id| id.as_proto_u32().unwrap_or_default());
        if let Some(refusal) = self.jobs.busy() {
            return Answer::Failed(refusal);
        }
        let held_on = self.jobs.held.as_ref().map(|held| held.pressed);
        if request.kind == ClickKind::Up {
            if held_on.is_none() {
                return Answer::Failed("no press is held".to_owned());
            }
            // an up names the held entity, or only a point
            if requested.is_some_and(|requested| held_on != Some(requested)) {
                return Answer::Failed(
                    refuse_while_held(held_on, request.command).unwrap_or_default(),
                );
            }
        } else if let Some(refusal) = refuse_while_held(held_on, request.command) {
            return Answer::Failed(refusal);
        }

        let frame = self.frame.0;
        let (scene, entity, target, at, binding) = if let Some(held) = self.jobs.held.take() {
            // an up releases the held press on the aim the down left open
            (
                held.scene,
                held.entity,
                held.target,
                Some(held.at),
                Some(held.binding),
            )
        } else if request.unaimed {
            // the ray stays the cursor's; only the log watches where the press lands
            if self.synthetic.0.is_some() {
                return Answer::Failed(
                    "an aim is active; wait for it to end or send /hover_entity --clear".to_owned(),
                );
            }
            let (scene, _) = match self.resolver.resolve() {
                Ok(resolved) => resolved,
                Err(e) => return Answer::Failed(e),
            };
            if request.hud_holds {
                return diagnosed(HUD_HOLDS_INPUT);
            }
            self.log.unaimed = true;
            (scene, None, None, None, request.binding)
        } else {
            let (scene, ctx) = match self.resolver.resolve() {
                Ok(resolved) => resolved,
                Err(e) => return Answer::Failed(e),
            };
            let target = match id {
                Some(id) => match ctx.bevy_entity(id) {
                    Some(target) => Some(target),
                    None => return diagnosed(ENTITY_NOT_LIVE),
                },
                None => None,
            };
            if request.hud_holds {
                return diagnosed(HUD_HOLDS_INPUT);
            }
            let at = match (request.at, id, target) {
                (Some(point), ..) => {
                    AimAt::Point(DclTranslation(point.to_array()).to_bevy_translation())
                }
                (None, Some(entity), Some(target)) if request.ui => AimAt::Ui { entity, target },
                (None, Some(entity), Some(target)) => {
                    let bounds = request.bounds
                        && anchor_point(
                            target,
                            Anchor::Bounds,
                            &self.transforms,
                            &self.meshes,
                            &self.children,
                        )
                        .is_some();
                    let anchor = if bounds {
                        Anchor::Bounds
                    } else {
                        Anchor::Origin
                    };
                    AimAt::Entity {
                        entity,
                        target,
                        anchor,
                    }
                }
                _ => unreachable!("an id without a live target returned above"),
            };
            let hold = match request.kind {
                ClickKind::Hover(frames) => Hold::Frames(frames.saturating_add(2)),
                _ => Hold::Open,
            };
            self.synthetic.0 = Some(SyntheticAim {
                scene,
                at,
                hold,
                started_frame: frame,
            });
            (scene, id, target, Some(at), request.binding)
        };
        self.log.events.clear();

        let what = match (entity, request.at) {
            (Some(entity), _) => format!("entity {}", entity.as_proto_u32().unwrap_or_default()),
            (None, Some(p)) => format!("the point ({}, {}, {})", p.x, p.y, p.z),
            (None, None) if request.unaimed => "an unaimed press".to_owned(),
            (None, None) => "the held press".to_owned(),
        };
        let mut machine = ClickMachine::new(
            request.kind,
            entity.map(|id| id.as_proto_u32().unwrap_or_default()),
            request.timeout,
            frame,
        );
        if let Some(press) = request.press {
            machine = machine.holding(press.hold_frames);
        }
        if request.unaimed {
            machine = machine.unaimed();
        }
        let (reply, rx) = RpcResultSender::channel();
        self.jobs.job = Some(Job {
            machine,
            reply,
            binding,
            button: request.button,
            scene,
            entity,
            target,
            at,
            what,
            started: frame,
            root_broadcast: false,
            root_events: Vec::new(),
            press: request.press,
            hit: None,
            entries: Vec::new(),
            events: Vec::new(),
            seen_events: HashSet::new(),
            hit_entity: None,
            took: None,
            release_step: None,
        });
        Answer::Pending(rx)
    }

    /// Ends a running job, the held press and the aim; answers once any release has landed.
    fn clear(&mut self) -> Answer {
        let (reply, rx) = RpcResultSender::channel();
        clear_pointer(
            &mut self.jobs,
            &mut self.synthetic,
            &mut self.log,
            &mut self.script,
            reply,
        );
        Answer::Pending(rx)
    }
}

fn hover_entity_cmd(mut input: ConsoleCommand<HoverEntityCommand>, mut control: PointerControl) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let result = if command.clear {
        control.clear()
    } else {
        hover_request(&command, &mut control).unwrap_or_else(Answer::Failed)
    };
    answer(&mut input, &mut control.pending, result);
}

fn hover_request(
    command: &HoverEntityCommand,
    control: &mut PointerControl,
) -> Result<Answer, String> {
    let frames = command
        .span
        .frames(&control.rate)?
        .unwrap_or(DEFAULT_HOVER_FRAMES);
    check_hover_frames(frames)?;
    let timeout = timeout_frames(
        command.timeout_frames,
        command.timeout_seconds,
        &control.rate,
    )?;
    let at = at_point(&command.at)?;
    Ok(control.start(Request {
        command: "/hover_entity",
        entity: command.entity.as_deref(),
        at,
        kind: ClickKind::Hover(frames),
        bounds: command.bounds,
        timeout,
        binding: None,
        button: None,
        hud_holds: false,
        unaimed: false,
        press: None,
        ui: false,
    }))
}

fn click_entity_cmd(
    mut input: ConsoleCommand<ClickEntityCommand>,
    mut control: PointerControl,
    input_map: Res<InputMap>,
    input_manager: InputManager,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let result = click_request(&command, &mut control, &input_map, &input_manager)
        .unwrap_or_else(Answer::Failed);
    answer(&mut input, &mut control.pending, result);
}

fn click_request(
    command: &ClickEntityCommand,
    control: &mut PointerControl,
    input_map: &InputMap,
    input_manager: &InputManager,
) -> Result<Answer, String> {
    let kind = match command.event.as_str() {
        "click" => ClickKind::Click,
        "down" => ClickKind::Down,
        "up" => ClickKind::Up,
        other => return Err(format!("unknown event `{other}`: use click, down or up")),
    };
    let (action, button) = match command.button.as_str() {
        "pointer" => (CommonInputAction::IaPointer, ("pointer", "IaPointer")),
        "primary" => (CommonInputAction::IaPrimary, ("primary", "IaPrimary")),
        "secondary" => (CommonInputAction::IaSecondary, ("secondary", "IaSecondary")),
        other => {
            return Err(format!(
                "unknown button `{other}`: use pointer, primary or secondary"
            ));
        }
    };
    let timeout = timeout_frames(
        command.timeout_frames,
        command.timeout_seconds,
        &control.rate,
    )?;
    let at = at_point(&command.at)?;
    let (binding, hud_holds) = if kind == ClickKind::Up {
        (None, false)
    } else {
        let Some(binding) = input_map.get_input(action) else {
            return Err(format!("{} has no binding", command.button));
        };
        if let Some(refusal) = binding_error(&command.button, &binding) {
            return Err(refusal);
        }
        let hud_holds = !input_manager.check_priority(&binding, InputPriority::Scene);
        (Some(binding), hud_holds)
    };
    Ok(control.start(Request {
        command: "/click_entity",
        entity: command.entity.as_deref(),
        at,
        kind,
        bounds: command.bounds,
        timeout,
        binding,
        button: Some(ButtonNames {
            arg: button.0,
            action: button.1,
        }),
        hud_holds,
        unaimed: false,
        press: None,
        ui: false,
    }))
}

// --- the driver ---

/// The scene ids of a hit container: (entity, container, the scene it belongs to).
pub(crate) fn hit_ids(
    container: Entity,
    ids: &Query<(Option<&SceneEntity>, Option<&ContainerEntity>)>,
) -> Option<(SceneEntityId, SceneEntityId, Entity)> {
    let (scene_entity, container_entity) = ids.get(container).ok()?;
    let (entity, root) = match (scene_entity, container_entity) {
        (Some(se), _) => (se.id, se.root),
        (None, Some(ce)) => (ce.container_id, ce.root),
        (None, None) => return None,
    };
    let container_id = container_entity.map_or(entity, |ce| ce.container_id);
    Some((entity, container_id, root))
}

/// The target as the machine compares it; a hit outside `scene` matches no entity of it.
fn target_seen(
    info: &PointerTargetInfo,
    scene: Entity,
    ids: &Query<(Option<&SceneEntity>, Option<&ContainerEntity>)>,
    parents: &Query<&ChildOf>,
) -> TargetSeen {
    let proto = |id: SceneEntityId| id.as_proto_u32().unwrap_or_default();
    let Some((entity, container, root)) = hit_ids(info.container, ids).filter(|ids| ids.2 == scene)
    else {
        return TargetSeen {
            entity: FOREIGN_HIT,
            container: FOREIGN_HIT,
            ancestors_hit: Vec::new(),
        };
    };
    let mut ancestors_hit = Vec::new();
    let mut next = parents.get(info.container).ok().map(ChildOf::parent);
    while let Some(ancestor) = next.filter(|&e| e != root) {
        if let Ok((Some(se), _)) = ids.get(ancestor) {
            if se.id == SceneEntityId::ROOT {
                break;
            }
            ancestors_hit.push(proto(se.id));
        }
        next = parents.get(ancestor).ok().map(ChildOf::parent);
    }
    TargetSeen {
        entity: proto(entity),
        container: proto(container),
        ancestors_hit,
    }
}

/// How a job ended: done (`Err` carries the diagnosis), or refused before pressing anything.
enum Outcome {
    Done(Result<(), String>),
    Refused(String),
}

/// Follows the running job each frame after the pointer pipeline: snapshots what the pipeline
/// saw, steps the machine, plays its presses and releases, and answers when it finishes.
#[allow(clippy::too_many_arguments)]
fn drive_pointer_job(
    mut jobs: ResMut<PointerJob>,
    frame: Res<FrameCount>,
    pointer: Res<PointerTarget>,
    mut synthetic: ResMut<SyntheticPointer>,
    mut log: ResMut<PointerEventLog>,
    mut script: ResMut<InputScript>,
    scenes: Query<&RendererSceneContext>,
    ids: Query<(Option<&SceneEntity>, Option<&ContainerEntity>)>,
    parents: Query<&ChildOf>,
    pointer_events: Query<&PointerEvents>,
    camera: Query<&GlobalTransform, With<PrimaryCamera>>,
    meshes: Query<(&Aabb, &GlobalTransform, &InheritedVisibility)>,
    children: Query<&Children>,
) {
    let frame = frame.0;
    let Some(job) = jobs.job.as_mut() else {
        return;
    };

    // the target counts only while this job's aim drives the pointer; with no aim, the cursor's
    // target counts until the press has landed, so the reply names what it landed on
    let aimed = frame > job.started
        && match job.at {
            Some(at) => synthetic.0.as_ref().is_some_and(|aim| aim.at == at),
            None => !job
                .events
                .iter()
                .chain(&job.root_events)
                .any(|e| e.event == "PetDown"),
        };
    let seen = pointer
        .0
        .as_ref()
        .filter(|_| aimed)
        .map(|info| (info, target_seen(info, job.scene, &ids, &parents)));
    if aimed && seen.is_none() {
        // the target is gone: what an earlier frame saw no longer stands
        job.hit = None;
        job.entries.clear();
    }
    if let Some((info, seen)) = &seen {
        let scene_title = hit_ids(info.container, &ids)
            .and_then(|(_, _, root)| scenes.get(root).ok())
            .map(|ctx| ctx.title.clone())
            .unwrap_or_default();
        job.hit = Some(HitReport {
            entity: seen.entity,
            container: seen.container,
            scene: scene_title,
            player_distance: info.distance.0,
            camera_distance: info.camera_distance.0,
            point: info.position,
        });
        job.entries = pointer_events
            .get(info.container)
            .map(|pe| hover_entries(info, pe))
            .unwrap_or_default();
        job.hit_entity = Some(seen.entity);
    }
    // an aim at a world point: where the hit is against the point
    let point = match job.at {
        Some(AimAt::Point(point)) if aimed => camera.single().ok().map(|camera| {
            let hit = seen.as_ref().map(|(info, _)| info);
            let inside = hit
                .and_then(|info| world_bounds(info.container, &meshes, &children))
                .is_some_and(|(min, max)| {
                    point.cmpge(min - BOUNDS_MARGIN).all() && point.cmple(max + BOUNDS_MARGIN).all()
                });
            classify_point(
                camera.translation(),
                point,
                hit.map(|info| info.position.unwrap_or(point)),
                inside,
            )
        }),
        _ => None,
    };

    let requested = job.requested();
    for logged in &log.events {
        let id = logged.entity.as_proto_u32().unwrap_or_default();
        let event = EventReport {
            event: format!("{:?}", logged.event),
            button: format!("{:?}", logged.button),
            tick: logged.tick,
        };
        if logged.entity == SceneEntityId::ROOT && requested != Some(id) {
            // another scene's root is no entity of this one
            if logged.container == job.scene {
                job.root_broadcast = true;
                if !job.root_events.contains(&event) {
                    job.root_events.push(event);
                }
            }
            continue;
        }
        // a press with no aim counts any entity's events: a proximity entry may take it
        if job.at.is_some() && Some(id) != requested && Some(id) != job.hit_entity {
            continue;
        }
        if logged.event == PointerEventType::PetDown && job.took.is_none() {
            job.took = Some(id);
        }
        if job
            .seen_events
            .insert((logged.event as i32, logged.button as i32, logged.tick))
        {
            job.events.push(event);
        }
    }
    // a press no entity took reaches the scene root: it landed all the same, so the job goes on
    // to its release rather than wait for the entity
    let logged = |event: PointerEventType| {
        let name = format!("{event:?}");
        job.events
            .iter()
            .chain(&job.root_events)
            .any(|e| e.event == name)
    };
    let ctx = scenes.get(job.scene).ok();
    let target = seen.map(|(_, seen)| seen);
    let observed = Observed {
        target,
        scene_tick: ctx.map(|ctx| ctx.tick_number),
        down_logged: logged(PointerEventType::PetDown),
        up_logged: logged(PointerEventType::PetUp),
        release_played: job.release_step.is_some_and(|step| script.played() >= step),
        entity_live: job
            .entity
            .is_none_or(|entity| ctx.and_then(|ctx| ctx.bevy_entity(entity)) == job.target),
        point,
        out_of_reach: job
            .button
            .is_some_and(|button| press_out_of_reach(button.action, &job.entries)),
    };

    let mut finished = None;
    for effect in job.machine.step(frame, observed) {
        match effect {
            Effect::Press | Effect::Release => {
                let state = if effect == Effect::Press {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                if let Some(step) = job
                    .binding
                    .and_then(|binding| binding_step(&binding, state))
                {
                    let step = script.push(step);
                    if effect == Effect::Release {
                        job.release_step = Some(step);
                    }
                }
            }
            Effect::Finish(result) => finished = Some(Outcome::Done(result)),
            Effect::Refuse(reason) => finished = Some(Outcome::Refused(reason)),
        }
    }
    let Some(result) = finished else {
        return;
    };

    let Some(job) = jobs.job.take() else {
        return;
    };
    let (reply, diagnosis) = match result {
        Outcome::Refused(reason) => (
            Err(refusal_json(reason, job.hit.as_ref(), job.root_broadcast)),
            None,
        ),
        Outcome::Done(result) => {
            let diagnosis = result.err();
            let warning = match job.at {
                Some(_) => job
                    .button
                    .filter(|_| diagnosis.is_none() && job.hit.is_some())
                    .and_then(|button| {
                        undeclared_warning(job.machine.kind(), button.action, &job.entries)
                    }),
                None => press::proximity_warning(job.took, job.hit.as_ref().map(|hit| hit.entity)),
            };
            let press = job.press.zip(job.button).map(|(press, button)| {
                let entity_bound = job.events.iter().any(|e| e.event == "PetDown");
                PressReport {
                    action: button.arg,
                    entity_bound,
                    aimed: job.at.is_some(),
                    hold_seconds: press.hold_seconds,
                    hold_frames: press.hold_frames,
                    root_events: &job.root_events,
                    hint: press::press_hint(
                        entity_bound,
                        job.root_broadcast,
                        job.hit.as_ref().map(|hit| hit.entity),
                        button.action,
                    ),
                }
            });
            let reply = reply_json(
                job.hit.as_ref(),
                &job.entries,
                &job.events,
                diagnosis.clone(),
                ReplyExtra {
                    button: job.button.filter(|_| job.press.is_none()).map(|b| b.arg),
                    root_broadcast: job.root_broadcast,
                    warning,
                    press,
                },
            );
            (Ok(reply), diagnosis)
        }
    };
    match (job.binding, job.at) {
        // a down that landed keeps its press and its open aim
        (Some(binding), Some(at))
            if diagnosis.is_none()
                && reply.is_ok()
                && job.machine.kind() == ClickKind::Down
                && job.machine.holds_press() =>
        {
            jobs.held = Some(Held {
                scene: job.scene,
                entity: job.entity,
                pressed: job.requested().or(job.hit_entity).unwrap_or_default(),
                target: job.target,
                at,
                binding,
            });
            job.reply.send(reply);
        }
        // anything else ends its aim, releasing a press it still holds first
        _ => {
            let release = job.release_binding();
            wind_down(
                &mut jobs,
                &mut synthetic,
                &mut log,
                &mut script,
                release,
                job.what,
                job.at,
                (job.reply, reply),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::{
        ecs::system::SystemState,
        input::{keyboard::KeyboardInput, mouse::MouseButtonInput},
    };

    use super::super::input::play_input_script;
    use super::*;

    type Control<'w> = (
        ResMut<'w, PointerJob>,
        ResMut<'w, SyntheticPointer>,
        ResMut<'w, PointerEventLog>,
        ResMut<'w, InputScript>,
    );

    /// The input script, the aim, the log, the job and the driver; no window and no scene.
    fn app() -> App {
        let mut app = App::new();
        app.add_event::<KeyboardInput>()
            .add_event::<MouseButtonInput>()
            .init_resource::<InputScript>()
            .init_resource::<SyntheticPointer>()
            .init_resource::<PointerEventLog>()
            .init_resource::<PointerJob>()
            .init_resource::<PointerTarget>()
            .insert_resource(FrameCount(0))
            .add_systems(PreUpdate, play_input_script)
            .add_systems(Update, (settle_release, drive_pointer_job).chain());
        app
    }

    #[test]
    fn clear_answers_and_frees_the_aim_only_once_the_release_has_played() {
        let mut app = app();
        let scene = app.world_mut().spawn_empty().id();
        let target = app.world_mut().spawn_empty().id();
        let entity = SceneEntityId::new(7, 0);
        let at = AimAt::Entity {
            entity,
            target,
            anchor: Anchor::Origin,
        };
        app.world_mut().resource_mut::<SyntheticPointer>().0 = Some(SyntheticAim {
            scene,
            at,
            hold: Hold::Open,
            started_frame: 0,
        });
        app.world_mut().resource_mut::<PointerJob>().held = Some(Held {
            scene,
            entity: Some(entity),
            pressed: 7,
            target: Some(target),
            at,
            binding: InputIdentifier::Mouse(MouseButton::Left),
        });
        // a step queued ahead (another command's), so the release plays a frame later
        app.world_mut()
            .resource_mut::<InputScript>()
            .push(Step::Cursor(Vec2::ZERO));

        let (reply, mut rx) = RpcResultSender::channel();
        let mut state = SystemState::<Control>::new(app.world_mut());
        let (mut jobs, mut synthetic, mut log, mut script) = state.get_mut(app.world_mut());
        clear_pointer(&mut jobs, &mut synthetic, &mut log, &mut script, reply);
        let refusal = Some(
            "a pointer command is still running on entity 7; wait for its reply or send /hover_entity --clear"
                .to_owned(),
        );

        // before anything played, and after the step ahead of the release: refused, aim kept
        for frame in 0..2 {
            assert_eq!(app.world().resource::<PointerJob>().busy(), refusal);
            assert!(
                app.world().resource::<SyntheticPointer>().0.is_some(),
                "frame {frame}"
            );
            assert!(rx.try_recv().is_err(), "frame {frame}");
            app.update();
        }

        // the release played in the last update, and the pipeline ran after it
        assert_eq!(app.world().resource::<InputScript>().played(), 2);
        assert_eq!(app.world().resource::<PointerJob>().busy(), None);
        assert!(app.world().resource::<SyntheticPointer>().0.is_none());
        assert_eq!(rx.try_recv().ok(), Some(Ok("cleared".to_owned())));
    }

    #[test]
    fn clear_with_nothing_held_answers_at_once() {
        let mut app = app();
        let (reply, mut rx) = RpcResultSender::channel();
        let mut state = SystemState::<Control>::new(app.world_mut());
        let (mut jobs, mut synthetic, mut log, mut script) = state.get_mut(app.world_mut());
        clear_pointer(&mut jobs, &mut synthetic, &mut log, &mut script, reply);
        assert_eq!(app.world().resource::<PointerJob>().busy(), None);
        assert_eq!(rx.try_recv().ok(), Some(Ok("cleared".to_owned())));
    }
}
