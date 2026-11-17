//! `/ui_drag`: press at one screen point, move to another over frames, release there. On the
//! `sdk` path an aim holds the pointer on the scene-UI element under each point of the path, as a
//! cursor moving over the scene's UI would, and the button's binding is pressed and released; on
//! the `device` path the cursor itself moves and the mouse button is pressed, so whatever is drawn
//! there takes the drag. Either way the command waits for the scene to be sent the press and the
//! release, then says what each end was over and what the scene consumed.

use bevy::{diagnostic::FrameCount, input::ButtonState, prelude::*, window::PrimaryWindow};
use bevy_console::ConsoleCommand;
use common::{
    inputs::{CommonInputAction, InputMap},
    rpc::RpcResultSender,
    sets::SceneSets,
};
use console::{
    reply::{self, Screen},
    DoAddConsoleCommand, PendingConsoleResponses,
};
use dcl_component::{proto_components::sdk::components::common::PointerEventType, SceneEntityId};
use scene_inspector::SceneResolver;
use scene_runner::{
    renderer_context::RendererSceneContext,
    update_scene::synthetic_pointer::{
        AimAt, Hold, LoggedPointerEvent, PointerEventLog, SyntheticAim, SyntheticPointer,
    },
    update_world::scene_ui::ui_automation::{Cover, UiAutomation},
};
use serde_json::{json, Value};

use super::{
    input::{InputScript, Step},
    pointer::{binding_error, binding_step, PointerJob},
    ui_hover::{with_hover_ended, HoverHandover},
};

/// Frames the path takes without --duration-frames.
const DEFAULT_FRAMES: u32 = 20;
/// Frames the pointer rests on the start before the press, so the element sees it arrive.
const SETTLE_FRAMES: u32 = 2;
/// Frames to wait for the scene to be sent the release.
const DRAIN_FRAMES: u32 = 180;
/// An event written at tick `t` is with the scene once its tick reaches `t + 2`.
const DRAINED_AFTER: u32 = 2;

pub(crate) struct AutomationUiDragPlugin;

impl Plugin for AutomationUiDragPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiDragJob>()
            .add_console_command::<UiDragCommand, _>(ui_drag_cmd)
            .add_systems(Update, drive_ui_drag.after(SceneSets::Input));
    }
}

/// Drag from one point to another, normalized (0 to 1 from the top left): pressed at --from,
/// moved over --duration-frames frames (20), released at --to. --path sdk holds the pointer on
/// the scene's UI under each point (refused when --from is not on it), device moves the cursor
/// and presses the mouse, auto (default) is sdk over the scene's UI and device elsewhere. Replies
/// `ok`, `path`, `pointerOver` {start, end}, `delivered` {down, up}, `entered`, `left`, `events`
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/ui_drag")]
pub struct UiDragCommand {
    /// where the press is: x y, 0 to 1 of the window from the top left
    #[arg(long, num_args = 2, required = true)]
    from: Vec<f32>,
    /// where the release is: x y, 0 to 1 of the window from the top left
    #[arg(long, num_args = 2, required = true)]
    to: Vec<f32>,
    /// frames the move from --from to --to takes
    #[arg(long, default_value_t = DEFAULT_FRAMES)]
    duration_frames: u32,
    /// auto, sdk or device
    #[arg(long, default_value = "auto")]
    path: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Path {
    Sdk,
    Device,
}

#[derive(Debug, PartialEq)]
enum Stage {
    /// sdk: the aim rests on the start until this frame, then presses
    Settle(u32),
    /// waiting for the script to play this step (the press on sdk, the release on device)
    Played(u64),
    /// sdk: the aim is at this step of the path
    Move(u32),
    /// waiting for the scene to be sent the events, until this frame
    Drain(u32),
}

/// The drag in flight.
#[derive(Resource, Default)]
pub struct UiDragJob(Option<UiDrag>);

impl UiDragJob {
    pub(crate) fn busy(&self) -> bool {
        self.0.is_some()
    }
}

pub struct UiDrag {
    path: Path,
    scene: Entity,
    from: Vec2,
    to: Vec2,
    frames: u32,
    scale: f32,
    stage: Stage,
    /// sdk: the script step that releases
    release: Option<Step>,
    /// what each end was over, and the events each declares
    start: End,
    end: End,
    reply: Value,
    respond: RpcResultSender<Result<String, String>>,
}

struct End {
    over: Option<Cover>,
    declared: Vec<String>,
}

impl End {
    fn label(&self) -> String {
        self.over
            .as_ref()
            .map_or_else(|| "world".to_owned(), Cover::blocked_by)
    }

