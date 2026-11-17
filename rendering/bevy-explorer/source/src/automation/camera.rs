//! `/camera_look`, `/look_at`, `/move_to`, `/set_camera_mode` and `/player_state`: turn the
//! player's camera as the mouse does, aim it at a world point, move the player, switch between first
//! and third person, and read the player's and the camera's pose. The turns go through
//! [`SyntheticLook`], which `update_camera` adds to the mouse's turn, so a scene camera, the pitch
//! clamp and a cinematic camera's ranges hold them back as they hold back a person's. `drive_camera`
//! follows a turn frame by frame and answers once it is done:
//!
//! - `/camera_look`: `{"deltaX", "deltaY", "units", "seconds", "turned": {"x", "y"},
//!   "cameraRotationEuler", "warning"?}`
//! - `/look_at`: `{"target", "aimErrorDegrees", "cameraRotationEuler", "cameraPosition", "mode",
//!   "settled", "frames", "warning"?}`
//! - `/move_to`: `{"position", "rotationEuler", "parcel", "settled", "cameraRotationEuler",
//!   "cameraPosition"}`, with `--look-at` also `"lookAt", "aimErrorDegrees", "aimSettled", "frames",
//!   "mode", "warning"?`
//!
//! Euler angles are Decentraland's (as a scene reads them): degrees from 0 to 360, `x` the pitch,
//! looking down positive, `y` the yaw clockwise from north, `z` the roll.

use bevy::prelude::*;
use bevy_console::ConsoleCommand;
use common::{
    rpc::{RpcCall, RpcCallEvent, RpcResultReceiver, RpcResultSender},
    sets::SceneSets,
    structs::{
        AvatarDynamicState, CameraOverride, CameraView, CursorLocks, OutOfWorld, PhotoMode,
        PrimaryCamera, PrimaryUser,
    },
};
use console::{
    reply::{self, refusal},
    DoAddConsoleCommand, PendingConsoleResponses,
};
use dcl_component::{
    proto_components::sdk::components::common::CameraType, transform_and_parent::DclTranslation,
};
use input_manager::{InputPriorities, InputPriority, InputType};
use scene_runner::{
    renderer_context::RendererSceneContext,
    update_world::camera_mode_area::{CameraHold, CameraModeHold},
};
use serde::Serialize;
use serde_json::{json, Value};
use user_input::camera::{update_camera, SyntheticLook, PITCH_LIMIT};

use super::{pointer::Point, walk::parcel_of};

/// The longest `/camera_look`.
const MAX_LOOK_SECONDS: f32 = 10.0;
/// The fastest `/camera_look`, in degrees per second either way.
const MAX_DEGREES_PER_SECOND: f32 = 3600.0;
/// How much the aim error may change in a frame and count as steady, in degrees.
const STEADY_DEGREES: f32 = 0.02;
/// Frames in a row the aim error must be steady for `/look_at` to have settled.
const STEADY_FRAMES: u32 = 5;
/// An aim error above this many degrees carries a warning.
const AIM_TOLERANCE_DEGREES: f32 = 2.0;
/// How far below the player's ground height counts as standing on it.
const GROUNDED_METRES: f32 = 0.2;
/// How far the player may move in a frame and count as still.
const STILL_METRES: f32 = 0.005;
/// Frames in a row the player must be still after a move to have settled.
const STILL_FRAMES: u32 = 3;
/// How long the player may take to settle after a move.
const SETTLE_SECONDS: f32 = 3.0;

pub(crate) struct AutomationCameraPlugin;

impl Plugin for AutomationCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraJob>()
            .add_console_command::<CameraLookCommand, _>(camera_look_cmd)
            .add_console_command::<LookAtCommand, _>(look_at_cmd)
            .add_console_command::<PlayerStateCommand, _>(player_state_cmd)
            .add_console_command::<MoveToCommand, _>(move_to_cmd)
            .add_console_command::<SetCameraModeCommand, _>(set_camera_mode_cmd)
            .add_systems(
                Update,
                drive_camera.in_set(SceneSets::Input).before(update_camera),
            );
    }
}

