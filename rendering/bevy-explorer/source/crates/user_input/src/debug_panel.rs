use std::collections::VecDeque;

use bevy::{diagnostic::FrameCount, prelude::*};
use bevy_console::ConsoleCommand;
use common::structs::{AvatarDynamicState, PrimaryCamera, PrimaryUser};
use console::{AddDebugSection, DebugInput, DebugInputKind, DebugSection, DoAddConsoleCommand};
use dcl_component::proto_components::common::Vector3;
use scene_runner::{renderer_context::RendererSceneContext, SceneUpdates};
use serde_json::{json, Value};

use crate::avatar_movement::{
    ActivePlayerComponent, AvatarLocomotionSettings, AvatarMovement, LastMovementStep,
};

pub(super) fn register(app: &mut App) {
    app.init_resource::<MovementSceneReplies>()
        .add_systems(PostUpdate, sample_movement_scene_replies)
        .add_console_command::<CameraState, _>(camera_state)
        .add_console_command::<MovementState, _>(movement_state)
        .add_console_command::<MovementSceneRepliesCommand, _>(movement_scene_replies)
        .add_preview_console_command::<CameraSensitivity, _>(camera_sensitivity)
        .add_preview_debug_inputs(DebugSection {
            title: "Camera",
            query: "/camera_state",
            fields: &[
                ("Follow distance (m)", "distance"),
                ("Far clip (m)", "far"),
                ("Vertical FOV (degrees)", "fov"),
                ("Scene override", "override"),
            ],
            actions: &[],
            inputs: &[DebugInput {
                label: "Look sensitivity",
                field: "sensitivity",
                command: "/camera_sensitivity {value}",
                kind: DebugInputKind::Number {
                    min: 0.01,
                    max: 100.0,
                    step: 0.01,
                },
            }],
        })
        .add_debug_section(DebugSection {
            title: "Locomotion",
            query: "/movement_state",
            fields: &[
                ("Walk speed", "walk"),
                ("Jog speed", "jog"),
                ("Run speed", "run"),
                ("Jump height", "jump"),
                ("Run jump height", "runJump"),
                ("Double jump height", "doubleJump"),
                ("Hard landing cooldown", "landing"),
                ("Requested velocity (X / Y / Z)", "requested"),
                ("Actual velocity (X / Y / Z)", "actual"),
                ("External velocity (X / Y / Z)", "external"),
            ],
            inputs: &[],
            actions: &[],
        })
        .add_debug_section(DebugSection {
            title: "Movement Scene Replies",
            query: "/movement_scene_replies",
            fields: &[
                ("Movement scene", "scene"),
                ("Frames sampled (last 256)", "frames"),
                ("Frame target (ms)", "frameTarget"),
                ("Frames without a reply from the same frame (%)", "missRate"),
                (
                    "Frames without a reply from the same frame (late reply / not dispatched)",
                    "missed",
                ),
                (
                    "Dispatch after the frame's scene loop start (ms) (min / mean / max)",
                    "sent",
                ),
                (
                    "Dispatch before the frame's deadline (ms) (min / mean / max)",
                    "sentBeforeDeadline",
                ),
                (
                    "Reply in time: before the deadline (ms) (min / mean / max)",
                    "onTime",
                ),
                (
                    "Reply late: after the deadline (ms) (min / mean / max)",
                    "late",
                ),
                ("Reply late: frames to apply (mean / max)", "lateFrames"),
            ],
            inputs: &[],
            actions: &[],
        });
}

/// Frames [`MovementSceneReplies`] keeps.
const REPLY_WINDOW: usize = 256;

/// One frame of the movement scene's ticks, from the scene runner's dispatch and reply times.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct ReplySample {
    /// The scene was dispatched this frame: ms after the frame's scene loop started, ms before
    /// its deadline.
    sent: Option<(f32, f32)>,
    /// A reply was applied this frame: frames since its dispatch, ms after its dispatch frame's
    /// deadline (negative: before it).
    reply: Option<(u32, f32)>,
}

/// The movement scene's last [`REPLY_WINDOW`] frames, sampled whether or not the panel is open.
#[derive(Resource, Default)]
struct MovementSceneReplies {
    scene: Option<Entity>,
    frames: VecDeque<ReplySample>,
}

