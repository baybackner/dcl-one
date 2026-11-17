//! `/ui_set_text` and `/ui_scroll`: write a scene-UI input's text, pick a dropdown option, or
//! scroll a container, by `crdtId`, through the widget's own paths: the text box's value (what
//! typing changes) and its submit event (what Enter sends), the option click's selection, and the
//! scrollable's position event. Each waits for the widget to take the write and for the scene to
//! be sent it, then says what changed.

use bevy::{diagnostic::FrameCount, prelude::*, window::PrimaryWindow};
use bevy_console::ConsoleCommand;
use bevy_simple_text_input::{TextInputSubmitEvent, TextInputValue};
use common::{
    rpc::{RpcResultReceiver, RpcResultSender},
    util::ModifyComponentExt,
};
use console::{
    reply::{self, FrameRate, Screen},
    DoAddConsoleCommand, PendingConsoleResponses,
};
use scene_inspector::SceneResolver;
use scene_runner::{
    renderer_context::RendererSceneContext,
    update_world::scene_ui::ui_automation::{UiAutomation, UiElement, UiKind},
};
use serde_json::{json, Value};
use ui_core::{
    combo_box::ComboBox,
    scrollable::{ScrollContent, ScrollPosition, ScrollTarget, ScrollTargetEvent},
    text_entry::{TextEntry, TextEntrySubmit, TextEntryValue},
    ui_actions::DataChanged,
};

use super::pointer::ui::{element_fields, merge};

/// Seconds a write may take to reach the widget, by default.
const DEFAULT_TIMEOUT_SECONDS: f32 = 10.0;
/// A widget's result written in the frame its value changed is with the scene once the scene's
/// tick has moved this far past that frame's: the handler runs a frame later and the write leaves
/// with the next dispatch.
const DRAINED_AFTER: u32 = 3;

pub(crate) struct AutomationUiWritePlugin;

impl Plugin for AutomationUiWritePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiWriteJob>()
            .add_console_command::<UiSetTextCommand, _>(ui_set_text_cmd)
            .add_console_command::<UiScrollCommand, _>(ui_scroll_cmd)
            .add_systems(Update, drive_ui_write);
    }
}

/// Write an input's text as typing would (replacing it), and with --submit send it as Enter does;
/// or pick a dropdown's option (--option-index, or --text naming the option). Refused for a
/// disabled element. Replies `ok`, `value`, `previousValue`, `delivered` {change, submit} (consumed,
/// not_sent or not_drained) and the element's crdtId, kind, screenRect, center, screen
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/ui_set_text")]
pub struct UiSetTextCommand {
    /// the element's crdtId, as /ui_list lists it
    crdt_id: u32,
    /// the text an input takes, or the label of a dropdown's option
    #[arg(long)]
    text: Option<String>,
    /// after writing, submit the input as Enter does (an input that clears on submit is emptied)
    #[arg(long)]
    submit: bool,
    /// a dropdown's option, from 0
    #[arg(long)]
    option_index: Option<usize>,
    /// whose UI: `sdk`, the scene's
    #[arg(long, default_value = "sdk")]
    stack: String,
    /// seconds the widget may take to take the write (10 by default)
    #[arg(long)]
    timeout_seconds: Option<f32>,
}

/// Scroll a scene-UI scroll container by --dx/--dy logical pixels (positive: toward later content,
/// right and down), as far as its content reaches. Replies `ok`, `before` and `after` {x, y}
/// offsets in pixels, `max`, `clamped`, an `info` line, `delivered` and the element's triple
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/ui_scroll")]
pub struct UiScrollCommand {
    /// the container's crdtId, as /ui_list lists it (kind scroll)
    crdt_id: u32,
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    dx: f32,
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    dy: f32,
    /// whose UI: `sdk`, the scene's
    #[arg(long, default_value = "sdk")]
    stack: String,
    /// seconds the container may take to move (10 by default)
    #[arg(long)]
    timeout_seconds: Option<f32>,
}

/// The write in flight.
#[derive(Resource, Default)]
pub struct UiWriteJob(Option<UiWrite>);

