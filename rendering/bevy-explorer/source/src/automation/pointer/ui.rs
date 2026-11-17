//! `/ui_click`: click a scene-UI element by its `crdtId`, as a cursor resting on it would (an aim
//! that holds the pointer on the element) or, with `--device`, by parking the cursor at its centre
//! and pressing the real button. A covered or disabled element is refused before anything is
//! pressed; once the click is done the command waits for the scene to drain its events and says,
//! for the enter, the down and the up, whether the scene consumed it.

use bevy::{diagnostic::FrameCount, prelude::*, window::PrimaryWindow};
use bevy_console::ConsoleCommand;
use common::{
    inputs::{CommonInputAction, InputMap},
    rpc::{RpcResultReceiver, RpcResultSender},
};
use console::reply::{self, Screen};
use input_manager::{InputManager, InputPriority};
use scene_runner::{
    renderer_context::RendererSceneContext,
    update_world::scene_ui::ui_automation::{UiAutomation, UiElement},
};
use serde_json::{json, Value};

use super::{
    binding_error, timeout_frames, Answer, ButtonNames, ClickKind, PointerControl, Request,
};
use crate::automation::{input::Step, ui_hover::with_hover_ended};

/// Frames to wait for the scene to drain the click's events after the click is done.
const DRAIN_FRAMES: u32 = 180;
/// The scene has been sent everything written at tick `t` once its tick reaches `t + 2`: a write
/// during an in-flight tick leaves with the next dispatch.
const DRAINED_AFTER: u32 = 2;

const EVENTS: [(&str, &str); 3] = [
    ("enter", "PetHoverEnter"),
    ("down", "PetDown"),
    ("up", "PetUp"),
];

/// Click a scene-UI element by crdtId (as /ui_list lists it); refused when another element covers
/// its centre (unless --force) or it is disabled. Replies `delivered` {enter, down, up}, each
/// consumed, not_declared, not_sent or not_drained, with the element's screenRect, center, screen
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/ui_click")]
pub struct UiClickCommand {
    /// the element's crdtId
    crdt_id: u32,
    /// whose UI: `sdk`, the scene's (the HUD's own is `/hud_click`)
    #[arg(long, default_value = "sdk")]
    stack: String,
    /// click through the cursor: park it at the element's centre and press the real button, so
    /// whatever is drawn there takes the click
    #[arg(long)]
    device: bool,
    /// click even when another element covers the centre: the plain click still reaches the
    /// element; a --device click reaches the cover
    #[arg(long)]
    force: bool,
    /// pointer, primary or secondary
    #[arg(long, default_value = "pointer")]
    button: String,
    /// Seconds the click may take (10 by default), not counting the wait for the scene to drain it
    #[arg(long)]
    timeout_seconds: Option<f32>,
}

/// The `/ui_click` in flight: its click job's answer, then the wait for the scene to drain it.
#[derive(Resource, Default)]
pub struct UiClickJob(Option<UiClick>);

pub struct UiClick {
    click: RpcResultReceiver<Result<String, String>>,
    /// the click job's reply and the scene tick its events are drained at
    clicked: Option<(Value, u32)>,
    deadline: u32,
    scene: Entity,
    element: Value,
    crdt_id: u32,
    declared: Vec<String>,
    device: bool,
    reply: RpcResultSender<Result<String, String>>,
}