fn sample_movement_scene_replies(
    player: Query<&ActivePlayerComponent<AvatarMovement>, With<PrimaryUser>>,
    scenes: Query<&RendererSceneContext>,
    frame: Res<FrameCount>,
    mut replies: ResMut<MovementSceneReplies>,
) {
    let scene = player
        .single()
        .ok()
        .map(|active| active.scene())
        .filter(|&e| e != Entity::PLACEHOLDER);
    let Some((scene, context)) = scene.and_then(|e| scenes.get(e).ok().map(|c| (e, c))) else {
        return;
    };
    if replies.scene != Some(scene) {
        replies.scene = Some(scene);
        replies.frames.clear();
    }
    if replies.frames.len() == REPLY_WINDOW {
        replies.frames.pop_front();
    }
    replies.frames.push_back(reply_sample(context, frame.0));
}

fn reply_sample(context: &RendererSceneContext, frame: u32) -> ReplySample {
    let timing = &context.tick_timing;
    ReplySample {
        sent: timing
            .sent
            .filter(|sent| sent.frame == frame)
            .map(|sent| (sent.after_start_ms(), sent.before_deadline_ms())),
        reply: timing
            .reply
            .filter(|reply| reply.frame == frame)
            .map(|reply| (reply.frames(), reply.after_deadline_ms())),
    }
}

/// `[min, mean, max]`, rounded to 0.01, or null without values.
fn spread(values: impl Iterator<Item = f32>) -> Value {
    let (mut n, mut sum, mut min, mut max) = (0, 0.0, f32::INFINITY, f32::NEG_INFINITY);
    for v in values {
        n += 1;
        sum += v;
        min = min.min(v);
        max = max.max(v);
    }
    let r = |v: f32| (f64::from(v) * 100.0).round() / 100.0;
    if n == 0 {
        Value::Null
    } else {
        json!([r(min), r(sum / n as f32), r(max)])
    }
}

/// The panel's figures over the sampled frames. A frame misses when no reply dispatched in it
/// was applied in it: its reply came after the deadline, or the scene was not dispatched (its
/// previous tick was still in flight).
fn summarize_replies(frames: &VecDeque<ReplySample>) -> Value {
    let on_time = |f: &&ReplySample| f.reply.is_some_and(|(n, _)| n == 0);
    let late = frames
        .iter()
        .filter(|f| !on_time(f) && f.sent.is_some())
        .count();
    let not_sent = frames
        .iter()
        .filter(|f| !on_time(f) && f.sent.is_none())
        .count();
    let replies = || frames.iter().filter_map(|f| f.reply);
    let late_frames = spread(replies().filter(|(n, _)| *n > 0).map(|(n, _)| n as f32));
    json!({
        "frames": frames.len(),
        "missRate": (!frames.is_empty())
            .then(|| ((late + not_sent) as f64 * 1000.0 / frames.len() as f64).round() / 10.0),
        "missed": [late, not_sent],
        "sent": spread(frames.iter().filter_map(|f| f.sent).map(|(start, _)| start)),
        "sentBeforeDeadline": spread(frames.iter().filter_map(|f| f.sent).map(|(_, d)| d)),
        "onTime": spread(replies().filter(|(n, _)| *n == 0).map(|(_, ms)| -ms)),
        "late": spread(replies().filter(|(n, _)| *n > 0).map(|(_, ms)| ms)),
        "lateFrames": late_frames.as_array().map(|a| json!([a[1], a[2]])),
    })
}

/// Read when the movement scene's ticks were dispatched in their frame and how their replies
/// met the frame's deadline, over the last 256 frames.
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/movement_scene_replies")]
struct MovementSceneRepliesCommand;

fn movement_scene_replies(
    mut cmd: ConsoleCommand<MovementSceneRepliesCommand>,
    replies: Res<MovementSceneReplies>,
    scenes: Query<&RendererSceneContext>,
    updates: Res<SceneUpdates>,
) {
    let Some(Ok(_)) = cmd.take() else { return };
    let mut report = summarize_replies(&replies.frames);
    let title = replies
        .scene
        .and_then(|e| scenes.get(e).ok())
        .map(|context| context.title.clone());
    report["scene"] = json!(title);
    report["frameTarget"] = json!(updates
        .frame_target
        .map(|d| (d.as_secs_f64() * 100_000.0).round() / 100.0));
    report["unavailable"] = json!({
        "": "No movement scene has controlled the player yet.",
        "frameTarget": "The frame rate is uncapped.",
        "missRate": "No frames sampled.",
        "sent": "The movement scene was not dispatched in the sampled frames.",
        "sentBeforeDeadline": "The movement scene was not dispatched in the sampled frames.",
        "onTime": "No reply arrived in its own frame.",
        "late": "No reply arrived after its frame's deadline.",
        "lateFrames": "No reply arrived after its frame's deadline.",
    });
    cmd.reply_ok(report.to_string());
}

