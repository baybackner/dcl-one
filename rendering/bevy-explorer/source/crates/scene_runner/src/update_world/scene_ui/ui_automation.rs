//! Scene UI as an agent reads it: every laid-out element of the parcel scene under the player, its
//! kind, where it is on screen and what covers it (`/ui_list`).

use bevy::{
    ecs::system::SystemParam,
    prelude::*,
    ui::{CalculatedClip, ComputedNode, FocusPolicy, UiStack},
    window::PrimaryWindow,
};
use bevy_console::ConsoleCommand;
use common::{structs::PrimaryUser, util::DespawnWith};
use console::reply::{self, Screen};
use dcl_component::SceneEntityId;
use serde_json::{json, Value};
use ui_core::{
    combo_box::ComboBox,
    text_entry::{TextEntry, TextEntryValue},
};

use super::{ui_text::UiText, UiLink};
use crate::{
    renderer_context::RendererSceneContext, update_world::pointer_events::PointerEvents,
    ContainingScene, SceneEntity,
};

/// What an element is to an agent, in the order an element with several is classed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiKind {
    Input,
    Dropdown,
    Scroll,
    PointerTarget,
    /// a label: listed only when asked for
    Text,
}

impl UiKind {
    pub fn name(self) -> &'static str {
        match self {
            UiKind::Input => "input",
            UiKind::Dropdown => "dropdown",
            UiKind::Scroll => "scroll",
            UiKind::PointerTarget => "pointerTarget",
            UiKind::Text => "text",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [
            UiKind::Input,
            UiKind::Dropdown,
            UiKind::Scroll,
            UiKind::PointerTarget,
            UiKind::Text,
        ]
        .into_iter()
        .find(|kind| kind.name().eq_ignore_ascii_case(name))
    }
}

/// What is drawn over an element's centre and takes the pointer there.
#[derive(Clone, Debug, PartialEq)]
pub enum Cover {
    /// a scene-UI element (of this scene or another)
    Scene { crdt_id: u32, scene: Entity },
    /// the engine's own UI, by its `Name` where it has one
    Client(String),
}

impl Cover {
    /// How a refusal names it: `the scene's UI (crdtId N)`, `the client's UI (<name>)`.
    pub fn blocked_by(&self) -> String {
        match self {
            Cover::Scene { crdt_id, .. } => format!("the scene's UI (crdtId {crdt_id})"),
            Cover::Client(name) => format!("the client's UI ({name})"),
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Cover::Scene { crdt_id, .. } => json!({ "crdtId": crdt_id }),
            Cover::Client(name) => json!({ "client": name }),
        }
    }
}

/// One laid-out scene-UI element.
#[derive(Clone, Debug)]
pub struct UiElement {
    /// the scene entity
    pub entity: Entity,
    pub id: SceneEntityId,
    /// the laid-out Bevy node
    pub node: Entity,
    pub kind: UiKind,
    /// a scroll container's scrollable (it holds the `ScrollPosition`)
    pub scroll: Option<Entity>,
    /// the visible part, logical pixels from the window's top left
    pub rect: Rect,
    /// a scroll container or the window cuts part of it off
    pub clipped: bool,
    pub draw_order: u32,
    pub disabled: bool,
    pub event_types: Vec<String>,
    pub text: Option<String>,
    pub value: Option<String>,
    pub placeholder: Option<String>,
    pub options: Option<(Vec<String>, isize)>,
    pub covered_by: Option<Cover>,
}

impl UiElement {
    pub fn crdt_id(&self) -> u32 {
        self.id.as_proto_u32().unwrap_or_default()
    }

    pub fn to_json(&self, screen: &Screen) -> Value {
        let center = screen.to_normalized(self.rect.center());
        let mut element = json!({
            "crdtId": self.crdt_id(),
            "kind": self.kind.name(),
            "screenRect": {
                "x": reply::round(self.rect.min.x),
                "y": reply::round(self.rect.min.y),
                "width": reply::round(self.rect.width()),
                "height": reply::round(self.rect.height()),
            },
            "center": { "x": reply::round_fine(center.x), "y": reply::round_fine(center.y) },
            "disabled": self.disabled,
            "pointerEventTypes": self.event_types,
            "clipped": self.clipped,
            "drawOrder": self.draw_order,
            "coveredBy": self.covered_by.as_ref().map_or(Value::Null, Cover::to_json),
        });
        for (key, value) in [
            ("text", &self.text),
            ("value", &self.value),
            ("placeholder", &self.placeholder),
        ] {
            if let Some(value) = value {
                element[key] = json!(value);
            }
        }
        if let Some((options, selected)) = &self.options {
            element["options"] = json!(options);
            element["selectedIndex"] = json!(selected);
        }
        element
    }
}

