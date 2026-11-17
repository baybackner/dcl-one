//! `/press_input`: press a scene input action by its SDK name, through the key or mouse button
//! bound to it, aimed like `/click_entity` or wherever the cursor is. It runs as a pointer job, so
//! it shares the click's machine, aim, refusals and evidence, and adds the action, whether an
//! entity took the press (`entityBound`), the hold and the events the scene root got.

use bevy::prelude::Res;
use bevy_console::ConsoleCommand;
use common::inputs::{CommonInputAction, InputMap};
use console::reply::Span;
use input_manager::{InputManager, InputPriority};

use super::{
    answer, at_point, binding_error, check_hover_frames, timeout_frames, Answer, ButtonNames,
    ClickKind, PointerControl, PressInput, Request,
};

/// The actions `/press_input` takes: the name it takes, the action, the name the scene's entries
/// use.
const ACTIONS: [(&str, CommonInputAction, &str); 14] = [
    ("pointer", CommonInputAction::IaPointer, "IaPointer"),
    ("primary", CommonInputAction::IaPrimary, "IaPrimary"),
    ("secondary", CommonInputAction::IaSecondary, "IaSecondary"),
    ("jump", CommonInputAction::IaJump, "IaJump"),
    ("forward", CommonInputAction::IaForward, "IaForward"),
    ("backward", CommonInputAction::IaBackward, "IaBackward"),
    ("right", CommonInputAction::IaRight, "IaRight"),
    ("left", CommonInputAction::IaLeft, "IaLeft"),
    ("action_3", CommonInputAction::IaAction3, "IaAction3"),
    ("action_4", CommonInputAction::IaAction4, "IaAction4"),
    ("action_5", CommonInputAction::IaAction5, "IaAction5"),
    ("action_6", CommonInputAction::IaAction6, "IaAction6"),
    ("walk", CommonInputAction::IaWalk, "IaWalk"),
    ("modifier", CommonInputAction::IaModifier, "IaModifier"),
];

/// The action `name` names, with its names; the refusal lists the names it takes.
pub fn parse_action(name: &str) -> Result<(CommonInputAction, ButtonNames), String> {
    ACTIONS
        .iter()
        .find(|(arg, ..)| *arg == name)
        .map(|&(arg, action, entries)| {
            (
                action,
                ButtonNames {
                    arg,
                    action: entries,
                },
            )
        })
        .ok_or_else(|| {
            let names: Vec<_> = ACTIONS.iter().map(|(arg, ..)| *arg).collect();
            format!(
                "unknown action `{name}`; the values are lowercase, one of: {}",
                names.join(", ")
            )
        })
}

/// The hint for a press no entity took: what the scene root got it instead of. `hit` is the
/// entity the ray was on, `entries_name` the action as the scene's entries name it.
pub fn press_hint(
    entity_bound: bool,
    root_broadcast: bool,
    hit: Option<u32>,
    entries_name: &str,
) -> Option<String> {
    if entity_bound || !root_broadcast {
        return None;
    }
    Some(match hit.filter(|&id| id != super::FOREIGN_HIT) {
        Some(id) => format!(
            "entity {id} was under the ray but takes no {entries_name} press in reach, so the scene root got the press (rootEvents); a scene reads it from the root's PointerEventsResult"
        ),
        None => "no entity of the scene was under the ray, so the scene root got the press (rootEvents); a scene reads it from the root's PointerEventsResult".to_owned(),
    })
}

/// The warning for an unaimed press another entity took than the one under the ray: one whose
/// proximity entry won the press. `took` is the entity whose `PetDown` was logged, `hit` the one
/// under the ray.
pub fn proximity_warning(took: Option<u32>, hit: Option<u32>) -> Option<String> {
    let took = took.filter(|&took| Some(took) != hit)?;
    Some(match hit.filter(|&id| id != super::FOREIGN_HIT) {
        Some(hit) => format!(
            "entity {took} took the press (a proximity entry), not entity {hit} under the ray; the reply's hit is entity {hit}"
        ),
        None => format!(
            "entity {took} took the press (a proximity entry); no entity of the scene was under the ray"
        ),
    })
}