/// Read the player's camera.
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/camera_state")]
struct CameraState;

/// Set look sensitivity for the player's camera in preview.
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/camera_sensitivity")]
struct CameraSensitivity {
    value: f32,
}

fn camera_sensitivity(
    mut cmd: ConsoleCommand<CameraSensitivity>,
    mut camera: Query<&mut PrimaryCamera>,
) {
    let Some(Ok(args)) = cmd.take() else { return };
    if !(0.01..=100.0).contains(&args.value) {
        cmd.reply_failed("sensitivity must be in 0.01..=100");
        return;
    }
    if let Ok(mut camera) = camera.single_mut() {
        camera.sensitivity = args.value;
        cmd.reply_ok(format!("camera sensitivity {}", args.value));
    } else {
        cmd.reply_failed("player camera unavailable");
    }
}

fn camera_state(
    mut cmd: ConsoleCommand<CameraState>,
    camera: Query<(&PrimaryCamera, &Projection)>,
) {
    let Some(Ok(_)) = cmd.take() else { return };
    let Ok((camera, projection)) = camera.single() else {
        cmd.reply_failed("player camera unavailable");
        return;
    };
    let (far, fov) = match projection {
        Projection::Perspective(p) => (Some(p.far), Some(p.fov.to_degrees())),
        _ => (None, None),
    };
    cmd.reply_ok(json!({"sensitivity":camera.sensitivity, "distance":camera.distance, "far":far, "fov":fov, "override":camera.scene_override.is_some()}).to_string());
}

/// Read the active SDK movement settings and last frame of controller feedback.
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/movement_state")]
struct MovementState;