/// Scene UI as laid out in the last frame: its elements and what covers them. Shared by the
/// `/ui_*` commands.
#[derive(SystemParam)]
pub struct UiAutomation<'w, 's> {
    elements: Query<
        'w,
        's,
        (
            Entity,
            &'static SceneEntity,
            &'static UiLink,
            Option<&'static UiText>,
            Option<&'static PointerEvents>,
        ),
    >,
    #[allow(clippy::type_complexity)]
    nodes: Query<
        'w,
        's,
        (
            &'static ComputedNode,
            &'static GlobalTransform,
            &'static InheritedVisibility,
            Option<&'static CalculatedClip>,
            Option<&'static Interaction>,
            Option<&'static FocusPolicy>,
        ),
    >,
    widgets: Query<
        'w,
        's,
        (
            Option<&'static TextEntry>,
            Option<&'static TextEntryValue>,
            Option<&'static ComboBox>,
        ),
    >,
    owners: Query<'w, 's, (Option<&'static DespawnWith>, Option<&'static Name>)>,
    scene_entities: Query<'w, 's, &'static SceneEntity>,
    hierarchy: Query<'w, 's, (&'static Node, Option<&'static ChildOf>)>,
    stack: Option<Res<'w, UiStack>>,
}

impl UiAutomation<'_, '_> {
    /// Every laid-out, shown element of the scene `root`'s window UI, top to bottom then left to
    /// right; labels (`UiKind::Text`) only with `with_text`.
    pub fn elements(&self, root: Entity, with_text: bool) -> Vec<UiElement> {
        let mut found: Vec<_> = self
            .elements
            .iter()
            .filter(|(_, scene_entity, link, ..)| {
                scene_entity.root == root
                    && scene_entity.id != SceneEntityId::ROOT
                    && link.is_window_ui
            })
            .filter_map(|(entity, scene_entity, link, text, events)| {
                self.element(entity, scene_entity.id, link, text, events)
            })
            .filter(|element| with_text || element.kind != UiKind::Text)
            .collect();
        found.sort_by(|a, b| {
            (a.rect.min.y, a.rect.min.x)
                .partial_cmp(&(b.rect.min.y, b.rect.min.x))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        found
    }

    fn element(
        &self,
        entity: Entity,
        id: SceneEntityId,
        link: &UiLink,
        text: Option<&UiText>,
        events: Option<&PointerEvents>,
    ) -> Option<UiElement> {
        let node = link.ui_entity;
        let (computed, global, visibility, clip, ..) = self.nodes.get(node).ok()?;
        if !visibility.get() || !super::ui_layout_feedback::displayed(&self.hierarchy, node) {
            return None;
        }
        let (entry, entry_value, combo) = self.widgets.get(node).ok()?;
        let event_types = events.map_or_else(Vec::new, |events| {
            let mut types: Vec<String> = events
                .msg
                .values()
                .flat_map(|pb| pb.pointer_events.iter())
                .map(|entry| format!("{:?}", entry.event_type()))
                .collect();
            types.sort();
            types.dedup();
            types
        });
        let kind = if entry.is_some() {
            UiKind::Input
        } else if combo.is_some() {
            UiKind::Dropdown
        } else if link.scroll_entity.is_some() {
            UiKind::Scroll
        } else if !event_types.is_empty() {
            UiKind::PointerTarget
        } else if text.is_some() {
            UiKind::Text
        } else {
            return None;
        };

        // bevy_ui works in physical pixels; replies are logical
        let scale = computed.inverse_scale_factor();
        let full = Rect::from_center_size(global.translation().truncate(), computed.size());
        let shown = clip.map_or(full, |clip| full.intersect(clip.clip));
        if shown.is_empty() {
            return None;
        }
        let covered_by = self.cover(node, computed.stack_index(), shown.center());

        Some(UiElement {
            entity,
            id,
            node,
            kind,
            scroll: link.scroll_entity,
            rect: Rect {
                min: shown.min * scale,
                max: shown.max * scale,
            },
            clipped: shown != full,
            draw_order: computed.stack_index(),
            disabled: entry.is_some_and(|entry| !entry.enabled)
                || combo.is_some_and(|combo| combo.disabled),
            event_types,
            text: text.map(|text| text.text.clone()),
            value: entry.map(|entry| {
                entry_value.map_or_else(|| entry.content.clone(), |value| value.0.clone())
            }),
            placeholder: entry
                .map(|entry| entry.hint_text.clone())
                .filter(|hint| !hint.is_empty()),
            options: combo.map(|combo| (combo.options.clone(), combo.selected)),
            covered_by,
        })
    }

    /// The topmost node drawn above `node` that takes the pointer at `point` (physical pixels),
    /// as Bevy's picking finds it: shown, interactive, blocking, not one of `node`'s own children.
    fn cover(&self, node: Entity, stack_index: u32, point: Vec2) -> Option<Cover> {
        let stack = self.stack.as_ref()?;
        let above = stack.uinodes.get(stack_index as usize + 1..)?;
        self.top_at(above, point, |other| self.inside(other, node))
    }

    /// What takes the pointer at `point` (physical pixels): the topmost shown, interactive,
    /// blocking node there, scene UI or the engine's own. `None` when the world gets it.
    pub fn at(&self, point: Vec2) -> Option<Cover> {
        let stack = self.stack.as_ref()?;
        self.top_at(&stack.uinodes, point, |_| false)
    }

    fn top_at(
        &self,
        nodes: &[Entity],
        point: Vec2,
        skip: impl Fn(Entity) -> bool,
    ) -> Option<Cover> {
        let top = nodes.iter().rev().copied().find(|&other| {
            let Ok((computed, global, visibility, clip, interaction, focus)) =
                self.nodes.get(other)
            else {
                return false;
            };
            let rect = Rect::from_center_size(global.translation().truncate(), computed.size());
            let rect = clip.map_or(rect, |clip| rect.intersect(clip.clip));
            visibility.get()
                && interaction.is_some()
                && focus.is_none_or(|focus| *focus == FocusPolicy::Block)
                && rect.contains(point)
                && !skip(other)
        })?;
        // a widget's inner node (a text entry's, a scroll bar's) belongs to the nearest ancestor
        // a scene entity owns
        let mut node = top;
        loop {
            let (owner, _) = self.owners.get(node).ok()?;
            if let Some(scene_entity) =
                owner.and_then(|owner| self.scene_entities.get(owner.0).ok())
            {
                return Some(Cover::Scene {
                    crdt_id: scene_entity.id.as_proto_u32().unwrap_or_default(),
                    scene: scene_entity.root,
                });
            }
            match self.hierarchy.get(node) {
                Ok((_, Some(parent))) => node = parent.parent(),
                _ => break,
            }
        }
        let name = self.owners.get(top).ok().and_then(|(_, name)| name);
        Some(Cover::Client(
            name.map_or_else(|| format!("{top}"), |name| name.to_string()),
        ))
    }

    /// Whether `node` is `ancestor` or below it.
    fn inside(&self, mut node: Entity, ancestor: Entity) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            match self.hierarchy.get(node) {
                Ok((_, Some(parent))) => node = parent.parent(),
                _ => return false,
            }
        }
    }
}