/// Press a scene input action by its SDK name (pointer, primary, secondary, jump, forward,
/// backward, right, left, action_3 to action_6, walk, modifier) through the key or mouse button
/// bound to it, and answer, as JSON, with the action, whether an entity took the press
/// (`entityBound`), the hit, the events the entity got and the ones the scene root got
/// (`rootEvents`), each with its scene tick, and a `hint` when the root got it. With --entity or
/// --at the press is aimed through the pointer pipeline as /click_entity aims; without either it
/// lands wherever the cursor is: on an entity under it that takes the action, else on the scene
/// root. --hold keeps the press down that many seconds (a click by default)
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/press_input")]
pub struct PressInputCommand {
    /// The action, lowercase: pointer, primary, secondary, jump, forward, backward, right, left,
    /// action_3, action_4, action_5, action_6, walk or modifier
    action: String,
    /// Seconds to keep the press down (at most 1800 frames); a click without it
    #[arg(long)]
    hold: Option<f32>,
    /// Aim at this entity (u32, as /scene_entities lists it), as /click_entity does
    #[arg(long)]
    entity: Option<String>,
    /// Aim at this world point, as /click_entity --at does
    #[arg(long, num_args = 3, value_names = ["x", "y", "z"], allow_negative_numbers = true)]
    at: Option<Vec<f32>>,
    /// Frames the press may take to land and release, beyond its hold (600 by default)
    #[arg(long, conflicts_with = "timeout_seconds")]
    timeout_frames: Option<u32>,
    /// Seconds the press may take, beyond its hold, instead of --timeout-frames
    #[arg(long)]
    timeout_seconds: Option<f32>,
}

pub(super) fn press_input_cmd(
    mut input: ConsoleCommand<PressInputCommand>,
    mut control: PointerControl,
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
    let result = press_request(&command, &mut control, &input_map, &input_manager)
        .unwrap_or_else(Answer::Failed);
    answer(&mut input, &mut control.pending, result, ended);
}

fn press_request(
    command: &PressInputCommand,
    control: &mut PointerControl,
    input_map: &InputMap,
    input_manager: &InputManager,
) -> Result<Answer, String> {
    let (action, button) = parse_action(&command.action)?;
    let hold_seconds = command.hold.unwrap_or(0.0);
    let hold_frames = Span {
        frames: None,
        seconds: Some(hold_seconds),
    }
    .frames(&control.rate)
    .map_err(|e| e.replace("--seconds", "--hold"))?
    .unwrap_or_default();
    check_hover_frames(hold_frames).map_err(|e| format!("--hold: {e}"))?;
    let timeout = timeout_frames(
        command.timeout_frames,
        command.timeout_seconds,
        &control.rate,
    )?;
    let at = at_point(&command.at)?;
    let Some(binding) = input_map.get_input(action) else {
        return Err(format!("{} has no binding", command.action));
    };
    if let Some(refusal) = binding_error(&command.action, &binding) {
        return Err(refusal);
    }
    let hud_holds = !input_manager.check_priority(&binding, InputPriority::Scene);
    Ok(control.start(Request {
        command: "/press_input",
        entity: command.entity.as_deref(),
        at,
        kind: ClickKind::Click,
        bounds: false,
        timeout,
        binding: Some(binding),
        button: Some(button),
        hud_holds,
        unaimed: command.entity.is_none() && at.is_none(),
        press: Some(PressInput {
            hold_seconds,
            hold_frames,
        }),
        ui: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_are_taken_by_their_lowercase_names() {
        let (action, names) = parse_action("action_3").unwrap();
        assert_eq!(action, CommonInputAction::IaAction3);
        assert_eq!((names.arg, names.action), ("action_3", "IaAction3"));
        let refusal = parse_action("PRIMARY").unwrap_err();
        assert!(refusal.starts_with("unknown action `PRIMARY`; the values are lowercase"));
        assert!(refusal.contains("pointer, primary, secondary"));
    }

    #[test]
    fn a_press_taken_off_the_ray_is_warned_about() {
        assert_eq!(proximity_warning(None, Some(5)), None);
        assert_eq!(proximity_warning(Some(5), Some(5)), None);
        assert!(proximity_warning(Some(7), Some(5))
            .unwrap()
            .starts_with("entity 7 took the press (a proximity entry), not entity 5"));
        assert!(proximity_warning(Some(7), None)
            .unwrap()
            .ends_with("no entity of the scene was under the ray"));
    }

    #[test]
    fn the_hint_names_what_the_root_got_the_press_instead_of() {
        assert_eq!(press_hint(true, false, Some(5), "IaPrimary"), None);
        // nothing landed at all: the diagnosis says why, not the hint
        assert_eq!(press_hint(false, false, None, "IaPrimary"), None);
        assert!(press_hint(false, true, Some(5), "IaPrimary")
            .unwrap()
            .starts_with("entity 5 was under the ray but takes no IaPrimary press"));
        assert!(press_hint(false, true, None, "IaJump")
            .unwrap()
            .starts_with("no entity of the scene"));
        assert!(
            press_hint(false, true, Some(super::super::FOREIGN_HIT), "IaJump")
                .unwrap()
                .starts_with("no entity of the scene")
        );
    }
}