fn movement_state(
    mut cmd: ConsoleCommand<MovementState>,
    active: Query<&ActivePlayerComponent<AvatarLocomotionSettings>, With<PrimaryUser>>,
    player: Query<(&ActivePlayerComponent<AvatarMovement>, &AvatarDynamicState), With<PrimaryUser>>,
    step: Res<LastMovementStep>,
) {
    let Some(Ok(_)) = cmd.take() else { return };
    let loc = active.single().ok().map(|active| &active.component.0);
    let player = player.single().ok();
    let vector = |v: Vector3| [v.x, v.y, v.z];
    // the last completed step: this frame's has not run yet when a command reads
    let requested = step.requested_velocity.or_else(|| {
        player.map(|(movement, _)| Vector3::world_vec_from_vec3(&movement.component.velocity))
    });
    // before the first step, a stationary controller reads as zero
    let actual = step.actual_velocity.or_else(|| {
        player
            .filter(|(_, dynamics)| dynamics.velocity == Vec3::ZERO)
            .map(|(_, dynamics)| Vector3::world_vec_from_vec3(&dynamics.velocity))
    });
    let setting_reason = if loc.is_some() {
        "The active locomotion settings do not specify this value."
    } else {
        "No active locomotion settings for the player."
    };
    cmd.reply_ok(json!({
        "walk":loc.and_then(|s| s.walk_speed), "jog":loc.and_then(|s| s.jog_speed), "run":loc.and_then(|s| s.run_speed),
        "jump":loc.and_then(|s| s.jump_height), "runJump":loc.and_then(|s| s.run_jump_height),
        "doubleJump":loc.and_then(|s| s.double_jump_height), "landing":loc.and_then(|s| s.hard_landing_cooldown),
        "requested":requested.map(vector), "actual":actual.map(vector),
        "external":step.external_velocity.map(vector),
        "unavailable":{
            "walk":setting_reason, "jog":setting_reason, "run":setting_reason, "jump":setting_reason,
            "runJump":setting_reason, "doubleJump":setting_reason, "landing":setting_reason,
            "requested":"No active player movement controller.",
            "actual":"No completed player movement step.",
            "external":"No SDK force or impulse in the last movement step.",
        },
    }).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avatar_movement::{AvatarMovement, AvatarMovementInfo, FromConfig};
    use bevy_console::{ConsoleCommandEntered, PrintConsoleLine};
    use common::structs::AvatarDynamicState;
    use std::sync::{mpsc, Arc};

    #[test]
    fn stationary_controller_reports_its_velocities_when_sdk_feedback_is_omitted() {
        let mut app = App::new();
        app.add_event::<ConsoleCommandEntered>()
            .add_event::<PrintConsoleLine>()
            .init_resource::<LastMovementStep>()
            .add_systems(Update, movement_state);
        app.world_mut().spawn((
            PrimaryUser::default(),
            ActivePlayerComponent::<AvatarMovement>::from_config(&default()),
            AvatarDynamicState::default(),
        ));
        let (tx, rx) = mpsc::channel();
        app.world_mut().send_event(ConsoleCommandEntered {
            command_name: "/movement_state".into(),
            args: vec![],
            responder: Some(Arc::new(move |result| tx.send(result).unwrap())),
        });
        app.update();
        let reply = rx.try_recv().unwrap().unwrap();
        let report: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(report["requested"], json!([0.0, 0.0, 0.0]));
        assert_eq!(report["actual"], json!([0.0, 0.0, 0.0]));
        assert!(report["external"].is_null());
    }

    #[test]
    fn replies_are_summarized_as_misses_dispatch_times_and_lateness() {
        let on_time = ReplySample {
            sent: Some((1.0, 15.0)),
            reply: Some((0, -5.0)),
        };
        // dispatched, its reply 2 ms after the deadline; applied in the next frame, which the
        // scene's tick still in flight kept from dispatching
        let late = ReplySample {
            sent: Some((3.0, 13.0)),
            reply: None,
        };
        let applied_late = ReplySample {
            sent: None,
            reply: Some((1, 2.0)),
        };
        let frames = VecDeque::from([on_time, late, applied_late, on_time]);
        let report = summarize_replies(&frames);
        assert_eq!(report["frames"], 4);
        assert_eq!(report["missRate"], 50.0);
        assert_eq!(report["missed"], json!([1, 1]));
        assert_eq!(report["sent"], json!([1.0, 1.67, 3.0]));
        assert_eq!(report["sentBeforeDeadline"], json!([13.0, 14.33, 15.0]));
        assert_eq!(report["onTime"], json!([5.0, 5.0, 5.0]));
        assert_eq!(report["late"], json!([2.0, 2.0, 2.0]));
        assert_eq!(report["lateFrames"], json!([1.0, 1.0]));

        let report = summarize_replies(&VecDeque::new());
        assert!(report["missRate"].is_null());
        assert!(report["late"].is_null());
        assert!(report["lateFrames"].is_null());
    }

    // The scenes' broadcast clears `AvatarMovementInfo` at the start of each frame, before a
    // command runs: the reply still carries the step that moved the player.
    #[test]
    fn a_moving_step_reports_its_actual_velocity_after_the_next_broadcast() {
        use crate::avatar_movement::{apply_movement, broadcast_movement_info, CentralCollisions};
        use bevy::time::{TimePlugin, TimeUpdateStrategy};
        use common::structs::{EngineMovementControl, SceneDrivenAnimationFeedback};

        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_millis(20),
            ))
            .add_event::<ConsoleCommandEntered>()
            .add_event::<PrintConsoleLine>()
            .init_resource::<AvatarMovementInfo>()
            .init_resource::<LastMovementStep>()
            .init_resource::<CentralCollisions>()
            .init_resource::<SceneDrivenAnimationFeedback>()
            .insert_resource(EngineMovementControl {
                // accept the test's movement, initiated at time zero
                accept_movement_after: -1.0,
                ..default()
            })
            .add_systems(Update, (broadcast_movement_info, movement_state).chain())
            .add_systems(PostUpdate, apply_movement);
        let mut movement = ActivePlayerComponent::<AvatarMovement>::from_config(&default());
        // 2 m/s east, 1 m/s north (SDK axes; bevy's z points south)
        movement.component.velocity = Vec3::new(2.0, 0.0, -1.0);
        app.world_mut().spawn((
            PrimaryUser::default(),
            movement,
            AvatarDynamicState::default(),
            Transform::from_xyz(0.0, 1.0, 0.0),
        ));
        app.update();
        app.update();

        let (tx, rx) = mpsc::channel();
        app.world_mut().send_event(ConsoleCommandEntered {
            command_name: "/movement_state".into(),
            args: vec![],
            responder: Some(Arc::new(move |result| tx.send(result).unwrap())),
        });
        app.update();
        let reply = rx.try_recv().unwrap().unwrap();
        let report: serde_json::Value = serde_json::from_str(&reply).unwrap();
        let actual: Vec<f64> = serde_json::from_value(report["actual"].clone()).unwrap();
        assert!((actual[0] - 2.0).abs() < 1e-3, "{report}");
        assert!(actual[1].abs() < 1e-3, "{report}");
        assert!((actual[2] - 1.0).abs() < 1e-3, "{report}");
        assert_eq!(report["requested"], json!([2.0, 0.0, 1.0]));
    }
}