struct UiWrite {
    scene: Entity,
    node: Entity,
    steps: Vec<Write>,
    /// the step waiting for its widget to take it, and the frame it gives up at
    waiting: Option<u32>,
    /// the scene tick the last taken write is with the scene at
    drained_at: u32,
    deadline: u32,
    timeout: u32,
    /// what each step did: (name, verdict)
    delivered: Vec<(&'static str, &'static str)>,
    reply: Value,
    respond: RpcResultSender<Result<String, String>>,
}

enum Write {
    Text(String),
    Submit { value: String, clear: bool },
    Option(isize),
    Scroll { scrollable: Entity, to: Vec2 },
}

impl Write {
    fn name(&self) -> &'static str {
        match self {
            Write::Text(_) => "change",
            Write::Submit { .. } => "submit",
            Write::Option(_) => "change",
            Write::Scroll { .. } => "scroll",
        }
    }
}

/// The element by crdtId in the active scene, refused when the stack is not `sdk`, it is not
/// shown, or another write runs.
fn find(
    crdt_id: u32,
    stack: &str,
    command: &str,
    resolver: &SceneResolver,
    ui: &UiAutomation,
    job: &UiWriteJob,
) -> Result<(Entity, UiElement), String> {
    if !stack.eq_ignore_ascii_case("sdk") {
        return Err(reply::refusal(
            format!("{command} writes the scene's UI only (--stack sdk), not --stack {stack}"),
            None,
        ));
    }
    if job.0.is_some() {
        return Err(reply::refusal(
            "a /ui_set_text or /ui_scroll is still running",
            None,
        ));
    }
    let (scene, title) = resolver
        .resolve()
        .map(|(scene, ctx)| (scene, ctx.title.clone()))?;
    ui.elements(scene, true)
        .into_iter()
        .find(|element| element.crdt_id() == crdt_id)
        .map(|element| (scene, element))
        .ok_or_else(|| {
            reply::refusal(
                format!(
                    "crdtId {crdt_id} is no scene-UI element shown on screen in {title} (see \
                     /ui_list)"
                ),
                None,
            )
        })
}

fn refused(reason: String, cause: Value, element: &Value) -> String {
    let mut refusal: Value =
        serde_json::from_str(&reply::refusal(reason, Some(cause))).unwrap_or_default();
    merge(&mut refusal, element);
    refusal.to_string()
}