/// Turn the camera as a held mouse would, for --seconds (0.5 by default, at most 10): dx turns
/// right (+) or left (-) and dy looks up (+) or down (-), in degrees per second. The pitch clamp
/// and a scene camera hold it back as they hold back the mouse. Answers, as JSON, with the rates,
/// the degrees it turned (turned.x right, turned.y up) and the camera's cameraRotationEuler
/// (Decentraland's: x pitch, looking down positive; y yaw clockwise from north; degrees 0 to 360)
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/camera_look")]
pub struct CameraLookCommand {
    /// Degrees per second to the right (+) or left (-)
    #[arg(allow_negative_numbers = true)]
    dx: f32,
    /// Degrees per second up (+) or down (-)
    #[arg(allow_negative_numbers = true)]
    dy: f32,
    /// How long to turn, at most 10
    #[arg(long, default_value_t = 0.5)]
    seconds: f32,
}

/// Aim the camera at a world point (Decentraland coordinates, as /world_state gives them), turning
/// it until the point stays under the view's centre, and answer, as JSON, with aimErrorDegrees: the
/// angle left between the view and the point, with a warning above 2 when the pitch clamp or the
/// third-person camera's orbit keeps it off. Waits at most --timeout-seconds (3 by default)
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/look_at")]
pub struct LookAtCommand {
    #[arg(allow_negative_numbers = true)]
    x: f32,
    #[arg(allow_negative_numbers = true)]
    y: f32,
    #[arg(allow_negative_numbers = true)]
    z: f32,
    /// How long to wait for the aim to settle
    #[arg(long, default_value_t = 3.0)]
    timeout_seconds: f32,
}

/// Move the player straight to a world point (Decentraland coordinates), as a scene's movePlayerTo
/// does, wait for it to settle, and with --look-at x y z face the player that way and aim the
/// camera at the point as /look_at does. Answers, as JSON, with the pose (position, rotationEuler,
/// parcel, settled, cameraRotationEuler, cameraPosition) and, with --look-at, aimErrorDegrees
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/move_to")]
pub struct MoveToCommand {
    #[arg(allow_negative_numbers = true)]
    x: f32,
    #[arg(allow_negative_numbers = true)]
    y: f32,
    #[arg(allow_negative_numbers = true)]
    z: f32,
    /// A world point to face and aim the camera at
    #[arg(long, num_args = 3, value_names = ["X", "Y", "Z"], allow_negative_numbers = true)]
    look_at: Option<Vec<f32>>,
    /// How long the aim may take to settle
    #[arg(long, default_value_t = 3.0)]
    timeout_seconds: f32,
}

/// Switch the player's camera to first_person or third_person, as zooming in or out does. Refused,
/// naming it, while a scene's CameraModeArea or VirtualCamera holds the camera; drone and free are
/// not modes of this client. Answers, as JSON, with the mode before and after
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/set_camera_mode")]
pub struct SetCameraModeCommand {
    /// first_person, third_person, drone or free
    mode: String,
}

/// The player's and the camera's pose, as JSON: position, rotationEuler, parcel, velocity,
/// grounded, outOfWorld, camera {mode, position, rotationEuler, fov, modeChangeAllowed, heldBy,
/// pointerLocked} and pointerLocked. Positions are Decentraland world coordinates; euler angles are Decentraland's
/// (x pitch, looking down positive; y yaw clockwise from north; degrees 0 to 360)
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/player_state")]
pub struct PlayerStateCommand;

/// Decentraland euler angles, in degrees from 0 to 360.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct Euler {
    x: f64,
    y: f64,
    z: f64,
}

/// A bevy rotation as Decentraland euler angles. Decentraland mirrors bevy's z axis, which turns a
/// yaw or a pitch the other way and keeps a roll.
pub(crate) fn dcl_euler(rotation: Quat) -> Euler {
    let (yaw, pitch, roll) = rotation.to_euler(EulerRot::YXZ);
    let degrees = |radians: f32| {
        let degrees = reply::round(radians.to_degrees().rem_euclid(360.0));
        if degrees >= 360.0 {
            0.0
        } else {
            degrees
        }
    };
    Euler {
        x: degrees(-pitch),
        y: degrees(-yaw),
        z: degrees(roll),
    }
}

/// The yaw and pitch, in bevy's radians, of a camera looking along `direction`.
pub(crate) fn aim_angles(direction: Vec3) -> (f32, f32) {
    let yaw = (-direction.x).atan2(-direction.z);
    let pitch = direction
        .y
        .atan2(Vec2::new(direction.x, direction.z).length());
    (yaw, pitch)
}

