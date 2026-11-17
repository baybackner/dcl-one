//! `/sweep_pointer`: press a pointer button on a target, turn the camera while it is held, and
//! release, as a person painting a stroke would. The camera is first aimed at the target (as
//! `/look_at` aims it) and the cursor parked at the screen's centre, so the press, the scene's
//! `PrimaryPointerInfo.worldRayDirection` and every ray while the button is held go through the
//! view's centre; the button is the real binding, pressed through the input script, and the turn is
//! a [`SyntheticLook`]. `drive_sweep` answers once the release has landed:
//!
//! `{"button", "deltaX", "deltaY", "units", "seconds", "aimErrorDegrees", "pressed": {"frame",
//! "tick", "hit"}, "swept": {"x", "y"}, "released": {"frame", "tick", "hit"},
//! "cameraRotationEuler", "warning"?}`, each `hit` `{"crdtEntityId", "hitPoint"}` or `null`.

use bevy::{diagnostic::FrameCount, input::ButtonState, prelude::*, window::PrimaryWindow};
use bevy_console::ConsoleCommand;
use common::{
    inputs::{CommonInputAction, InputIdentifier, InputMap},
    rpc::RpcResultSender,
    sets::SceneSets,
    structs::{PhotoMode, PrimaryCamera},
};
use console::{
    reply::{self, refusal},
    DoAddConsoleCommand, PendingConsoleResponses,
};
use dcl_component::transform_and_parent::DclTranslation;
use input_manager::{InputManager, InputPriority};
use scene_inspector::{parse_entity_id, SceneResolver};
use scene_runner::{
    renderer_context::RendererSceneContext,
    update_scene::{
        pointer_results::PointerTarget,
        synthetic_pointer::{anchor_point, Anchor, SyntheticPointer},
    },
    ContainerEntity, SceneEntity,
};
use serde_json::{json, Value};
use user_input::camera::{update_camera, SyntheticLook, PITCH_LIMIT};

use super::{
    camera::{aim_angles, aim_error_degrees, dcl_euler, wrap, CameraJob},
    input::{InputScript, Step},
    pointer::{binding_error, binding_step, hit_ids, Point},
};

/// The longest sweep.
const MAX_SECONDS: f32 = 10.0;
/// How much the aim error may change in a frame and count as steady, in degrees.
const STEADY_DEGREES: f32 = 0.02;
/// Frames in a row the aim error must be steady before the press.
const STEADY_FRAMES: u32 = 5;
/// How long the aim may take before the press.
const AIM_SECONDS: f32 = 3.0;

pub(crate) struct AutomationSweepPlugin;

impl Plugin for AutomationSweepPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SweepJob>()
            .add_console_command::<SweepPointerCommand, _>(sweep_pointer_cmd)
            .add_systems(
                Update,
                drive_sweep.in_set(SceneSets::Input).before(update_camera),
            );
    }
}

/// Press a pointer button on a scene entity (--entity, at its bounds' centre) or a world point
/// (--at x y z), turn the camera while it is held at --dx degrees per second right (+) or left (-)
/// and --dy up (+) or down (-) for --seconds (1 by default, at most 10), and release. The camera
/// is aimed at the target first and the cursor parked at the view's centre, so the scene's
/// pointer ray follows the turn. Answers, as JSON, with what the ray hit at the press and at the
/// release (each with its frame and the scene's tick) and the degrees swept
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/sweep_pointer")]
pub struct SweepPointerCommand {
    /// The scene entity to press on
    #[arg(long)]
    entity: Option<String>,
    /// The world point to press on (Decentraland coordinates)
    #[arg(long, num_args = 3, value_names = ["X", "Y", "Z"], allow_negative_numbers = true)]
    at: Option<Vec<f32>>,
    /// Degrees per second to the right (+) or left (-) while held
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    dx: f32,
    /// Degrees per second up (+) or down (-) while held
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    dy: f32,
    /// How long to hold and turn
    #[arg(long, default_value_t = 1.0)]
    seconds: f32,
    /// pointer (default), primary or secondary
    #[arg(long, default_value = "pointer")]
    button: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    /// turning the camera onto the target
    Aiming {
        elapsed: f32,
        last_error: Option<f32>,
        steady: u32,
    },
    /// the cursor move and the press are queued
    Pressing { step: u64 },
    /// the button is down; `elapsed` of the turn done
    Holding { elapsed: f32 },
    /// the release is queued
    Releasing { step: u64 },
}

struct Sweep {
    reply: RpcResultSender<Result<String, String>>,
    stage: Stage,
    /// the point to press on, in bevy's world
    target: Vec3,
    /// the entity asked for, to warn when the press lands elsewhere
    entity: Option<u32>,
    scene: Entity,
    binding: InputIdentifier,
    button: String,
    /// radians per second: right, up
    rate: Vec2,
    seconds: f32,
    aim_error: f32,
    /// the camera's yaw and pitch when the button went down
    start: Vec2,
    pressed: Value,
}

