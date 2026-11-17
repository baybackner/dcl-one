//! `/ui_hover`: park the pointer on a scene-UI element by its `crdtId`, as a cursor resting on it
//! would, and leave it there across commands until `/ui_hover --leave`, the next `/ui_hover` or
//! another gesture ends it. The park is an open `AimAt::Ui` aim, so only the addressed element is
//! entered (a real cursor would enter its ancestors too). A gesture that arrives while a hover is
//! parked is not taken (the console hands it back next frame) until the hover's leave has been
//! sent to the scene and drained; its reply then carries `hoverEnded`, so the leave and the
//! gesture's own enter reach the scene in separate ticks.

use bevy::{diagnostic::FrameCount, ecs::system::SystemParam, prelude::*, window::PrimaryWindow};
use bevy_console::{ConsoleCommand, ConsoleCommandEntered};
use common::{
    rpc::{RpcResultReceiver, RpcResultSender},
    sets::SceneSets,
};
use console::{
    reply::{self, Screen},
    DoAddConsoleCommand, PendingConsoleResponses,
};
use dcl_component::{proto_components::sdk::components::common::PointerEventType, SceneEntityId};
use scene_inspector::SceneResolver;
use scene_runner::{
    update_scene::synthetic_pointer::{
        AimAt, Hold, PointerEventLog, SyntheticAim, SyntheticPointer,
    },
    update_world::scene_ui::ui_automation::UiAutomation,
};
use serde_json::{json, Value};

use super::{
    pointer::{
        ui::{element_fields, merge},
        PointerJob,
    },
    sweep::SweepJob,
    ui_drag::UiDragJob,
};

/// Frames to wait for the scene to be sent an enter or leave once it is written.
const DRAIN_FRAMES: u32 = 180;
/// An event written at tick `t` is with the scene once its tick reaches `t + 2`.
const DRAINED_AFTER: u32 = 2;
/// Frames an enter or leave may take to be written before it counts as not sent.
const WRITE_FRAMES: u32 = 30;
/// Frames the pointer pipeline needs to run after the aim changes.
const SETTLE_FRAMES: u32 = 2;
/// Frames after a hand-over in which the gesture that waited for it claims its `hoverEnded`.
const CLAIM_FRAMES: u32 = 3;
/// The gestures that end a parked hover before they start.
const GESTURES: [&str; 8] = [
    "ui_click",
    "ui_drag",
    "ui_set_text",
    "ui_scroll",
    "click_entity",
    "hover_entity",
    "press_input",
    "sweep_pointer",
];

pub(crate) struct AutomationUiHoverPlugin;

impl Plugin for AutomationUiHoverPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiHover>()
            .add_console_command::<UiHoverCommand, _>(ui_hover_cmd)
            .add_systems(
                Update,
                (watch_gestures, drive_ui_hover)
                    .chain()
                    .after(SceneSets::Input),
            );
    }
}

/// Hover a scene-UI element by crdtId (as /ui_list lists it) and keep it hovered across commands
/// until --leave, the next /ui_hover or another gesture (/ui_click, /ui_drag, /ui_set_text,
/// /ui_scroll, /click_entity, /hover_entity, /press_input, /sweep_pointer) ends it; that gesture's
/// reply carries `hoverEnded`. Refused when another element covers the centre (unless --force).
/// Replies `hovering`, `delivered` {enter}, the element's screenRect, center and screen
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/ui_hover")]
pub struct UiHoverCommand {
    /// the element's crdtId
    crdt_id: Option<u32>,
    /// whose UI: `sdk`, the scene's (the default)
    #[arg(long)]
    stack: Option<String>,
    /// end the parked hover; takes no crdtId or --stack
    #[arg(long)]
    leave: bool,
    /// hover even when another element covers the centre
    #[arg(long)]
    force: bool,
    /// refused: a device hover on scene UI is not offered
    #[arg(long)]
    device: bool,
}

/// The parked hover, the hand-over ending it, and the `/ui_hover` in flight.
#[derive(Resource, Default)]
pub struct UiHover {
    parked: Option<Parked>,
    ending: Option<Ending>,
    /// a gesture waits for the parked hover to end
    end_requested: bool,
    /// what the last hand-over ended, and the frame it ended on, for the gesture that waited
    ended: Option<(String, u32)>,
    job: Option<HoverJob>,
}