/// The parcel scene's UI elements on screen: `crdtId`, `kind` (input, dropdown, scroll,
/// pointerTarget, or text with --kind text), `screenRect` (logical px), `center` (normalized),
/// `disabled`, `pointerEventTypes`, `text`/`value`/`placeholder`/`options`, `coveredBy`
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/ui_list")]
pub struct UiListCommand {
    /// whose UI: `sdk`, the scene's (the HUD's own is `/hud_list`)
    #[arg(long, default_value = "sdk")]
    stack: String,
    /// only this kind: input, dropdown, scroll, pointerTarget or text (labels, listed only here)
    #[arg(long)]
    kind: Option<String>,
    /// at most this many elements
    #[arg(long, default_value_t = 200)]
    limit: usize,
}

pub fn ui_list_command(
    mut input: ConsoleCommand<UiListCommand>,
    ui: UiAutomation,
    player: Query<Entity, With<PrimaryUser>>,
    containing_scene: ContainingScene,
    contexts: Query<&RendererSceneContext>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    if !command.stack.eq_ignore_ascii_case("sdk") {
        input.reply_failed(reply::refusal(
            format!(
                "/ui_list serves the scene's UI only (--stack sdk); the client's own UI is read \
                 with /hud_list, not --stack {}",
                command.stack
            ),
            None,
        ));
        return;
    }
    let kind = match command
        .kind
        .as_deref()
        .map(|name| (name, UiKind::parse(name)))
    {
        None => None,
        Some((_, Some(kind))) => Some(kind),
        Some((name, None)) => {
            input.reply_failed(format!(
                "--kind is input, dropdown, scroll, pointerTarget or text; got {name}"
            ));
            return;
        }
    };
    let Some(root) = player
        .single()
        .ok()
        .and_then(|player| containing_scene.get_parcel(player))
    else {
        input.reply_failed("player is not in any scene");
        return;
    };
    let Some(screen) = Screen::primary(&windows) else {
        input.reply_failed("no window");
        return;
    };

    let mut elements = ui.elements(root, kind == Some(UiKind::Text));
    elements.retain(|element| kind.is_none_or(|kind| element.kind == kind));
    let count = elements.len();
    elements.truncate(command.limit);
    let context = contexts.get(root).ok();
    let mut reply = json!({
        "stack": "sdk",
        "scene": context.map(|context| json!({ "hash": context.hash.as_ref(), "title": context.title })),
        "screen": screen.to_json(),
        "count": count,
        "elements": elements.iter().map(|element| element.to_json(&screen)).collect::<Vec<_>>(),
        "coverage": "coveredBy names scene UI and the engine's own UI; what the HUD covers (the \
                     page on the web, the CEF overlay natively) is not tested here",
    });
    if count > elements.len() {
        reply["truncated"] = json!(true);
    }
    input.reply_ok(reply.to_string());
}