/// An angle in radians brought to -π..π.
pub(crate) fn wrap(angle: f32) -> f32 {
    (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// The angle, in degrees, between a camera's forward and the way to a point.
pub(crate) fn aim_error_degrees(forward: Vec3, to_point: Vec3) -> f32 {
    forward.angle_between(to_point).to_degrees()
}

fn forced_mode(mode: CameraType) -> &'static str {
    match mode {
        CameraType::CtFirstPerson => "first_person",
        CameraType::CtThirdPerson => "third_person",
        CameraType::CtCinematic => "cinematic",
    }
}

/// What holds the camera, as a refusal names it: the text and the cause.
fn hold_cause(hold: &CameraHold, scenes: &Query<&RendererSceneContext>) -> (String, Value) {
    let crdt = hold.entity.as_proto_u32().unwrap_or_default();
    let (scene, parcel) = scenes.get(hold.scene).map_or_else(
        |_| (String::new(), [0, 0]),
        |ctx| (ctx.hash.to_string(), [ctx.base.x, ctx.base.y]),
    );
    match &hold.area {
        Some(area) => {
            let size = area.area.unwrap_or_default();
            let forced = forced_mode(area.mode());
            (
                format!(
                    "camera mode is locked by a CameraModeArea of scene '{scene}' (base parcel \
                     {},{}; entity crdtId {crdt}; forces {forced}; area ({:.2}, {:.2}, {:.2})); \
                     leave the area to change modes",
                    parcel[0], parcel[1], size.x, size.y, size.z
                ),
                json!({
                    "kind": "CameraModeArea",
                    "sceneId": scene,
                    "parcel": parcel,
                    "crdtEntityId": crdt,
                    "forcedMode": forced,
                    "size": Point::from(Vec3::new(size.x, size.y, size.z)),
                }),
            )
        }
        None => (
            format!(
                "the view is held by the VirtualCamera (entity crdtId {crdt}) of scene '{scene}' \
                 (base parcel {},{}); the scene must hand the camera back",
                parcel[0], parcel[1]
            ),
            json!({
                "kind": "VirtualCamera",
                "sceneId": scene,
                "parcel": parcel,
                "crdtEntityId": crdt,
            }),
        ),
    }
}

fn mode_name(camera: &PrimaryCamera, photo: bool) -> &'static str {
    if photo {
        return "free";
    }
    match camera.view() {
        CameraView::FirstPerson => "first_person",
        CameraView::ThirdPerson => "third_person",
        CameraView::Cinematic => "cinematic",
    }
}

enum Gesture {
    Look {
        /// radians per second: right, up
        rate: Vec2,
        seconds: f32,
        elapsed: f32,
        /// radians asked of the camera so far: right, up
        applied: Vec2,
        /// the camera's yaw and pitch when the turn started
        start: Vec2,
        /// the turn is done; answer next frame, once the camera has moved
        done: bool,
    },
    Aim {
        /// the point, in bevy's world
        target: Vec3,
        timeout: f32,
        elapsed: f32,
        frames: u32,
        last_error: Option<f32>,
        steady: u32,
    },
    Move {
        /// answers once the engine has moved the player
        moved: RpcResultReceiver<bool>,
        /// `None` until it has; then the frame the player has been still since, in frames
        still: Option<u32>,
        settle: f32,
        last: Vec3,
        /// the point to aim at once the player has settled, and the aim's timeout
        look_at: Option<(Vec3, f32)>,
    },
}

struct Job {
    reply: RpcResultSender<Result<String, String>>,
    gesture: Gesture,
    /// a `/move_to`'s pose, answered with its aim
    moved: Option<Value>,
}

#[derive(Resource, Default)]
pub struct CameraJob(Option<Job>);

impl CameraJob {
    /// A turn, aim or move is running.
    pub(crate) fn busy(&self) -> bool {
        self.0.is_some()
    }
}