    fn crdt_id(&self) -> Option<u32> {
        match self.over {
            Some(Cover::Scene { crdt_id, .. }) => Some(crdt_id),
            _ => None,
        }
    }
}

fn aim_at(cover: &Cover, frame: u32) -> Option<SyntheticAim> {
    match *cover {
        Cover::Scene {
            scene, entity, id, ..
        } => Some(SyntheticAim {
            scene,
            at: AimAt::Ui {
                entity: id,
                target: entity,
            },
            hold: Hold::Open,
            started_frame: frame,
        }),
        Cover::Client(_) => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ui_drag_cmd(
    mut input: ConsoleCommand<UiDragCommand>,
    resolver: SceneResolver,
    ui: UiAutomation,
    mut job: ResMut<UiDragJob>,
    pointer_job: Res<PointerJob>,
    mut synthetic: ResMut<SyntheticPointer>,
    mut log: ResMut<PointerEventLog>,
    mut script: ResMut<InputScript>,
    input_map: Res<InputMap>,
    frame: Res<FrameCount>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut pending: ResMut<PendingConsoleResponses>,
    mut handover: HoverHandover,
) {
    if handover.defers() {
        return;
    }
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let ended = handover.claim();
    let started = (|| {
        if job.0.is_some() {
            return Err(reply::refusal("a /ui_drag is still running", None));
        }
        if let Some(busy) = pointer_job.busy() {
            return Err(reply::refusal(busy, None));
        }
        if synthetic.0.is_some() || log.unaimed {
            return Err(reply::refusal(
                "a pointer aim is active; wait for its command's reply or send /hover_entity \
                 --clear",
                None,
            ));
        }
        let screen = Screen::primary(&windows).ok_or("no window")?;
        let from = reply::normalized_point(command.from[0], command.from[1])?;
        let to = reply::normalized_point(command.to[0], command.to[1])?;
        if command.duration_frames == 0 {
            return Err("--duration-frames takes 1 or more".to_owned());
        }
        let (scene, _) = resolver.resolve()?;
        let (from, to) = (screen.to_pixels(from), screen.to_pixels(to));
        let declared = |cover: &Option<Cover>| -> Vec<String> {
            let Some(Cover::Scene { crdt_id, scene, .. }) = cover else {
                return Vec::new();
            };
            ui.elements(*scene, true)
                .into_iter()
                .find(|element| element.crdt_id() == *crdt_id)
                .map(|element| element.event_types)
                .unwrap_or_default()
        };
        let start_over = ui.at(from * screen.scale);
        let end_over = ui.at(to * screen.scale);
        let start = End {
            declared: declared(&start_over),
            over: start_over,
        };
        let end = End {
            declared: declared(&end_over),
            over: end_over,
        };
        let on_scene_ui = start.crdt_id().is_some();
        let path = match command.path.as_str() {
            "sdk" if !on_scene_ui => {
                return Err(reply::refusal(
                    format!(
                        "--path sdk drags the scene's UI, and --from is over {}; --path device \
                         drags whatever is there",
                        start.label()
                    ),
                    Some(json!({ "pointerOver": { "start": start.label() } })),
                ))
            }
            "sdk" => Path::Sdk,
            "device" => Path::Device,
            "auto" if on_scene_ui => Path::Sdk,
            "auto" => Path::Device,
            other => return Err(format!("unknown --path `{other}`: use auto, sdk or device")),
        };

        let point = |p: Vec2| {
            let n = screen.to_normalized(p);
            json!({ "x": reply::round_fine(n.x), "y": reply::round_fine(n.y) })
        };
        let reply = json!({
            "path": match path { Path::Sdk => "sdk", Path::Device => "device" },
            "from": point(from),
            "to": point(to),
            "durationFrames": command.duration_frames,
            "pointerOver": { "start": start.label(), "end": end.label() },
            "screen": screen.to_json(),
        });

        log.events.clear();
        let (stage, release) = match path {
            Path::Sdk => {
                let binding = input_map
                    .get_input(CommonInputAction::IaPointer)
                    .ok_or("pointer has no binding")?;
                if let Some(refusal) = binding_error("pointer", &binding) {
                    return Err(refusal);
                }
                synthetic.0 = start.over.as_ref().and_then(|over| aim_at(over, frame.0));
                (
                    Stage::Settle(frame.0 + SETTLE_FRAMES),
                    binding_step(&binding, ButtonState::Released),
                )
            }
            Path::Device => {
                // the cursor rests on the start, presses, moves a step a frame, releases
                log.unaimed = true;
                for _ in 0..=SETTLE_FRAMES {
                    script.push(Step::Cursor(from));
                }
                script.push(Step::Button(MouseButton::Left, ButtonState::Pressed));
                for i in 1..=command.duration_frames {
                    let t = i as f32 / command.duration_frames as f32;
                    script.push(Step::Cursor(from.lerp(to, t)));
                }
                let release = script.push(Step::Button(MouseButton::Left, ButtonState::Released));
                (Stage::Played(release), None)
            }
        };
        let (respond, rx) = RpcResultSender::channel();
        job.0 = Some(UiDrag {
            path,
            scene,
            from,
            to,
            frames: command.duration_frames,
            scale: screen.scale,
            stage,
            release,
            start,
            end,
            reply,
            respond,
        });
        Ok(rx)
    })();
    match started {
        Ok(rx) => pending.push_receiver(
            rx,
            move |r| with_hover_ended(r, ended.as_deref()),
            input.take_responder(),
        ),
        Err(e) => match with_hover_ended(Err(e), ended.as_deref()) {
            Ok(e) | Err(e) => input.reply_failed(e),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn drive_ui_drag(
    mut job: ResMut<UiDragJob>,
    frame: Res<FrameCount>,
    ui: UiAutomation,
    mut synthetic: ResMut<SyntheticPointer>,
    mut log: ResMut<PointerEventLog>,
    mut script: ResMut<InputScript>,
    input_map: Res<InputMap>,
    scenes: Query<&RendererSceneContext>,
) {
    let Some(drag) = job.0.as_mut() else {
        return;
    };
    match drag.stage {
        Stage::Settle(until) => {
            if frame.0 < until {
                return;
            }
            let press = input_map
                .get_input(CommonInputAction::IaPointer)
                .and_then(|binding| binding_step(&binding, ButtonState::Pressed));
            match press {
                Some(press) => drag.stage = Stage::Played(script.push(press)),
                None => drag.stage = Stage::Drain(frame.0),
            }
            return;
        }
        Stage::Played(step) => {
            if script.played() < step {
                return;
            }
            drag.stage = match drag.path {
                // pressed: move along the path
                Path::Sdk if drag.release.is_some() => Stage::Move(1),
                // released
                _ => Stage::Drain(frame.0 + DRAIN_FRAMES),
            };
            return;
        }
        Stage::Move(i) => {
            // the aim follows the scene-UI element under the path; off the scene's UI it stays on
            // the last one
            let t = i as f32 / drag.frames as f32;
            let point = drag.from.lerp(drag.to, t) * drag.scale;
            if let Some(aim) = ui.at(point).and_then(|over| aim_at(&over, frame.0)) {
                if synthetic.0.as_ref().map(|current| current.at) != Some(aim.at) {
                    synthetic.0 = Some(aim);
                }
            }
            if i < drag.frames {
                drag.stage = Stage::Move(i + 1);
            } else {
                let release = drag.release.take().expect("an sdk drag holds its release");
                drag.stage = Stage::Played(script.push(release));
            }
            return;
        }
        Stage::Drain(until) => {
            let tick = scenes.get(drag.scene).map_or(0, |ctx| ctx.tick_number);
            let last = log
                .events
                .iter()
                .filter(|event| in_scene(event, drag.scene))
                .map(|event| event.tick)
                .max();
            if last.is_some_and(|last| tick < last + DRAINED_AFTER) && frame.0 < until {
                return;
            }
        }
    }

    // drained: let go of the pointer and answer
    let drag = job.0.take().unwrap();
    let tick = scenes.get(drag.scene).map_or(0, |ctx| ctx.tick_number);
    // other scenes the player stands in get their own root write, counted in their own ticks
    let mut events = std::mem::take(&mut log.events);
    events.retain(|event| in_scene(event, drag.scene));
    synthetic.0 = None;
    log.unaimed = false;

    let crdt = |id: SceneEntityId| id.as_proto_u32().unwrap_or_default();
    let mut reply = drag.reply;
    let ids = |event: PointerEventType| -> Vec<u32> {
        let mut ids: Vec<u32> = events
            .iter()
            .filter(|logged| logged.event == event && logged.entity != SceneEntityId::ROOT)
            .map(|logged| crdt(logged.entity))
            .collect();
        ids.dedup();
        ids
    };
    reply["entered"] = json!(ids(PointerEventType::PetHoverEnter));
    reply["left"] = json!(ids(PointerEventType::PetHoverLeave));
    reply["events"] = json!(events
        .iter()
        .map(|logged| json!({
            "crdtEntityId": crdt(logged.entity),
            "event": format!("{:?}", logged.event),
            "tick": logged.tick,
        }))
        .collect::<Vec<_>>());
    reply["rootBroadcast"] = json!(events
        .iter()
        .any(|logged| logged.entity == SceneEntityId::ROOT));

    let verdict = |end: &End, event: PointerEventType| -> &'static str {
        let name = format!("{event:?}");
        let Some(id) = end.crdt_id() else {
            return "not_declared";
        };
        if !end.declared.contains(&name) {
            return "not_declared";
        }
        match events
            .iter()
            .filter(|logged| logged.event == event && crdt(logged.entity) == id)
            .map(|logged| logged.tick)
            .max()
        {
            Some(at) if tick >= at + DRAINED_AFTER => "consumed",
            Some(_) => "not_drained",
            None => "not_sent",
        }
    };
    let down = verdict(&drag.start, PointerEventType::PetDown);
    let up = verdict(&drag.end, PointerEventType::PetUp);
    reply["delivered"] = json!({ "down": down, "up": up });

    let world = drag.start.over.is_none() && drag.end.over.is_none();
    let (ok, note) = outcome(world, down, up, &drag.start.label(), &drag.end.label());
    reply["ok"] = json!(ok);
    if ok {
        reply["info"] = json!(note);
        drag.respond.send(Ok(reply.to_string()));
    } else {
        reply["reason"] = json!(note);
        drag.respond.send(Err(reply.to_string()));
    }
}

/// Whether an event is the drag's scene's: an element's, or its own root's (a root write's
/// `container` is its scene).
fn in_scene(event: &LoggedPointerEvent, scene: Entity) -> bool {
    event.entity != SceneEntityId::ROOT || event.container == scene
}

/// Whether a drag did what it was for, and the line that says so: an `info` when it did, the
/// `reason` when it did not.
fn outcome(world: bool, down: &str, up: &str, start: &str, end: &str) -> (bool, String) {
    if world {
        return (
            true,
            "both ends are the world: no UI element received the drag (a drag across the world \
             is /sweep_pointer)"
                .to_owned(),
        );
    }
    match (down, up) {
        ("not_declared", "not_declared") => (
            false,
            format!(
                "neither {start} declares PET_DOWN nor {end} PET_UP, so the drag told the scene \
                 nothing"
            ),
        ),
        (_, up) if up != "consumed" && up != "not_declared" => (
            false,
            format!("the scene did not consume the release ({up})"),
        ),
        (down, _) if down != "consumed" && down != "not_declared" => (
            false,
            format!("the scene did not consume the press ({down})"),
        ),
        _ => (true, format!("pressed on {start}, released on {end}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_succeeds_only_when_the_scene_consumed_what_its_ends_declare() {
        let (a, b) = ("the scene's UI (crdtId 673)", "the scene's UI (crdtId 675)");
        let (ok, info) = outcome(false, "consumed", "consumed", a, b);
        assert!(ok);
        assert_eq!(info, format!("pressed on {a}, released on {b}"));
        assert!(outcome(false, "consumed", "not_declared", a, b).0);
        let (ok, reason) = outcome(false, "consumed", "not_drained", a, b);
        assert!(!ok);
        assert!(reason.contains("did not consume the release"));
        assert!(!outcome(false, "not_sent", "consumed", a, b).0);
        assert!(!outcome(false, "not_declared", "not_declared", a, b).0);
        assert!(outcome(true, "not_declared", "not_declared", "world", "world").0);
    }
}