/// What takes the pointer at a window position, in logical pixels from the top left (or, with
/// --normalized, 0 to 1): `takenBy` {crdtId} for a scene's UI, {client} for the engine's own,
/// null where the world gets it; `blockedByUi` names a scene's UI as /click_at refuses it
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/ui_at")]
pub struct UiAtCommand {
    x: f32,
    y: f32,
    /// x and y are 0 to 1 of the window from the top left, not pixels
    #[arg(long)]
    normalized: bool,
}

pub fn ui_at_command(
    mut input: ConsoleCommand<UiAtCommand>,
    ui: UiAutomation,
    contexts: Query<&RendererSceneContext>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let Some(Ok(command)) = input.take() else {
        return;
    };
    let Some(screen) = Screen::primary(&windows) else {
        input.reply_failed("no window");
        return;
    };
    let position = if command.normalized {
        match reply::normalized_point(command.x, command.y) {
            Ok(point) => screen.to_pixels(point),
            Err(e) => {
                input.reply_failed(e);
                return;
            }
        }
    } else {
        Vec2::new(command.x, command.y)
    };
    let cover = ui.at(position * screen.scale);
    input.reply_ok(ui_at_reply(position, &screen, cover.as_ref(), &contexts).to_string());
}

/// `/ui_at`'s reply, and the fields `/click_at` refuses with.
pub fn ui_at_reply(
    position: Vec2,
    screen: &Screen,
    cover: Option<&Cover>,
    contexts: &Query<&RendererSceneContext>,
) -> Value {
    let normalized = screen.to_normalized(position);
    let mut reply = json!({
        "x": reply::round(position.x),
        "y": reply::round(position.y),
        "normalized": { "x": reply::round_fine(normalized.x), "y": reply::round_fine(normalized.y) },
        "screen": screen.to_json(),
        "takenBy": cover.map_or(Value::Null, Cover::to_json),
    });
    if let Some(Cover::Scene { scene, .. }) = cover {
        reply["takenBy"]["scene"] = json!(contexts.get(*scene).ok().map(|c| c.hash.as_ref()));
        reply["blockedByUi"] = json!(cover.map(Cover::blocked_by));
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_parse_by_their_reply_names() {
        for kind in [
            UiKind::Input,
            UiKind::Dropdown,
            UiKind::Scroll,
            UiKind::PointerTarget,
            UiKind::Text,
        ] {
            assert_eq!(UiKind::parse(kind.name()), Some(kind));
        }
        assert_eq!(UiKind::parse("pointertarget"), Some(UiKind::PointerTarget));
        assert_eq!(UiKind::parse("button"), None);
    }

    #[test]
    fn the_center_is_the_rect_centre_over_the_screen() {
        let element = UiElement {
            entity: Entity::PLACEHOLDER,
            id: SceneEntityId::new(597, 1),
            node: Entity::PLACEHOLDER,
            kind: UiKind::PointerTarget,
            scroll: None,
            rect: Rect::new(100.0, 50.0, 300.0, 90.0),
            clipped: false,
            draw_order: 3,
            disabled: false,
            event_types: vec!["PetDown".to_owned()],
            text: Some("Button 1".to_owned()),
            value: None,
            placeholder: None,
            options: None,
            covered_by: Some(Cover::Scene {
                crdt_id: 600,
                scene: Entity::PLACEHOLDER,
            }),
        };
        let screen = Screen {
            width: 1600.0,
            height: 800.0,
            scale: 2.0,
        };
        let json = element.to_json(&screen);
        assert_eq!(json["crdtId"], 66133);
        assert_eq!(json["center"]["x"], 0.125);
        assert_eq!(json["center"]["y"], 0.0875);
        assert_eq!(json["screenRect"]["width"], 200.0);
        assert_eq!(json["coveredBy"]["crdtId"], 600);
        assert_eq!(json["text"], "Button 1");
        assert!(json.get("value").is_none());
    }
}