#[derive(Clone)]
struct Parked {
    scene: Entity,
    id: SceneEntityId,
    /// the element's scene entity
    target: Entity,
    declared: Vec<String>,
}

impl Parked {
    fn address(&self) -> String {
        format!("sdk crdtId {}", self.id.as_proto_u32().unwrap_or_default())
    }

    fn at(&self) -> AimAt {
        AimAt::Ui {
            entity: self.id,
            target: self.target,
        }
    }

    fn declares(&self, event: PointerEventType) -> bool {
        self.declared.contains(&format!("{event:?}"))
    }
}

struct Ending {
    parked: Parked,
    started: u32,
}

struct HoverJob {
    stage: Stage,
    /// `None` for --leave
    crdt_id: Option<u32>,
    force: bool,
    hover_ended: Option<String>,
    respond: RpcResultSender<Result<String, String>>,
}

enum Stage {
    /// the hover parked before this one is ending
    AwaitEnding,
    Start,
    /// the aim is on the element; waiting for the scene to be sent its enter
    Enter {
        parked: Parked,
        element: Value,
        info: Option<String>,
        started: u32,
    },
}

/// What a gesture command uses to wait for a parked hover to end and to claim what ended.
#[derive(SystemParam)]
pub(crate) struct HoverHandover<'w> {
    hover: ResMut<'w, UiHover>,
    frame: Res<'w, FrameCount>,
}

impl HoverHandover<'_> {
    /// Whether a gesture must not start yet: a hover is parked, being ended, or being set. A
    /// command that does not take its invocation gets it again next frame.
    pub(crate) fn defers(&self) -> bool {
        self.hover.parked.is_some() || self.hover.ending.is_some() || self.hover.job.is_some()
    }

    /// The hover the hand-over just ended, for the gesture's reply.
    pub(crate) fn claim(&mut self) -> Option<String> {
        let frame = self.frame.0;
        self.hover
            .ended
            .take()
            .filter(|(_, at)| frame <= at + CLAIM_FRAMES)
            .map(|(text, _)| text)
    }
}

/// Adds `hoverEnded` to a gesture's reply: a field of a JSON reply, a last line of a text one.
pub(crate) fn with_hover_ended(
    reply: Result<String, String>,
    ended: Option<&str>,
) -> Result<String, String> {
    let Some(ended) = ended else {
        return reply;
    };
    let add = |text: String| match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(mut fields)) => {
            fields.insert("hoverEnded".to_owned(), json!(ended));
            Value::Object(fields).to_string()
        }
        _ => format!("{text}\nhoverEnded: {ended}"),
    };
    reply.map(add).map_err(add)
}