/// Why a turn cannot start, or `None`. A cinematic camera refuses `/look_at` always and
/// `/camera_look` when it allows no manual rotation.
fn refuse_turn(
    job: &CameraJob,
    camera: Option<(&PrimaryCamera, bool)>,
    out_of_world: bool,
    priorities: &InputPriorities,
    aim: bool,
) -> Option<String> {
    if job.0.is_some() {
        return Some(refusal(
            "a camera turn is still running; wait for its reply",
            None,
        ));
    }
    let Some((camera, photo)) = camera else {
        return Some(refusal("no camera", None));
    };
    if photo {
        return Some(refusal("photo mode holds the camera", None));
    }
    if out_of_world {
        return Some(refusal(
            "the player is out of world (loading): the camera looks at it from above",
            None,
        ));
    }
    if priorities.get(InputType::All) > InputPriority::Scene {
        return Some(refusal("input held by the HUD", None));
    }
    if let Some(CameraOverride::Cinematic(cinematic)) = &camera.scene_override {
        if aim || !cinematic.allow_manual_rotation {
            let reason = if aim {
                "a scene camera (VirtualCamera or cinematic CameraModeArea) holds the view"
            } else {
                "a scene camera (VirtualCamera or cinematic CameraModeArea) holds the view and \
                 allows no manual rotation"
            };
            return Some(refusal(reason, Some(json!({ "camera": "cinematic" }))));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn camera_look_cmd(
    mut input: ConsoleCommand<CameraLookCommand>,
    mut job: ResMut<CameraJob>,
    mut pending: ResMut<PendingConsoleResponses>,
    priorities: Res<InputPriorities>,
    camera: Query<(&PrimaryCamera, Has<PhotoMode>)>,
    player: Query<Has<OutOfWorld>, With<PrimaryUser>>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let seconds = command.seconds;
    if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_LOOK_SECONDS {
        input.reply_failed(format!(
            "--seconds takes more than 0 and at most {MAX_LOOK_SECONDS}; got {seconds}"
        ));
        return;
    }
    let in_range = |v: f32| v.is_finite() && v.abs() <= MAX_DEGREES_PER_SECOND;
    if !in_range(command.dx) || !in_range(command.dy) {
        input.reply_failed(format!(
            "dx and dy take degrees per second from -{MAX_DEGREES_PER_SECOND} to \
             {MAX_DEGREES_PER_SECOND}; got {} {}",
            command.dx, command.dy
        ));
        return;
    }
    let camera = camera.single().ok();
    let out_of_world = player.single().unwrap_or_default();
    if let Some(refused) = refuse_turn(&job, camera, out_of_world, &priorities, false) {
        input.reply_failed(refused);
        return;
    }
    let start = camera.map_or(Vec2::ZERO, |(c, _)| Vec2::new(c.yaw, c.pitch));
    let rx = start_job(
        &mut job,
        Gesture::Look {
            rate: Vec2::new(command.dx, command.dy) * std::f32::consts::PI / 180.0,
            seconds,
            elapsed: 0.0,
            applied: Vec2::ZERO,
            start,
            done: false,
        },
    );
    pending.push_receiver(rx, |r| r, input.take_responder());
}

#[allow(clippy::too_many_arguments)]
fn look_at_cmd(
    mut input: ConsoleCommand<LookAtCommand>,
    mut job: ResMut<CameraJob>,
    mut pending: ResMut<PendingConsoleResponses>,
    priorities: Res<InputPriorities>,
    camera: Query<(&PrimaryCamera, Has<PhotoMode>)>,
    player: Query<Has<OutOfWorld>, With<PrimaryUser>>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let timeout = command.timeout_seconds;
    if !timeout.is_finite() || timeout <= 0.0 || timeout > 30.0 {
        input.reply_failed(format!(
            "--timeout-seconds takes more than 0 and at most 30; got {timeout}"
        ));
        return;
    }
    if ![command.x, command.y, command.z]
        .iter()
        .all(|v| v.is_finite())
    {
        input.reply_failed("x, y and z must be numbers");
        return;
    }
    let camera = camera.single().ok();
    let out_of_world = player.single().unwrap_or_default();
    if let Some(refused) = refuse_turn(&job, camera, out_of_world, &priorities, true) {
        input.reply_failed(refused);
        return;
    }
    let target =
        dcl_component::transform_and_parent::DclTranslation([command.x, command.y, command.z])
            .to_bevy_translation();
    let rx = start_job(
        &mut job,
        Gesture::Aim {
            target,
            timeout,
            elapsed: 0.0,
            frames: 0,
            last_error: None,
            steady: 0,
        },
    );
    pending.push_receiver(rx, |r| r, input.take_responder());
}

fn start_job(job: &mut CameraJob, gesture: Gesture) -> RpcResultReceiver<Result<String, String>> {
    let (reply, rx) = RpcResultSender::channel();
    job.0 = Some(Job {
        reply,
        gesture,
        moved: None,
    });
    rx
}

#[allow(clippy::too_many_arguments)]
fn move_to_cmd(
    mut input: ConsoleCommand<MoveToCommand>,
    mut job: ResMut<CameraJob>,
    mut pending: ResMut<PendingConsoleResponses>,
    mut events: EventWriter<RpcCallEvent>,
    priorities: Res<InputPriorities>,
    camera: Query<(&PrimaryCamera, Has<PhotoMode>)>,
    player: Query<(&GlobalTransform, Has<OutOfWorld>), With<PrimaryUser>>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let timeout = command.timeout_seconds;
    if !timeout.is_finite() || timeout <= 0.0 || timeout > 30.0 {
        input.reply_failed(format!(
            "--timeout-seconds takes more than 0 and at most 30; got {timeout}"
        ));
        return;
    }
    let look_at = command
        .look_at
        .as_deref()
        .map(|v| Vec3::new(v[0], v[1], v[2]));
    let numbers = [command.x, command.y, command.z]
        .into_iter()
        .chain(look_at.iter().flat_map(|v| v.to_array()));
    if !numbers.into_iter().all(f32::is_finite) {
        input.reply_failed("the coordinates must be numbers");
        return;
    }
    let Ok((transform, out_of_world)) = player.single() else {
        input.reply_failed("no player");
        return;
    };
    let refused = match look_at {
        Some(_) => refuse_turn(&job, camera.single().ok(), out_of_world, &priorities, true),
        None => job.0.is_some().then(|| {
            refusal(
                "a camera turn or move is still running; wait for its reply",
                None,
            )
        }),
    };
    if let Some(refused) = refused {
        input.reply_failed(refused);
        return;
    }
    let to = DclTranslation([command.x, command.y, command.z]).to_bevy_translation();
    let look_at = look_at.map(|v| DclTranslation(v.to_array()).to_bevy_translation());
    let (response, moved) = RpcResultSender::<bool>::channel();
    events.write(RpcCallEvent::engine(RpcCall::MovePlayer {
        to,
        looking_at: look_at,
        duration: None,
        camera_rotation: None,
        response: Some(response),
    }));
    let rx = start_job(
        &mut job,
        Gesture::Move {
            moved,
            still: None,
            settle: 0.0,
            last: transform.translation(),
            look_at: look_at.map(|point| (point, timeout)),
        },
    );
    pending.push_receiver(rx, |r| r, input.take_responder());
}

fn set_camera_mode_cmd(
    mut input: ConsoleCommand<SetCameraModeCommand>,
    mut camera: Query<(&mut PrimaryCamera, Has<PhotoMode>)>,
    hold: Res<CameraModeHold>,
    scenes: Query<&RendererSceneContext>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let first_person = match command.mode.as_str() {
        "first_person" => true,
        "third_person" => false,
        "drone" | "free" => {
            input.reply_failed(refusal(
                format!(
                    "this client has no {} camera mode: its modes are first_person and \
                     third_person (photo mode, opened from the HUD, is its free camera)",
                    command.mode
                ),
                None,
            ));
            return;
        }
        other => {
            input.reply_failed(format!(
                "unknown mode `{other}`: use first_person, third_person, drone or free"
            ));
            return;
        }
    };
    let Ok((mut camera, photo)) = camera.single_mut() else {
        input.reply_failed(refusal("no camera", None));
        return;
    };
    if photo {
        input.reply_failed(refusal("photo mode holds the camera", None));
        return;
    }
    if let Some(hold) = &hold.0 {
        let (reason, cause) = hold_cause(hold, &scenes);
        input.reply_failed(refusal(reason, Some(cause)));
        return;
    }
    if camera.scene_override.is_some() {
        input.reply_failed(refusal("a scene camera holds the camera", None));
        return;
    }
    let before = mode_name(&camera, photo);
    if first_person {
        camera.distance = 0.0;
    } else if camera.distance <= PrimaryCamera::FIRST_PERSON_DISTANCE {
        // the default follow distance; a player's own third-person zoom is kept
        camera.distance = 1.0;
    }
    let after = mode_name(&camera, photo);
    input.reply_ok(
        json!({ "before": before, "after": after, "changed": before != after }).to_string(),
    );
}

/// Follows the turn each frame, before `update_camera`: asks the camera for this frame's share of
/// a `/camera_look`, or, for a `/look_at`, for the yaw and pitch that aim it at the point from where
/// it now stands (a third-person camera moves as it turns, so it is asked again each frame), and
/// answers once the turn is done or the aim error has stopped changing.
fn drive_camera(
    mut job: ResMut<CameraJob>,
    mut look: ResMut<SyntheticLook>,
    time: Res<Time>,
    camera: Query<(&Transform, &PrimaryCamera, Has<PhotoMode>)>,
    player: Query<&GlobalTransform, With<PrimaryUser>>,
) {
    let Some(current) = job.0.as_mut() else {
        return;
    };
    let Ok((transform, camera, photo)) = camera.single() else {
        if let Some(job) = job.0.take() {
            job.reply.send(Err(refusal("no camera", None)));
        }
        return;
    };
    let dt = time.delta_secs();
    let mut next = None;
    let answer = match &mut current.gesture {
        Gesture::Look {
            rate,
            seconds,
            elapsed,
            applied,
            start,
            done,
        } => {
            if *done {
                Some(Ok(look_reply(
                    *rate, *seconds, *applied, *start, transform, camera,
                )))
            } else {
                *elapsed = (*elapsed + dt).min(*seconds);
                let want = *rate * *elapsed;
                let step = want - *applied;
                *applied = want;
                look.yaw += step.x;
                look.pitch += step.y;
                *done = *elapsed >= *seconds;
                None
            }
        }
        Gesture::Aim {
            target,
            timeout,
            elapsed,
            frames,
            last_error,
            steady,
        } => {
            let to_point = *target - transform.translation;
            let error = aim_error_degrees(transform.forward().as_vec3(), to_point);
            *steady = match *last_error {
                Some(last) if (error - last).abs() < STEADY_DEGREES => *steady + 1,
                _ => 0,
            };
            *last_error = Some(error);
            *elapsed += dt;
            let settled = *steady >= STEADY_FRAMES;
            if settled || *elapsed >= *timeout {
                let aim = aim_reply(
                    *target, error, settled, *frames, *timeout, transform, camera, photo,
                );
                Some(match current.moved.take() {
                    Some(moved) => Ok(with_aim(moved, aim)),
                    None => Ok(aim),
                })
            } else {
                let (yaw, pitch) = aim_angles(to_point);
                // `update_camera` turns right by decreasing the yaw
                look.yaw = -wrap(yaw - camera.yaw);
                look.pitch = pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT) - camera.pitch;
                *frames += 1;
                None
            }
        }
        Gesture::Move {
            moved,
            still,
            settle,
            last,
            look_at,
        } => {
            let position = player.single().map_or(*last, GlobalTransform::translation);
            let still_now = position.distance(*last) < STILL_METRES;
            *last = position;
            match *still {
                None => match moved.poll_once() {
                    Ok(None) => None,
                    Ok(Some(true)) => {
                        *still = Some(0);
                        None
                    }
                    Ok(Some(false)) | Err(()) => {
                        Some(Err(refusal("the engine cancelled the move", None)))
                    }
                },
                Some(frames) => {
                    let frames = if still_now { frames + 1 } else { 0 };
                    *still = Some(frames);
                    *settle += dt;
                    let settled = frames >= STILL_FRAMES;
                    if settled || *settle >= SETTLE_SECONDS {
                        let pose = move_pose(position, player.single().ok(), settled);
                        match *look_at {
                            Some((target, timeout)) => {
                                current.moved = Some(pose);
                                next = Some(Gesture::Aim {
                                    target,
                                    timeout,
                                    elapsed: 0.0,
                                    frames: 0,
                                    last_error: None,
                                    steady: 0,
                                });
                                None
                            }
                            None => {
                                let mut pose = pose;
                                pose["cameraRotationEuler"] = json!(dcl_euler(transform.rotation));
                                pose["cameraPosition"] =
                                    json!(Point::from_bevy(transform.translation));
                                Some(Ok(pose.to_string()))
                            }
                        }
                    } else {
                        None
                    }
                }
            }
        }
    };
    if let Some(gesture) = next {
        current.gesture = gesture;
    }
    if let Some(answer) = answer {
        if let Some(job) = job.0.take() {
            job.reply.send(answer);
        }
    }
}

/// Where a move left the player.
fn move_pose(position: Vec3, transform: Option<&GlobalTransform>, settled: bool) -> Value {
    let rotation = transform.map_or(Quat::IDENTITY, |t| t.to_scale_rotation_translation().1);
    let [x, _, z] = DclTranslation::from_bevy_translation(position).0;
    json!({
        "position": Point::from_bevy(position),
        "rotationEuler": dcl_euler(rotation),
        "parcel": parcel_of(x, z),
        "settled": settled,
    })
}

/// A move's pose with its aim: the aim's camera pose and error, its target as `lookAt` and its
/// `settled` as `aimSettled`.
fn with_aim(mut moved: Value, aim: String) -> String {
    if let (Some(moved), Ok(Value::Object(aim))) =
        (moved.as_object_mut(), serde_json::from_str::<Value>(&aim))
    {
        for (key, value) in aim {
            let key = match key.as_str() {
                "target" => "lookAt".to_owned(),
                "settled" => "aimSettled".to_owned(),
                _ => key,
            };
            moved.insert(key, value);
        }
    }
    moved.to_string()
}

fn look_reply(
    rate: Vec2,
    seconds: f32,
    applied: Vec2,
    start: Vec2,
    transform: &Transform,
    camera: &PrimaryCamera,
) -> String {
    // turned right is a falling yaw
    let turned = Vec2::new(start.x - camera.yaw, camera.pitch - start.y);
    let mut reply = json!({
        "deltaX": reply::round(rate.x.to_degrees()),
        "deltaY": reply::round(rate.y.to_degrees()),
        "units": "degrees per second",
        "seconds": reply::round(seconds),
        "turned": {
            "x": reply::round(turned.x.to_degrees()),
            "y": reply::round(turned.y.to_degrees()),
        },
        "cameraRotationEuler": dcl_euler(transform.rotation),
    });
    let short = (applied - turned).abs() * 180.0 / std::f32::consts::PI;
    let warning = (short.max_element() > 0.5).then(|| {
        let cause = if (camera.pitch.abs() - PITCH_LIMIT).abs() < 1e-3 {
            "the pitch clamp (85.7 degrees up or down) stopped it"
        } else if camera.scene_override.is_some() {
            "a scene camera's range held it back"
        } else {
            "other input turned the camera too"
        };
        format!(
            "asked for {:.1} degrees right and {:.1} up, turned {:.1} and {:.1}: {cause}",
            applied.x.to_degrees(),
            applied.y.to_degrees(),
            turned.x.to_degrees(),
            turned.y.to_degrees()
        )
    });
    reply::add_warning(&mut reply, warning);
    reply.to_string()
}

#[allow(clippy::too_many_arguments)]
fn aim_reply(
    target: Vec3,
    error: f32,
    settled: bool,
    frames: u32,
    timeout: f32,
    transform: &Transform,
    camera: &PrimaryCamera,
    photo: bool,
) -> String {
    let mut reply = json!({
        "target": Point::from_bevy(target),
        "aimErrorDegrees": reply::round(error),
        "cameraRotationEuler": dcl_euler(transform.rotation),
        "cameraPosition": Point::from_bevy(transform.translation),
        "mode": mode_name(camera, photo),
        "settled": settled,
        "frames": frames,
    });
    let (_, pitch) = aim_angles(target - transform.translation);
    let warning = if !settled {
        Some(format!(
            "timed out after {timeout} s waiting for the aim to settle; the view is {:.1} degrees \
             off the point",
            error
        ))
    } else if error > AIM_TOLERANCE_DEGREES {
        Some(if pitch.abs() > PITCH_LIMIT {
            format!(
                "the pitch clamp (85.7 degrees up or down) keeps the point {error:.1} degrees off \
                 the view; first person or a farther stand reaches it"
            )
        } else {
            format!(
                "the camera settled {error:.1} degrees off the point: the third-person camera \
                 orbits the player's head, so it cannot centre a point this close to it"
            )
        })
    } else {
        None
    };
    reply::add_warning(&mut reply, warning);
    reply.to_string()
}

#[allow(clippy::type_complexity)]
fn player_state_cmd(
    mut input: ConsoleCommand<PlayerStateCommand>,
    player: Query<
        (
            &GlobalTransform,
            Option<&AvatarDynamicState>,
            Has<OutOfWorld>,
        ),
        With<PrimaryUser>,
    >,
    camera: Query<(
        &GlobalTransform,
        &PrimaryCamera,
        &Projection,
        Has<PhotoMode>,
    )>,
    locks: Res<CursorLocks>,
    hold: Res<CameraModeHold>,
    scenes: Query<&RendererSceneContext>,
) {
    let Some(Ok(_)) = input.take() else {
        return;
    };
    let Ok((transform, dynamics, out_of_world)) = player.single() else {
        input.reply_failed("no player");
        return;
    };
    let (_, rotation, position) = transform.to_scale_rotation_translation();
    let [x, _, z] =
        dcl_component::transform_and_parent::DclTranslation::from_bevy_translation(position).0;
    let velocity = dynamics.map_or(Vec3::ZERO, |d| d.velocity);
    let mut reply = json!({
        "position": Point::from_bevy(position),
        "rotationEuler": dcl_euler(rotation),
        "parcel": parcel_of(x, z),
        "velocity": Point::from(Vec3::new(velocity.x, velocity.y, -velocity.z)),
        "grounded": dynamics.map(|d| d.ground_height < GROUNDED_METRES),
        "outOfWorld": out_of_world,
        "camera": Value::Null,
        "pointerLocked": locks.0.contains("camera"),
    });
    if let Ok((transform, camera, projection, photo)) = camera.single() {
        let (_, rotation, position) = transform.to_scale_rotation_translation();
        let fov = match projection {
            Projection::Perspective(perspective) => {
                Some(reply::round(perspective.fov.to_degrees()))
            }
            _ => None,
        };
        reply["camera"] = json!({
            "mode": mode_name(camera, photo),
            "position": Point::from_bevy(position),
            "rotationEuler": dcl_euler(rotation),
            "fov": fov,
            "modeChangeAllowed": camera.scene_override.is_none(),
            "heldBy": hold.0.as_ref().map(|hold| hold_cause(hold, &scenes).1),
            "pointerLocked": locks.0.contains("camera"),
        });
    }
    input.reply_ok(reply.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn euler_is_decentralands_yaw_from_north_and_pitch_down_positive() {
        assert_eq!(
            dcl_euler(Quat::IDENTITY),
            Euler {
                x: 0.0,
                y: 0.0,
                z: 0.0
            }
        );
        // a bevy yaw of +90 degrees looks west: 270 clockwise from north
        let west = Quat::from_rotation_y(90f32.to_radians());
        assert_eq!(dcl_euler(west).y, 270.0);
        // a bevy pitch of +30 degrees looks up: -30, so 330
        let up = Quat::from_rotation_x(30f32.to_radians());
        assert_eq!(dcl_euler(up).x, 330.0);
    }

    #[test]
    fn aim_angles_point_the_camera_along_the_direction() {
        let (yaw, pitch) = aim_angles(Vec3::NEG_Z);
        assert!(close(yaw, 0.0) && close(pitch, 0.0));
        let (yaw, _) = aim_angles(Vec3::NEG_X);
        assert!(close(yaw, 90f32.to_radians()));
        let (_, pitch) = aim_angles(Vec3::new(0.0, 1.0, -1.0));
        assert!(close(pitch, 45f32.to_radians()));
        let direction = Vec3::new(3.0, -2.0, 5.0);
        let (yaw, pitch) = aim_angles(direction);
        let forward = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0) * Vec3::NEG_Z;
        assert!(aim_error_degrees(forward, direction) < 0.01);
    }

    #[test]
    fn wrap_takes_the_short_way_round() {
        assert!(close(wrap(270f32.to_radians()), -90f32.to_radians()));
        assert!(close(wrap(-200f32.to_radians()), 160f32.to_radians()));
        assert!(close(wrap(0.5), 0.5));
    }
}