#[derive(Resource, Default)]
pub struct SweepJob(Option<Sweep>);

#[allow(clippy::too_many_arguments)]
fn sweep_pointer_cmd(
    mut input: ConsoleCommand<SweepPointerCommand>,
    mut job: ResMut<SweepJob>,
    camera_job: Res<CameraJob>,
    synthetic: Res<SyntheticPointer>,
    mut pending: ResMut<PendingConsoleResponses>,
    resolver: SceneResolver,
    input_map: Res<InputMap>,
    input_manager: InputManager,
    camera: Query<(&PrimaryCamera, Has<PhotoMode>)>,
    transforms: Query<&GlobalTransform>,
    meshes: Query<(
        &bevy::render::primitives::Aabb,
        &GlobalTransform,
        &InheritedVisibility,
    )>,
    children: Query<&Children>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let result = (|| {
        if job.0.is_some() || camera_job.busy() {
            return Err(refusal(
                "a sweep or camera turn is still running; wait for its reply",
                None,
            ));
        }
        if synthetic.0.is_some() {
            return Err(refusal(
                "a pointer command's aim is active; wait for it to end or send /hover_entity --clear",
                None,
            ));
        }
        let seconds = command.seconds;
        if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_SECONDS {
            return Err(format!(
                "--seconds takes more than 0 and at most {MAX_SECONDS}; got {seconds}"
            ));
        }
        if !command.dx.is_finite() || !command.dy.is_finite() {
            return Err("--dx and --dy take degrees per second".to_owned());
        }
        let action = match command.button.as_str() {
            "pointer" => CommonInputAction::IaPointer,
            "primary" => CommonInputAction::IaPrimary,
            "secondary" => CommonInputAction::IaSecondary,
            other => {
                return Err(format!(
                    "unknown button `{other}`: use pointer, primary or secondary"
                ))
            }
        };
        let name = format!("{action:?}");
        let binding = input_map
            .get_input(action)
            .ok_or_else(|| format!("{name} has no binding"))?;
        if let Some(refused) = binding_error(&name, &binding) {
            return Err(refused);
        }
        if !input_manager.check_priority(&binding, InputPriority::Scene) {
            return Err(refusal("input held by the HUD", None));
        }
        match camera.single() {
            Ok((_, true)) => return Err(refusal("photo mode holds the camera", None)),
            Ok((camera, false)) if camera.scene_override.is_some() => {
                return Err(refusal(
                    "a scene camera (CameraModeArea or VirtualCamera) holds the camera",
                    None,
                ))
            }
            Ok(_) => (),
            Err(_) => return Err(refusal("no camera", None)),
        }
        let (scene, ctx) = resolver.resolve()?;
        let (target, entity) = match (&command.at, &command.entity) {
            (Some(at), entity) => (
                DclTranslation([at[0], at[1], at[2]]).to_bevy_translation(),
                entity
                    .as_deref()
                    .map(parse_entity_id)
                    .transpose()?
                    .and_then(|id| id.as_proto_u32()),
            ),
            (None, Some(entity)) => {
                let id = parse_entity_id(entity)?;
                let target = ctx
                    .bevy_entity(id)
                    .ok_or_else(|| refusal(format!("entity {entity} is not live"), None))?;
                let point = anchor_point(target, Anchor::Bounds, &transforms, &meshes, &children)
                    .or_else(|| transforms.get(target).ok().map(|t| t.translation()))
                    .ok_or_else(|| refusal(format!("entity {entity} is not placed"), None))?;
                (point, id.as_proto_u32())
            }
            (None, None) => return Err("give --entity <id> or --at x y z".to_owned()),
        };
        let (reply, rx) = RpcResultSender::channel();
        job.0 = Some(Sweep {
            reply,
            stage: Stage::Aiming {
                elapsed: 0.0,
                last_error: None,
                steady: 0,
            },
            target,
            entity,
            scene,
            binding,
            button: command.button.clone(),
            rate: Vec2::new(command.dx, command.dy) * std::f32::consts::PI / 180.0,
            seconds,
            aim_error: 0.0,
            start: Vec2::ZERO,
            pressed: Value::Null,
        });
        Ok(rx)
    })();
    match result {
        Ok(rx) => pending.push_receiver(rx, |r| r, input.take_responder()),
        Err(e) => input.reply_failed(e),
    }
}

/// What the pointer ray hits now: `{"crdtEntityId", "hitPoint"}`, or `null`.
fn hit_now(
    pointer: &PointerTarget,
    ids: &Query<(Option<&SceneEntity>, Option<&ContainerEntity>)>,
) -> Value {
    let Some(info) = pointer.0.as_ref() else {
        return Value::Null;
    };
    json!({
        "crdtEntityId": hit_ids(info.container, ids).and_then(|(entity, ..)| entity.as_proto_u32()),
        "hitPoint": info.position.map(Point::from_bevy),
    })
}