/// Asks for the parked hover to end when a gesture arrives.
fn watch_gestures(mut entered: EventReader<ConsoleCommandEntered>, mut hover: ResMut<UiHover>) {
    let gesture = entered
        .read()
        .any(|command| GESTURES.contains(&command.command_name.trim_start_matches('/')));
    if gesture && hover.parked.is_some() {
        hover.end_requested = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn ui_hover_cmd(
    mut input: ConsoleCommand<UiHoverCommand>,
    mut hover: ResMut<UiHover>,
    pointer_job: Res<PointerJob>,
    drag: Res<UiDragJob>,
    sweep: Res<SweepJob>,
    synthetic: Res<SyntheticPointer>,
    log: Res<PointerEventLog>,
    mut pending: ResMut<PendingConsoleResponses>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    // refusals that leave a parked hover in place
    let started = (|| -> Result<RpcResultReceiver<Result<String, String>>, String> {
        if command.device {
            return Err(reply::refusal(
                "--device applies only to client UI: a device hover on scene UI is not offered, \
                 because the real mouse shares its pointer and would end it at any moment. Omit \
                 --device to hover scene UI (the semantic path parks and drains the enter).",
                None,
            ));
        }
        if command.leave {
            let address = match (&command.stack, command.crdt_id) {
                (Some(_), _) => Some("--stack"),
                (None, Some(_)) => Some("the crdtId"),
                (None, None) => None,
            };
            if let Some(address) = address {
                return Err(reply::refusal(
                    format!(
                        "--leave ends whichever hover is parked and takes no address; drop \
                         {address}."
                    ),
                    None,
                ));
            }
        } else if command.crdt_id.is_none() {
            return Err("expected a crdtId, or --leave".to_owned());
        }
        if let Some(stack) = command
            .stack
            .as_ref()
            .filter(|s| !s.eq_ignore_ascii_case("sdk"))
        {
            return Err(reply::refusal(
                format!(
                    "/ui_hover hovers the scene's UI only (--stack sdk); the client's own UI is \
                     driven with the /hud_ commands, not --stack {stack}"
                ),
                None,
            ));
        }
        if hover.job.is_some() {
            return Err(reply::refusal("a /ui_hover is still running", None));
        }
        if hover.ending.is_some() {
            return Err(reply::refusal(
                "the parked hover is being ended for another command; send /ui_hover again once \
                 it replies",
                None,
            ));
        }
        if let Some(busy) = pointer_job.busy() {
            return Err(reply::refusal(busy, None));
        }
        let parked_aim = hover.parked.as_ref().map(Parked::at);
        let foreign_aim = synthetic
            .0
            .as_ref()
            .is_some_and(|aim| Some(aim.at) != parked_aim);
        if drag.busy() || sweep.busy() || foreign_aim || log.unaimed {
            return Err(reply::refusal(
                "a pointer command is running; wait for its reply",
                None,
            ));
        }
        let (respond, rx) = RpcResultSender::channel();
        hover.job = Some(HoverJob {
            stage: if hover.parked.is_some() {
                Stage::AwaitEnding
            } else {
                Stage::Start
            },
            crdt_id: command.crdt_id.filter(|_| !command.leave),
            force: command.force,
            hover_ended: None,
            respond,
        });
        Ok(rx)
    })();
    match started {
        Ok(rx) => pending.push_receiver(rx, |r| r, input.take_responder()),
        Err(e) => input.reply_failed(e),
    }
}

#[allow(clippy::too_many_arguments)]
fn drive_ui_hover(
    mut hover: ResMut<UiHover>,
    frame: Res<FrameCount>,
    resolver: SceneResolver,
    ui: UiAutomation,
    mut synthetic: ResMut<SyntheticPointer>,
    mut log: ResMut<PointerEventLog>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let hover = &mut *hover;
    let frame = frame.0;
    let tick = |scene: Entity| {
        resolver
            .scenes
            .get(scene)
            .ok()
            .map(|(_, ctx)| ctx.tick_number)
    };

    // a parked hover ends when a gesture or a new /ui_hover waits for it
    let awaited = matches!(
        hover.job,
        Some(HoverJob {
            stage: Stage::AwaitEnding,
            ..
        })
    );
    if hover.ending.is_none() && (hover.end_requested || awaited) {
        if let Some(parked) = hover.parked.take() {
            let ours = synthetic
                .0
                .as_ref()
                .is_some_and(|aim| aim.at == parked.at());
            let live = resolver
                .scenes
                .get(parked.scene)
                .is_ok_and(|(_, ctx)| ctx.bevy_entity(parked.id) == Some(parked.target));
            if ours {
                synthetic.0 = None;
            }
            log.events.clear();
            if !ours {
                finish_ending(
                    hover,
                    format!("{} (the aim had already been ended)", parked.address()),
                    frame,
                );
            } else if !live {
                finish_ending(
                    hover,
                    format!(
                        "{} (the element is gone; no leave was sent)",
                        parked.address()
                    ),
                    frame,
                );
            } else {
                // the pipeline writes the leave once the ray follows the cursor again
                log.unaimed = true;
                hover.ending = Some(Ending {
                    parked,
                    started: frame,
                });
            }
        }
    }
    hover.end_requested = false;

    if let Some(ending) = hover.ending.as_ref() {
        let parked = &ending.parked;
        let leave = logged_tick(&log, parked.id, PointerEventType::PetHoverLeave);
        let now = tick(parked.scene);
        let note = match (leave, now) {
            (_, None) => Some(" (the scene is gone; no leave was sent)"),
            (Some(at), Some(now)) if now >= at + DRAINED_AFTER => Some(""),
            (Some(_), _) if frame >= ending.started + DRAIN_FRAMES => {
                Some(" (its leave was not drained)")
            }
            (Some(_), _) => None,
            (None, _) if !parked.declares(PointerEventType::PetHoverLeave) => (frame
                >= ending.started + SETTLE_FRAMES)
                .then_some(" (it declares no PET_HOVER_LEAVE)"),
            (None, _) => {
                (frame >= ending.started + WRITE_FRAMES).then_some(" (its leave was not sent)")
            }
        };
        if let Some(note) = note {
            let text = format!("{}{note}", parked.address());
            hover.ending = None;
            log.unaimed = false;
            log.events.clear();
            finish_ending(hover, text, frame);
        }
    }

    let Some(job) = hover.job.as_mut() else {
        // a parked hover logs nothing a later command reads
        if hover.parked.is_some() {
            log.events.clear();
        }
        return;
    };
    match &job.stage {
        Stage::AwaitEnding => {
            if hover.ending.is_none() && hover.parked.is_none() {
                job.stage = Stage::Start;
            }
        }
        Stage::Start => {
            let Some(crdt_id) = job.crdt_id else {
                // --leave
                let job = hover.job.take().unwrap();
                let reply = match job.hover_ended {
                    Some(left) => json!({ "ok": true, "left": left }),
                    None => json!({
                        "ok": true,
                        "left": null,
                        "info": "no hover was parked, so nothing was left",
                    }),
                };
                job.respond.send(Ok(reply.to_string()));
                return;
            };
            match start_hover(crdt_id, job.force, &resolver, &ui, &windows) {
                Ok((parked, element, info)) => {
                    synthetic.0 = Some(SyntheticAim {
                        scene: parked.scene,
                        at: parked.at(),
                        hold: Hold::Open,
                        started_frame: frame,
                    });
                    log.events.clear();
                    job.stage = Stage::Enter {
                        parked,
                        element,
                        info,
                        started: frame,
                    };
                }
                Err(refusal) => {
                    let job = hover.job.take().unwrap();
                    let reply = with_hover_ended(Err(refusal), job.hover_ended.as_deref());
                    job.respond.send(reply);
                }
            }
        }
        Stage::Enter {
            parked, started, ..
        } => {
            let enter = logged_tick(&log, parked.id, PointerEventType::PetHoverEnter);
            let now = tick(parked.scene);
            let verdict = match (enter, now) {
                (_, None) => Some("not_sent"),
                (Some(at), Some(now)) if now >= at + DRAINED_AFTER => Some("consumed"),
                (Some(_), _) if frame >= started + DRAIN_FRAMES => Some("not_drained"),
                (Some(_), _) => None,
                (None, _) if !parked.declares(PointerEventType::PetHoverEnter) => {
                    (frame >= started + SETTLE_FRAMES).then_some("not_declared")
                }
                (None, _) => (frame >= started + WRITE_FRAMES).then_some("not_sent"),
            };
            let Some(verdict) = verdict else {
                return;
            };
            let job = hover.job.take().unwrap();
            let Stage::Enter {
                parked,
                element,
                info,
                ..
            } = job.stage
            else {
                unreachable!()
            };
            log.events.clear();
            let (ok, note) = hover_outcome(verdict, info);
            let mut reply = json!({
                "ok": ok,
                "hovering": if ok { json!(parked.address()) } else { Value::Null },
                "delivered": { "enter": verdict },
            });
            merge(&mut reply, &element);
            if ok {
                hover.parked = Some(parked);
                if let Some(note) = note {
                    reply["info"] = json!(note);
                }
            } else {
                synthetic.0 = None;
                reply["reason"] = json!(note);
            }
            let reply = if ok {
                Ok(reply.to_string())
            } else {
                Err(reply.to_string())
            };
            job.respond
                .send(with_hover_ended(reply, job.hover_ended.as_deref()));
        }
    }
}

/// The hand-over is done: what it ended goes to the `/ui_hover` waiting for it, else to the
/// gesture that asked.
fn finish_ending(hover: &mut UiHover, text: String, frame: u32) {
    match hover.job.as_mut() {
        Some(job) if matches!(job.stage, Stage::AwaitEnding) => job.hover_ended = Some(text),
        _ => hover.ended = Some((text, frame)),
    }
}

/// The latest tick an event of `event` for the element was written at, while the log records.
fn logged_tick(log: &PointerEventLog, id: SceneEntityId, event: PointerEventType) -> Option<u32> {
    log.events
        .iter()
        .filter(|logged| logged.entity == id && logged.event == event)
        .map(|logged| logged.tick)
        .max()
}

/// The element to park on, its reply fields and an `info` line; or the refusal.
fn start_hover(
    crdt_id: u32,
    force: bool,
    resolver: &SceneResolver,
    ui: &UiAutomation,
    windows: &Query<&Window, With<PrimaryWindow>>,
) -> Result<(Parked, Value, Option<String>), String> {
    let screen = Screen::primary(windows).ok_or("no window")?;
    let (scene, ctx) = resolver.resolve()?;
    let Some(found) = ui
        .elements(scene, true)
        .into_iter()
        .find(|element| element.crdt_id() == crdt_id)
    else {
        return Err(reply::refusal(
            format!(
                "crdtId {crdt_id} is no scene-UI element shown on screen in {} (see /ui_list)",
                ctx.title
            ),
            None,
        ));
    };
    let element = element_fields(&found, &screen);
    let info = match (&found.covered_by, force) {
        (Some(cover), false) => {
            let mut refusal: Value = serde_json::from_str(&reply::refusal(
                "another element covers the target at its center",
                Some(json!({ "blockedBy": cover.blocked_by(), "coveredBy": element["coveredBy"] })),
            ))
            .unwrap_or_default();
            merge(&mut refusal, &element);
            return Err(refusal.to_string());
        }
        (Some(cover), true) => Some(format!(
            "--force hovered through {}, which covers the element's center",
            cover.blocked_by()
        )),
        (None, _) => None,
    };
    Ok((
        Parked {
            scene,
            id: found.id,
            target: found.entity,
            declared: found.event_types,
        },
        element,
        info,
    ))
}

/// Whether the hover parked, and the line that says so: an `info` when it did (or `None`), the
/// `reason` when it did not.
fn hover_outcome(enter: &str, info: Option<String>) -> (bool, Option<String>) {
    match enter {
        "consumed" => (true, info),
        "not_declared" => (
            true,
            Some(
                info.map_or(String::new(), |info| format!("{info}; "))
                    + "the element declares no PET_HOVER_ENTER (no onMouseEnter), so the scene \
                       was told nothing; the pointer rests on it until the hover ends",
            ),
        ),
        other => (
            false,
            Some(format!(
                "the scene was not sent the enter ({other}); nothing is parked"
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_ended_joins_a_json_reply_and_follows_a_text_one() {
        let json = with_hover_ended(Ok(r#"{"ok":true}"#.to_owned()), Some("sdk crdtId 562"));
        let json: Value = serde_json::from_str(&json.unwrap()).unwrap();
        assert_eq!(json["hoverEnded"], "sdk crdtId 562");
        let text = with_hover_ended(Err("no press is held".to_owned()), Some("sdk crdtId 562"));
        assert_eq!(
            text.unwrap_err(),
            "no press is held\nhoverEnded: sdk crdtId 562"
        );
        assert_eq!(
            with_hover_ended(Ok("x".to_owned()), None),
            Ok("x".to_owned())
        );
    }

    #[test]
    fn a_hover_parks_unless_its_enter_was_lost() {
        assert_eq!(hover_outcome("consumed", None), (true, None));
        let (ok, info) = hover_outcome("not_declared", None);
        assert!(ok);
        assert!(info.unwrap().contains("PET_HOVER_ENTER"));
        let (ok, reason) = hover_outcome("not_drained", None);
        assert!(!ok);
        assert!(reason.unwrap().contains("not_drained"));
    }

    #[test]
    fn a_gesture_waits_while_a_hover_is_parked_and_claims_what_ended() {
        let mut app = App::new();
        app.init_resource::<FrameCount>().init_resource::<UiHover>();
        let mut state = bevy::ecs::system::SystemState::<HoverHandover>::new(app.world_mut());
        assert!(!state.get_mut(app.world_mut()).defers());
        let ended = || Some(("sdk crdtId 562".to_owned(), 0));
        app.world_mut().resource_mut::<UiHover>().ended = ended();
        app.world_mut().resource_mut::<FrameCount>().0 = 2;
        let mut handover = state.get_mut(app.world_mut());
        assert_eq!(handover.claim().as_deref(), Some("sdk crdtId 562"));
        assert_eq!(handover.claim(), None);
        // a stale hand-over is not claimed by an unrelated gesture
        app.world_mut().resource_mut::<UiHover>().ended = ended();
        app.world_mut().resource_mut::<FrameCount>().0 = 100;
        assert_eq!(state.get_mut(app.world_mut()).claim(), None);
    }
}