#[allow(clippy::too_many_arguments)]
pub fn ui_click_cmd(
    mut input: ConsoleCommand<UiClickCommand>,
    mut control: PointerControl,
    ui: UiAutomation,
    mut jobs: ResMut<UiClickJob>,
    windows: Query<&Window, With<PrimaryWindow>>,
    input_map: Res<InputMap>,
    input_manager: InputManager,
) {
    if control.handover.defers() {
        return;
    }
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let ended = control.handover.claim();
    match ui_click(
        &command,
        &mut control,
        &ui,
        &mut jobs,
        &windows,
        &input_map,
        &input_manager,
    ) {
        Ok(rx) => control.pending.push_receiver(
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
fn ui_click(
    command: &UiClickCommand,
    control: &mut PointerControl,
    ui: &UiAutomation,
    jobs: &mut UiClickJob,
    windows: &Query<&Window, With<PrimaryWindow>>,
    input_map: &InputMap,
    input_manager: &InputManager,
) -> Result<RpcResultReceiver<Result<String, String>>, String> {
    if !command.stack.eq_ignore_ascii_case("sdk") {
        return Err(reply::refusal(
            format!(
                "/ui_click clicks the scene's UI only (--stack sdk); the client's own UI is \
                 clicked with /hud_click, not --stack {}",
                command.stack
            ),
            None,
        ));
    }
    if jobs.0.is_some() {
        return Err(reply::refusal("a /ui_click is still running", None));
    }
    let (action, button) = match command.button.as_str() {
        "pointer" => (CommonInputAction::IaPointer, ("pointer", "IaPointer")),
        "primary" => (CommonInputAction::IaPrimary, ("primary", "IaPrimary")),
        "secondary" => (CommonInputAction::IaSecondary, ("secondary", "IaSecondary")),
        other => {
            return Err(format!(
                "unknown button `{other}`: use pointer, primary or secondary"
            ))
        }
    };
    let timeout = timeout_frames(None, command.timeout_seconds, &control.rate)?;
    let screen = Screen::primary(windows).ok_or("no window")?;
    let (scene, title) = control
        .resolver
        .resolve()
        .map(|(scene, ctx)| (scene, ctx.title.clone()))?;
    let Some(found) = ui
        .elements(scene, true)
        .into_iter()
        .find(|element| element.crdt_id() == command.crdt_id)
    else {
        return Err(reply::refusal(
            format!(
                "crdtId {} is no scene-UI element shown on screen in {title} (see /ui_list)",
                command.crdt_id
            ),
            None,
        ));
    };
    let element = element_fields(&found, &screen);
    let refused = |reason: String, cause: Value| {
        let mut refusal: Value =
            serde_json::from_str(&reply::refusal(reason, Some(cause))).unwrap_or_default();
        merge(&mut refusal, &element);
        refusal.to_string()
    };
    if found.disabled {
        return Err(refused(
            format!("the {} is disabled", found.kind.name()),
            json!({ "crdtId": command.crdt_id, "disabled": true }),
        ));
    }
    if let (Some(cover), false) = (&found.covered_by, command.force) {
        return Err(refused(
            "another element covers the target at its center".to_owned(),
            json!({ "blockedBy": cover.blocked_by(), "coveredBy": element["coveredBy"] }),
        ));
    }

    let Some(binding) = input_map.get_input(action) else {
        return Err(format!("{} has no binding", command.button));
    };
    if let Some(refusal) = binding_error(&command.button, &binding) {
        return Err(refusal);
    }
    let hud_holds = !input_manager.check_priority(&binding, InputPriority::Scene);
    if command.device {
        // the cursor reaches the element's centre before the press, two frames on
        control.script.push(Step::Cursor(found.rect.center()));
    }
    let entity = command.crdt_id.to_string();
    let started = control.start(Request {
        command: "/ui_click",
        entity: (!command.device).then_some(entity.as_str()),
        at: None,
        kind: ClickKind::Click,
        bounds: false,
        timeout,
        binding: Some(binding),
        button: Some(ButtonNames {
            arg: button.0,
            action: button.1,
        }),
        hud_holds,
        unaimed: command.device,
        press: None,
        ui: true,
    });
    let click = match started {
        Answer::Pending(rx) => rx,
        Answer::Failed(e) => return Err(e),
        // a diagnosis: the click never started
        Answer::Json(json) => {
            let mut reply: Value = serde_json::from_str(&json).unwrap_or_default();
            merge(&mut reply, &element);
            return Err(reply.to_string());
        }
    };
    let (reply, rx) = RpcResultSender::channel();
    jobs.0 = Some(UiClick {
        click,
        clicked: None,
        deadline: 0,
        scene,
        element,
        crdt_id: command.crdt_id,
        declared: found.event_types.clone(),
        device: command.device,
        reply,
    });
    Ok(rx)
}

/// The element triple every reply carries, success or failure: `crdtId`, `kind`, `screenRect`,
/// `center`, `screen`, and what covers it.
pub(crate) fn element_fields(element: &UiElement, screen: &Screen) -> Value {
    let listed = element.to_json(screen);
    json!({
        "crdtId": listed["crdtId"],
        "kind": listed["kind"],
        "screenRect": listed["screenRect"],
        "center": listed["center"],
        "coveredBy": listed["coveredBy"],
        "screen": screen.to_json(),
    })
}

pub(crate) fn merge(into: &mut Value, fields: &Value) {
    if let (Some(into), Some(fields)) = (into.as_object_mut(), fields.as_object()) {
        for (key, value) in fields {
            into.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
}

pub fn drive_ui_click(
    mut jobs: ResMut<UiClickJob>,
    frame: Res<FrameCount>,
    scenes: Query<&RendererSceneContext>,
) {
    let Some(job) = jobs.0.as_mut() else {
        return;
    };
    if job.clicked.is_none() {
        match job.click.poll_once() {
            Ok(None) => return,
            Ok(Some(Ok(json))) => {
                let clicked: Value = serde_json::from_str(&json).unwrap_or_default();
                let last = clicked["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|event| event["tick"].as_u64())
                    .max()
                    .unwrap_or_default() as u32;
                job.clicked = Some((clicked, last + DRAINED_AFTER));
                job.deadline = frame.0 + DRAIN_FRAMES;
            }
            Ok(Some(Err(e))) => {
                // a refusal of the click job: say which element it was for
                let mut reply: Value =
                    serde_json::from_str(&e).unwrap_or_else(|_| json!({ "refused": e }));
                merge(&mut reply, &job.element);
                let job = jobs.0.take().unwrap();
                job.reply.send(Err(reply.to_string()));
                return;
            }
            Err(()) => {
                let job = jobs.0.take().unwrap();
                job.reply.send(Err("the click was cancelled".to_owned()));
                return;
            }
        }
    }
    let Some((_, drained_at)) = job.clicked else {
        return;
    };
    let tick = scenes.get(job.scene).map_or(0, |ctx| ctx.tick_number);
    if tick < drained_at && frame.0 < job.deadline {
        return;
    }
    let job = jobs.0.take().unwrap();
    let result = verdict(&job, tick);
    job.reply.send(result);
}

/// The reply: the click's, with what the scene consumed of each event.
fn verdict(job: &UiClick, tick: u32) -> Result<String, String> {
    let (clicked, _) = job.clicked.as_ref().expect("verdict after the click");
    // a device click goes where the cursor's ray goes: its events are this element's only if the
    // press landed on it
    let landed_elsewhere = job.device
        && clicked["crdtEntityId"]
            .as_u64()
            .is_some_and(|hit| hit != u64::from(job.crdt_id));
    let events: Vec<(String, u32)> = clicked["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| {
            Some((
                event["event"].as_str()?.to_owned(),
                event["tick"].as_u64()? as u32,
            ))
        })
        .collect();
    let mut delivered = json!({});
    for (key, event) in EVENTS {
        let logged = events
            .iter()
            .filter(|(name, _)| name == event)
            .map(|(_, at)| *at)
            .max();
        delivered[key] = json!(if !job.declared.iter().any(|declared| declared == event) {
            "not_declared"
        } else {
            match logged {
                Some(_) if landed_elsewhere => "not_sent",
                Some(at) if tick >= at + DRAINED_AFTER => "consumed",
                Some(_) => "not_drained",
                None => "not_sent",
            }
        });
    }
    let down = delivered["down"].as_str().unwrap_or_default();
    let up = delivered["up"].as_str().unwrap_or_default();
    let mut reply = json!({
        "path": if job.device { "device" } else { "sdk" },
        "delivered": delivered,
    });
    merge(&mut reply, &job.element);
    for key in ["hit", "rootBroadcast", "events", "button", "warning"] {
        if !clicked[key].is_null() {
            reply[key] = clicked[key].clone();
        }
    }
    if landed_elsewhere {
        reply["warning"] = json!(format!(
            "the press landed on crdtId {}, not on the element: something drawn over its centre \
             took the cursor's click",
            clicked["crdtEntityId"]
        ));
    }

    let info = match (down, up) {
        ("consumed", "not_declared") => Some(
            "the element declares no PET_UP handler (no onMouseUp), so only the press reached the \
             scene",
        ),
        ("not_declared", "consumed") => Some(
            "the element declares no PET_DOWN handler (no onMouseDown), so only the release \
             reached the scene",
        ),
        _ => None,
    };
    if let Some(info) = info {
        reply["info"] = json!(info);
    }
    let ok = down == "consumed" || up == "consumed";
    reply["ok"] = json!(ok);
    if ok {
        return Ok(reply.to_string());
    }
    reply["reason"] = json!(if down == "not_declared" && up == "not_declared" {
        "the element declares no PET_DOWN or PET_UP handler (no onMouseDown/onMouseUp), so the \
         click told the scene nothing"
            .to_owned()
    } else {
        format!("the scene consumed neither the press ({down}) nor the release ({up})")
    });
    Err(reply.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(declared: &[&str], device: bool, clicked: Value) -> UiClick {
        let (_, click) = RpcResultSender::channel();
        let (reply, _) = RpcResultSender::channel();
        UiClick {
            click,
            clicked: Some((clicked, 0)),
            deadline: 0,
            scene: Entity::PLACEHOLDER,
            element: json!({ "crdtId": 619, "screen": { "width": 1600.0 } }),
            crdt_id: 619,
            declared: declared.iter().map(|d| d.to_string()).collect(),
            device,
            reply,
        }
    }

    fn events(list: &[(&str, u32)]) -> Value {
        json!(list
            .iter()
            .map(|(event, tick)| json!({ "event": event, "button": "IaPointer", "tick": tick }))
            .collect::<Vec<_>>())
    }

    #[test]
    fn a_press_only_button_is_consumed_down_and_not_declared_up() {
        let clicked = json!({ "crdtEntityId": 619, "events": events(&[("PetDown", 10)]) });
        let reply: Value =
            serde_json::from_str(&verdict(&job(&["PetDown"], false, clicked), 12).unwrap())
                .unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["delivered"]["down"], "consumed");
        assert_eq!(reply["delivered"]["up"], "not_declared");
        assert_eq!(reply["delivered"]["enter"], "not_declared");
        assert!(reply["info"].as_str().unwrap().contains("PET_UP"));
        assert_eq!(reply["crdtId"], 619);
    }

    #[test]
    fn an_event_the_scene_has_not_been_sent_is_not_drained() {
        let clicked = json!({ "crdtEntityId": 619, "events": events(&[("PetDown", 10)]) });
        let reply = verdict(&job(&["PetDown"], false, clicked), 11).unwrap_err();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["delivered"]["down"], "not_drained");
    }

    #[test]
    fn a_click_on_an_element_with_no_handler_fails_and_says_so() {
        let clicked = json!({ "events": events(&[("PetHoverEnter", 3)]) });
        let declared = ["PetHoverEnter", "PetHoverLeave"];
        let reply = verdict(&job(&declared, true, clicked), 20).unwrap_err();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["delivered"]["enter"], "consumed");
        assert_eq!(reply["delivered"]["down"], "not_declared");
        assert!(reply["reason"]
            .as_str()
            .unwrap()
            .contains("no PET_DOWN or PET_UP"));
        assert_eq!(reply["path"], "device");
    }

    #[test]
    fn a_device_press_that_lands_elsewhere_counts_for_nothing() {
        let clicked = json!({ "crdtEntityId": 685, "events": events(&[("PetDown", 10)]) });
        let reply = verdict(&job(&["PetDown"], true, clicked), 30).unwrap_err();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["delivered"]["down"], "not_sent");
        assert!(reply["warning"].as_str().unwrap().contains("685"));
    }
}