/// Follows the sweep each frame, before `update_camera`: aims the camera at the target, parks the
/// cursor at the centre and presses, turns the camera while the button is held, and releases.
#[allow(clippy::too_many_arguments)]
fn drive_sweep(
    mut job: ResMut<SweepJob>,
    mut look: ResMut<SyntheticLook>,
    mut script: ResMut<InputScript>,
    time: Res<Time>,
    frame: Res<FrameCount>,
    pointer: Res<PointerTarget>,
    ids: Query<(Option<&SceneEntity>, Option<&ContainerEntity>)>,
    scenes: Query<&RendererSceneContext>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Transform, &PrimaryCamera)>,
) {
    let Some(sweep) = job.0.as_mut() else {
        return;
    };
    let Ok((transform, camera)) = camera.single() else {
        if let Some(sweep) = job.0.take() {
            sweep.reply.send(Err(refusal("no camera", None)));
        }
        return;
    };
    let dt = time.delta_secs();
    let tick = scenes.get(sweep.scene).map(|ctx| ctx.tick_number).ok();
    let stamp = |hit: Value| json!({ "frame": frame.0, "tick": tick, "hit": hit });
    let mut answer = None;
    sweep.stage = match sweep.stage {
        Stage::Aiming {
            elapsed,
            last_error,
            steady,
        } => {
            let to_point = sweep.target - transform.translation;
            let error = aim_error_degrees(transform.forward().as_vec3(), to_point);
            let steady = match last_error {
                Some(last) if (error - last).abs() < STEADY_DEGREES => steady + 1,
                _ => 0,
            };
            let elapsed = elapsed + dt;
            if steady >= STEADY_FRAMES || elapsed >= AIM_SECONDS {
                sweep.aim_error = error;
                let centre = windows
                    .single()
                    .map(|w| Vec2::new(w.width(), w.height()) / 2.0)
                    .unwrap_or_default();
                script.push(Step::Cursor(centre));
                match binding_step(&sweep.binding, ButtonState::Pressed) {
                    Some(press) => Stage::Pressing {
                        step: script.push(press),
                    },
                    None => {
                        answer = Some(Err(refusal("the button has no key or mouse binding", None)));
                        sweep.stage
                    }
                }
            } else {
                let (yaw, pitch) = aim_angles(to_point);
                look.yaw = -wrap(yaw - camera.yaw);
                look.pitch = pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT) - camera.pitch;
                Stage::Aiming {
                    elapsed,
                    last_error: Some(error),
                    steady,
                }
            }
        }
        // the press is played in this frame's `PreUpdate`, and the ray has seen it by now
        Stage::Pressing { step } if script.played() >= step => {
            sweep.pressed = stamp(hit_now(&pointer, &ids));
            sweep.start = Vec2::new(camera.yaw, camera.pitch);
            Stage::Holding { elapsed: 0.0 }
        }
        Stage::Holding { elapsed } => {
            let next = (elapsed + dt).min(sweep.seconds);
            let step = sweep.rate * (next - elapsed);
            look.yaw += step.x;
            look.pitch += step.y;
            if next >= sweep.seconds {
                match binding_step(&sweep.binding, ButtonState::Released) {
                    Some(release) => Stage::Releasing {
                        step: script.push(release),
                    },
                    None => Stage::Holding { elapsed: next },
                }
            } else {
                Stage::Holding { elapsed: next }
            }
        }
        Stage::Releasing { step } if script.played() >= step => {
            let released = stamp(hit_now(&pointer, &ids));
            // turned right is a falling yaw
            let swept = Vec2::new(sweep.start.x - camera.yaw, camera.pitch - sweep.start.y);
            answer = Some(Ok(sweep_reply(sweep, released, swept, transform)));
            sweep.stage
        }
        stage => stage,
    };
    if let Some(answer) = answer {
        if let Some(sweep) = job.0.take() {
            sweep.reply.send(answer);
        }
    }
}

fn sweep_reply(sweep: &Sweep, released: Value, swept: Vec2, transform: &Transform) -> String {
    let mut reply = json!({
        "button": sweep.button,
        "deltaX": reply::round(sweep.rate.x.to_degrees()),
        "deltaY": reply::round(sweep.rate.y.to_degrees()),
        "units": "degrees per second",
        "seconds": reply::round(sweep.seconds),
        "aimErrorDegrees": reply::round(sweep.aim_error),
        "pressed": sweep.pressed,
        "swept": {
            "x": reply::round(swept.x.to_degrees()),
            "y": reply::round(swept.y.to_degrees()),
        },
        "released": released,
        "cameraRotationEuler": dcl_euler(transform.rotation),
    });
    let pressed_on = sweep.pressed["hit"]["crdtEntityId"].as_u64();
    let warning = match (sweep.entity, pressed_on) {
        (Some(asked), Some(hit)) if u64::from(asked) != hit => Some(format!(
            "the press landed on entity {hit}, not {asked}: something nearer covers it"
        )),
        (Some(asked), None) => Some(format!(
            "the press landed on no entity of the scene, not {asked}"
        )),
        _ => None,
    };
    reply::add_warning(&mut reply, warning);
    reply.to_string()
}