fn timeout(seconds: Option<f32>, rate: &FrameRate) -> Result<u32, String> {
    match seconds {
        Some(s) if !s.is_finite() || s <= 0.0 => Err(format!(
            "--timeout-seconds takes a duration above 0; got {s}"
        )),
        s => Ok(rate.frames_in(s.unwrap_or(DEFAULT_TIMEOUT_SECONDS))),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ui_set_text_cmd(
    mut input: ConsoleCommand<UiSetTextCommand>,
    resolver: SceneResolver,
    ui: UiAutomation,
    mut job: ResMut<UiWriteJob>,
    widgets: Query<(Option<&TextEntry>, Option<&ComboBox>)>,
    windows: Query<&Window, With<PrimaryWindow>>,
    rate: Res<FrameRate>,
    mut pending: ResMut<PendingConsoleResponses>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let started = (|| {
        let (scene, found) = find(
            command.crdt_id,
            &command.stack,
            "/ui_set_text",
            &resolver,
            &ui,
            &job,
        )?;
        let screen = Screen::primary(&windows).ok_or("no window")?;
        let element = element_fields(&found, &screen);
        let timeout = timeout(command.timeout_seconds, &rate)?;
        if found.disabled {
            return Err(refused(
                format!(
                    "the {} is disabled: a person could not write it",
                    found.kind.name()
                ),
                json!({ "crdtId": command.crdt_id, "disabled": true }),
                &element,
            ));
        }
        let (entry, combo) = widgets.get(found.node).unwrap_or((None, None));
        let (steps, mut reply) = match (found.kind, entry, combo) {
            (UiKind::Input, Some(entry), _) => {
                let Some(text) = command.text.clone() else {
                    return Err(refused(
                        "an input takes --text".to_owned(),
                        json!({ "crdtId": command.crdt_id, "kind": "input" }),
                        &element,
                    ));
                };
                if command.option_index.is_some() {
                    return Err(refused(
                        "--option-index picks a dropdown's option; this is an input".to_owned(),
                        json!({ "crdtId": command.crdt_id, "kind": "input" }),
                        &element,
                    ));
                }
                let mut steps = vec![Write::Text(text.clone())];
                if command.submit {
                    steps.push(Write::Submit {
                        value: text.clone(),
                        clear: entry.accept_line,
                    });
                }
                let reply = json!({
                    "value": text,
                    "previousValue": found.value,
                    "submitted": command.submit,
                    "clearsOnSubmit": entry.accept_line,
                });
                (steps, reply)
            }
            (UiKind::Dropdown, _, Some(combo)) => {
                let index = match (command.option_index, &command.text) {
                    (Some(index), _) => index,
                    (None, Some(text)) => combo
                        .options
                        .iter()
                        .position(|option| option == text)
                        .ok_or_else(|| {
                            refused(
                                format!("the dropdown has no option {text:?}"),
                                json!({ "options": combo.options }),
                                &element,
                            )
                        })?,
                    (None, None) => {
                        return Err(refused(
                            "a dropdown takes --option-index or --text naming an option".to_owned(),
                            json!({ "options": combo.options }),
                            &element,
                        ))
                    }
                };
                if index >= combo.options.len() {
                    return Err(refused(
                        format!(
                            "--option-index {index} is past the dropdown's {} options",
                            combo.options.len()
                        ),
                        json!({ "options": combo.options }),
                        &element,
                    ));
                }
                if command.submit {
                    return Err(refused(
                        "--submit submits an input; a dropdown has nothing to submit".to_owned(),
                        json!({ "crdtId": command.crdt_id, "kind": "dropdown" }),
                        &element,
                    ));
                }
                let reply = json!({
                    "selectedIndex": index,
                    "value": combo.options[index],
                    "previousIndex": combo.selected,
                });
                (vec![Write::Option(index as isize)], reply)
            }
            (kind, ..) => {
                return Err(refused(
                    format!(
                        "crdtId {} is a {}, not an input or a dropdown",
                        command.crdt_id,
                        kind.name()
                    ),
                    json!({ "crdtId": command.crdt_id, "kind": kind.name() }),
                    &element,
                ))
            }
        };
        merge(&mut reply, &element);
        Ok(start(&mut job, scene, found.node, steps, timeout, reply))
    })();
    match started {
        Ok(rx) => pending.push_receiver(rx, |r| r, input.take_responder()),
        Err(e) => input.reply_failed(e),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ui_scroll_cmd(
    mut input: ConsoleCommand<UiScrollCommand>,
    resolver: SceneResolver,
    ui: UiAutomation,
    mut job: ResMut<UiWriteJob>,
    scrollables: Query<(&ComputedNode, &ScrollContent, Option<&ScrollPosition>)>,
    nodes: Query<&ComputedNode>,
    windows: Query<&Window, With<PrimaryWindow>>,
    rate: Res<FrameRate>,
    mut pending: ResMut<PendingConsoleResponses>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let started = (|| {
        let (scene, found) = find(
            command.crdt_id,
            &command.stack,
            "/ui_scroll",
            &resolver,
            &ui,
            &job,
        )?;
        let screen = Screen::primary(&windows).ok_or("no window")?;
        let element = element_fields(&found, &screen);
        let timeout = timeout(command.timeout_seconds, &rate)?;
        if !command.dx.is_finite() || !command.dy.is_finite() {
            return Err("--dx and --dy take logical pixels".to_owned());
        }
        let Some((scrollable, (container, content, position))) = found
            .scroll
            .and_then(|scrollable| Some((scrollable, scrollables.get(scrollable).ok()?)))
        else {
            return Err(refused(
                format!(
                    "crdtId {} is a {}, not a scroll container",
                    command.crdt_id,
                    found.kind.name()
                ),
                json!({ "crdtId": command.crdt_id, "kind": found.kind.name() }),
                &element,
            ));
        };
        let content = nodes
            .get(content.0)
            .map_err(|_| "the container has no content")?;
        // how far the content can move, logical pixels, as the scrollable computes it
        let max = ((content.size() - container.size()) * container.inverse_scale_factor())
            .max(Vec2::ZERO);
        let fraction = position.map_or(Vec2::ZERO, |p| Vec2::new(p.h, p.v));
        let Scrolled {
            before,
            after,
            to,
            clamped,
        } = scrolled(fraction, max, Vec2::new(command.dx, command.dy));
        let point = |v: Vec2| json!({ "x": reply::round(v.x), "y": reply::round(v.y) });
        let mut reply = json!({
            "before": point(before),
            "after": point(after),
            "requested": { "dx": reply::round(command.dx), "dy": reply::round(command.dy) },
            "max": point(max),
            "clamped": clamped,
        });
        let moved = (after - before).abs().max_element() >= 0.5;
        reply["info"] = json!(if moved {
            format!(
                "scroll offset ({}, {}) -> ({}, {})",
                reply["before"]["x"],
                reply["before"]["y"],
                reply["after"]["x"],
                reply["after"]["y"]
            )
        } else if max == Vec2::ZERO {
            "did not move: the content does not overflow the container".to_owned()
        } else {
            format!(
                "did not move: the offset is already at that end ({}, {} of {}, {})",
                reply["before"]["x"], reply["before"]["y"], reply["max"]["x"], reply["max"]["y"]
            )
        });
        merge(&mut reply, &element);
        if !moved {
            reply["ok"] = json!(true);
            reply["delivered"] = json!({ "scroll": "not_sent" });
            let (respond, rx) = RpcResultSender::channel();
            respond.send(Ok(reply.to_string()));
            return Ok(rx);
        }
        Ok(start(
            &mut job,
            scene,
            scrollable,
            vec![Write::Scroll { scrollable, to }],
            timeout,
            reply,
        ))
    })();
    match started {
        Ok(rx) => pending.push_receiver(rx, |r| r, input.take_responder()),
        Err(e) => input.reply_failed(e),
    }
}

/// A scroll by `by` logical pixels from the position `fraction` (0 to 1 of `max`, how far the
/// content can move): the offsets before and after in pixels, the position to set, and whether the
/// ends cut it short.
struct Scrolled {
    before: Vec2,
    after: Vec2,
    to: Vec2,
    clamped: bool,
}

fn scrolled(fraction: Vec2, max: Vec2, by: Vec2) -> Scrolled {
    let before = fraction * max;
    let wanted = before + by;
    let after = wanted.clamp(Vec2::ZERO, max);
    let to = Vec2::new(
        if max.x > 0.0 { after.x / max.x } else { 0.0 },
        if max.y > 0.0 { after.y / max.y } else { 0.0 },
    );
    Scrolled {
        before,
        after,
        to,
        clamped: wanted != after,
    }
}

fn start(
    job: &mut UiWriteJob,
    scene: Entity,
    node: Entity,
    mut steps: Vec<Write>,
    timeout: u32,
    reply: Value,
) -> RpcResultReceiver<Result<String, String>> {
    let (respond, rx) = RpcResultSender::channel();
    steps.reverse();
    job.0 = Some(UiWrite {
        scene,
        node,
        steps,
        waiting: None,
        drained_at: 0,
        deadline: 0,
        timeout,
        delivered: Vec::new(),
        reply,
        respond,
    });
    rx
}

/// Run the write: issue a step, wait for its widget to take it, then the next; once all are taken,
/// wait for the scene's tick to pass the last one and reply.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn drive_ui_write(
    mut job: ResMut<UiWriteJob>,
    frame: Res<FrameCount>,
    scenes: Query<&RendererSceneContext>,
    mut commands: Commands,
    children: Query<&Children>,
    mut values: Query<(Entity, &mut TextInputValue)>,
    taken: Query<(
        Option<Ref<TextEntryValue>>,
        Option<Ref<TextEntrySubmit>>,
        Option<Ref<ComboBox>>,
        Option<Ref<ScrollPosition>>,
    )>,
    mut submits: EventWriter<TextInputSubmitEvent>,
    mut scrolls: EventWriter<ScrollTargetEvent>,
) {
    let Some(write) = job.0.as_mut() else {
        return;
    };
    let tick = scenes.get(write.scene).map_or(0, |ctx| ctx.tick_number);

    if let (Some(step), Some(gives_up)) = (write.steps.last(), write.waiting) {
        let (value, submit, combo, scroll) = taken.get(write.node).unwrap_or_default();
        let took = match step {
            Write::Text(text) => value.is_some_and(|v| v.is_changed() && v.0 == text.trim()),
            Write::Submit { value, .. } => {
                submit.is_some_and(|s| s.is_changed() && s.0 == value.trim())
            }
            Write::Option(index) => combo.is_some_and(|c| c.is_changed() && c.selected == *index),
            Write::Scroll { to, .. } => scroll.is_some_and(|p| {
                p.is_changed() && (Vec2::new(p.h, p.v) - *to).abs().max_element() < 0.01
            }),
        };
        if took {
            let step = write.steps.pop().unwrap();
            write.delivered.push((step.name(), "sent"));
            write.waiting = None;
            write.drained_at = tick + DRAINED_AFTER;
            write.deadline = frame.0 + write.timeout;
        } else if frame.0 >= gives_up {
            let step = write.steps.pop().unwrap();
            write.delivered.push((step.name(), "not_sent"));
            write.steps.clear();
            write.waiting = None;
        } else {
            return;
        }
    }

    // the scene keeps only the last result an element wrote in a tick: a step waits for the
    // scene to be sent the one before it
    if !write.steps.is_empty() && tick < write.drained_at && frame.0 < write.deadline {
        return;
    }
    if let Some(step) = write.steps.last() {
        // the text box is the entry's child that holds the typed value
        let text_box = children
            .get(write.node)
            .into_iter()
            .flatten()
            .copied()
            .find(|child| values.contains(*child));
        match step {
            Write::Text(text) => {
                if let Some(Ok((_, mut value))) = text_box.map(|e| values.get_mut(e)) {
                    value.0 = text.clone();
                }
            }
            Write::Submit { value, clear } => {
                // as Enter does: the event with the value, the box emptied unless it retains it
                if let Some(Ok((entity, mut text))) = text_box.map(|e| values.get_mut(e)) {
                    submits.write(TextInputSubmitEvent {
                        entity,
                        value: value.clone(),
                    });
                    if *clear {
                        text.0.clear();
                    }
                }
            }
            Write::Option(index) => {
                let index = *index;
                // as an option's click does (ui_core combo_box)
                commands
                    .entity(write.node)
                    .modify_component(move |combo: &mut ComboBox| combo.selected = index)
                    .try_insert(DataChanged);
            }
            Write::Scroll { scrollable, to } => {
                scrolls.write(ScrollTargetEvent {
                    scrollable: *scrollable,
                    position: ScrollTarget::Literal(*to),
                });
            }
        }
        write.waiting = Some(frame.0 + write.timeout);
        return;
    }

    // every step taken (or one never was): wait for the scene to be sent the last
    let all_sent = write
        .delivered
        .iter()
        .all(|(_, verdict)| *verdict == "sent");
    if all_sent && tick < write.drained_at && frame.0 < write.deadline {
        return;
    }
    let write = job.0.take().unwrap();
    let mut delivered = json!({});
    for (name, verdict) in &write.delivered {
        delivered[*name] = json!(match *verdict {
            "sent" if tick >= write.drained_at => "consumed",
            "sent" => "not_drained",
            other => other,
        });
    }
    let mut reply = write.reply;
    let ok = write
        .delivered
        .iter()
        .all(|(name, _)| delivered[*name] == "consumed");
    reply["delivered"] = delivered;
    reply["ok"] = json!(ok);
    if ok {
        write.respond.send(Ok(reply.to_string()));
    } else {
        reply["reason"] =
            json!("the widget did not take the write, or the scene was not sent it in time");
        write.respond.send(Err(reply.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scroll_moves_by_pixels_and_stops_at_the_ends() {
        let max = Vec2::new(0.0, 800.0);
        let down = scrolled(Vec2::ZERO, max, Vec2::new(0.0, 400.0));
        assert_eq!(
            (down.before, down.after),
            (Vec2::ZERO, Vec2::new(0.0, 400.0))
        );
        assert_eq!(down.to, Vec2::new(0.0, 0.5));
        assert!(!down.clamped);

        let up_at_top = scrolled(Vec2::ZERO, max, Vec2::new(0.0, -400.0));
        assert_eq!(up_at_top.after, Vec2::ZERO);
        assert!(up_at_top.clamped);

        let past_the_end = scrolled(Vec2::new(0.0, 0.5), max, Vec2::new(30.0, 1000.0));
        assert_eq!(past_the_end.after, Vec2::new(0.0, 800.0));
        assert_eq!(past_the_end.to, Vec2::new(0.0, 1.0));
        assert!(past_the_end.clamped);
    }
}
