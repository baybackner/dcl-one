//! The pure side of `/hover_entity` and `/click_entity`: the state machine a job steps through
//! frame by frame, and the refusals and diagnoses it words.

use bevy::math::Vec3;
use common::inputs::InputIdentifier;
use scene_runner::update_scene::synthetic_pointer::HoverEntryReport;

pub(super) const ENTITY_NOT_LIVE: &str = "entity not live";
pub(super) const HUD_HOLDS_INPUT: &str = "input held by the HUD";
pub(super) const OUT_OF_REACH: &str =
    "out of reach: the entity's entry for the button is past its distance limit";
pub(super) const NOTHING_AT_POINT: &str =
    "nothing at the point: the ray from the camera through it reaches no collider there";
/// The id a hit reports when it is no entity of the job's scene (an avatar, another scene).
pub(super) const FOREIGN_HIT: u32 = u32::MAX;
/// How far from an aimed world point a hit may be and still be what is at the point; a hit whose
/// visible bounds hold the point is at it however far its surface is.
pub const POINT_TOLERANCE: f32 = 1.0;
/// Frames a point aim may find nothing at its point before it refuses (a frame for the aim to
/// reach the pointer target, and one more).
pub const MISS_FRAMES: u32 = 2;

/// What the pointer ray reached, against the world point it was aimed through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointHit {
    /// the hit is at the point: within [`POINT_TOLERANCE`], or its bounds hold the point
    At,
    /// the hit is nearer the camera than the point: it stands in front of it
    Before,
    /// the hit is past the point: the point is in empty air
    Beyond,
    /// the ray reached nothing
    Nothing,
}

/// Where `hit` (the ray's hit position, if any) is against the `point` the ray from `camera` was
/// aimed through; `inside_hit_bounds` is whether the hit entity's visible bounds hold the point.
pub fn classify_point(
    camera: Vec3,
    point: Vec3,
    hit: Option<Vec3>,
    inside_hit_bounds: bool,
) -> PointHit {
    let Some(hit) = hit else {
        return PointHit::Nothing;
    };
    if inside_hit_bounds || hit.distance(point) <= POINT_TOLERANCE {
        PointHit::At
    } else if camera.distance(hit) < camera.distance(point) {
        PointHit::Before
    } else {
        PointHit::Beyond
    }
}

/// What a job does once the aim is on the entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickKind {
    /// hover for this many frames
    Hover(u32),
    /// press, then release on a later scene tick
    Click,
    /// press and keep the press held (the aim stays, `Hold::Open`)
    Down,
    /// release a held press
    Up,
}

/// The pointer target a frame saw, as proto entity ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSeen {
    /// the hit entity
    pub entity: u32,
    /// the entity owning the hit collider
    pub container: u32,
    /// the hit container's ancestors that are scene entities, below the scene root
    pub ancestors_hit: Vec<u32>,
}

impl TargetSeen {
    fn matches(&self, id: u32) -> bool {
        self.entity == id || self.container == id || self.ancestors_hit.contains(&id)
    }
}

/// One frame of the pipeline as the job sees it. The default has `entity_live: true` (a manual
/// `Default`), so a test names only what it changes.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    pub target: Option<TargetSeen>,
    pub scene_tick: Option<u32>,
    /// a `PetDown` for the entity, or for the scene root (no entity took it), is in the log since
    /// the job started
    pub down_logged: bool,
    /// a `PetUp` for the entity or the scene root is in the log since the job started
    pub up_logged: bool,
    /// the release the job queued has been played, and this frame's pointer pipeline ran after it
    pub release_played: bool,
    /// the scene still maps the entity to the aimed bevy entity
    pub entity_live: bool,
    /// for an aim at a world point, where the hit is against it; `None` for an entity aim
    pub point: Option<PointHit>,
    /// the hit declares an entry for the press but none in reach ([`press_out_of_reach`])
    pub out_of_reach: bool,
}

impl Default for Observed {
    fn default() -> Self {
        Self {
            target: None,
            scene_tick: None,
            down_logged: false,
            up_logged: false,
            release_played: false,
            entity_live: true,
            point: None,
            out_of_reach: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// press the binding
    Press,
    /// release the binding
    Release,
    /// the job is over: `Err` carries the diagnosis
    Finish(Result<(), String>),
    /// the job is over without pressing anything: the reason it refused
    Refuse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Target,
    Hovering,
    Tick,
    Release,
    Done,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::Target => "target",
            Stage::Hovering => "hover",
            Stage::Tick => "tick",
            Stage::Release | Stage::Done => "release",
        }
    }
}

/// The steps of a hover or click, decided from one [`Observed`] frame at a time. Nothing is
/// evaluated before `started + 2` (the aim needs a frame to reach the pointer target); past
/// `started + timeout` the job times out naming the stage it waited in; a hover's deadline is at
/// least `started + frames + 3`, so the default timeout never cuts a long hover short. While no
/// target is resolved the job waits (a collider may still be loading), unless the aimed point is
/// past the cursor ray's reach; a resolved target that is not the entity fails at once. A job aimed
/// at a world point takes what the ray reaches at the point (any entity of the scene, or the one
/// asked for); a hit in front of the point blocks it, and nothing at the point refuses the job
/// before anything is pressed. A release
/// is done once the scene logged the `PetUp` or the release has been played (a target with no
/// `PetUp` entry logs none). A failure does not release a press itself:
/// [`ClickMachine::holds_press`] tells the caller to.
pub struct ClickMachine {
    kind: ClickKind,
    /// the proto id asked for; `None` takes whatever a point aim reaches
    entity: Option<u32>,
    timeout: u32,
    started: u32,
    stage: Stage,
    pressed: bool,
    /// the scene tick the down was first seen at
    down_tick: Option<u32>,
}

impl ClickMachine {
    /// `entity` is the proto id asked for (`None` for a point aim naming none); an `Up` starts
    /// holding the press it releases.
    pub fn new(kind: ClickKind, entity: Option<u32>, timeout: u32, started: u32) -> Self {
        Self {
            kind,
            entity,
            timeout: match kind {
                ClickKind::Hover(frames) => timeout.max(frames.saturating_add(3)),
                _ => timeout,
            },
            started,
            stage: Stage::Target,
            pressed: kind == ClickKind::Up,
            down_tick: None,
        }
    }

    pub fn kind(&self) -> ClickKind {
        self.kind
    }

    /// Whether a press is down that the machine has not released.
    pub fn holds_press(&self) -> bool {
        self.pressed
    }

    pub fn step(&mut self, frame: u32, observed: Observed) -> Vec<Effect> {
        if self.stage == Stage::Done || frame < self.started.saturating_add(2) {
            return Vec::new();
        }
        if !observed.entity_live {
            return self.finish(Err(ENTITY_NOT_LIVE.to_owned()));
        }
        if frame >= self.started.saturating_add(self.timeout) {
            let stage = self.stage.name();
            return self.finish(Err(format!("timed out waiting for {stage}")));
        }
        match self.stage {
            Stage::Target if self.kind == ClickKind::Up => {
                self.pressed = false;
                self.stage = Stage::Release;
                vec![Effect::Release]
            }
            Stage::Target => match (observed.point, observed.target) {
                (Some(PointHit::Nothing | PointHit::Beyond), _) => {
                    if frame >= self.started.saturating_add(2 + MISS_FRAMES) {
                        self.refuse(NOTHING_AT_POINT.to_owned())
                    } else {
                        Vec::new()
                    }
                }
                (_, None) => Vec::new(),
                (Some(PointHit::Before), Some(target)) => self.finish(Err(blocked_by(&target))),
                (point, Some(target)) if !self.accepts(&target) => match self.entity {
                    // the point holds another entity of the scene than the one asked for
                    Some(asked) if point.is_some() && target.container != FOREIGN_HIT => self
                        .refuse(format!(
                            "the point holds entity {}, not entity {asked}",
                            target.entity
                        )),
                    _ => self.finish(Err(blocked_by(&target))),
                },
                (_, Some(_)) if matches!(self.kind, ClickKind::Hover(_)) => {
                    self.stage = Stage::Hovering;
                    self.hover(frame)
                }
                // the press would go past the entity to the scene root: none is sent
                (_, Some(_)) if observed.out_of_reach => self.finish(Err(OUT_OF_REACH.to_owned())),
                (_, Some(_)) => {
                    self.pressed = true;
                    self.stage = Stage::Tick;
                    vec![Effect::Press]
                }
            },
            Stage::Hovering => self.hover(frame),
            Stage::Tick => {
                if !observed.down_logged {
                    return Vec::new();
                }
                if self.kind == ClickKind::Down {
                    return self.finish(Ok(()));
                }
                let Some(tick) = observed.scene_tick else {
                    return Vec::new();
                };
                // the up goes on a later tick than the down, so the scene sees both
                if tick > *self.down_tick.get_or_insert(tick) {
                    self.pressed = false;
                    self.stage = Stage::Release;
                    vec![Effect::Release]
                } else {
                    Vec::new()
                }
            }
            Stage::Release if observed.up_logged || observed.release_played => self.finish(Ok(())),
            Stage::Release | Stage::Done => Vec::new(),
        }
    }

    fn hover(&mut self, frame: u32) -> Vec<Effect> {
        let ClickKind::Hover(frames) = self.kind else {
            return Vec::new();
        };
        if frame >= self.started.saturating_add(2).saturating_add(frames) {
            self.finish(Ok(()))
        } else {
            Vec::new()
        }
    }

    fn accepts(&self, target: &TargetSeen) -> bool {
        match self.entity {
            Some(id) => target.matches(id),
            None => target.container != FOREIGN_HIT,
        }
    }

    fn finish(&mut self, result: Result<(), String>) -> Vec<Effect> {
        self.stage = Stage::Done;
        vec![Effect::Finish(result)]
    }

    fn refuse(&mut self, reason: String) -> Vec<Effect> {
        self.stage = Stage::Done;
        vec![Effect::Refuse(reason)]
    }
}

/// The diagnosis for a hit that stands in the way.
fn blocked_by(target: &TargetSeen) -> String {
    if target.container == FOREIGN_HIT {
        "blocked by an avatar or another scene's entity".to_owned()
    } else {
        format!("blocked by entity {}", target.container)
    }
}

/// The longest hover `/hover_entity --frames` accepts.
pub const MAX_HOVER_FRAMES: u32 = 1800;

/// The refusal for a hover longer than [`MAX_HOVER_FRAMES`].
pub fn check_hover_frames(frames: u32) -> Result<(), String> {
    if frames > MAX_HOVER_FRAMES {
        Err(format!("frames must be at most {MAX_HOVER_FRAMES}"))
    } else {
        Ok(())
    }
}

/// The refusal while a press is held on `held` (a proto id), if one is.
pub fn refuse_while_held(held: Option<u32>, _command: &str) -> Option<String> {
    held.map(|id| {
        format!("a press is held on entity {id}; send /click_entity {id} --event up or /hover_entity --clear")
    })
}

/// The refusal for a binding the input script cannot press: gamepad buttons and axes.
pub fn binding_error(button: &str, binding: &InputIdentifier) -> Option<String> {
    match binding {
        InputIdentifier::Key(_) | InputIdentifier::Mouse(_) => None,
        InputIdentifier::Gamepad(_) | InputIdentifier::Analog(..) => Some(format!(
            "{button} is bound to a gamepad button; bind a key or mouse button"
        )),
    }
}

/// Whether a press of `button` (as the entries name it: `IaPointer`) would miss the entity's entry
/// for it: the entity declares a `PetDown` entry for the button, and none is in reach.
pub fn press_out_of_reach(button: &str, entries: &[HoverEntryReport]) -> bool {
    let mut declared = entries
        .iter()
        .filter(|entry| {
            entry.event == "PetDown" && (entry.button == button || entry.button == "IaAny")
        })
        .peekable();
    declared.peek().is_some() && declared.all(|entry| !entry.enabled)
}

/// The warning for a click on an entity that declares no cursor entry for `button` (as the
/// entries name it: `IaPointer`) and an event the click sends (`PetDown`, `PetUp`): the press still
/// reaches the entity's results, but no handler bound to a declared entry runs on it.
pub fn undeclared_warning(
    kind: ClickKind,
    button: &str,
    entries: &[HoverEntryReport],
) -> Option<String> {
    let edges: &[(&str, &str)] = match kind {
        ClickKind::Click => &[("press", "PetDown"), ("release", "PetUp")],
        ClickKind::Down => &[("press", "PetDown")],
        ClickKind::Up => &[("release", "PetUp")],
        ClickKind::Hover(_) => &[],
    };
    let listens = if entries.is_empty() {
        "nothing".to_owned()
    } else {
        entries
            .iter()
            .map(|entry| format!("{}/{}", entry.event, entry.button))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let parts: Vec<_> = edges
        .iter()
        .filter(|(_, event)| {
            !entries.iter().any(|entry| {
                entry.event == *event && (entry.button == button || entry.button == "IaAny")
            })
        })
        .map(|(edge, event)| {
            format!(
                "{edge}: the entity declares no PointerEvents entry for {event}/{button}; it listens for: {listens}"
            )
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::{GamepadButton, KeyCode, MouseButton};
    fn seen(id: u32) -> Option<TargetSeen> {
        Some(TargetSeen {
            entity: id,
            container: id,
            ancestors_hit: vec![id],
        })
    }

    #[test]
    fn click_presses_after_the_target_matches_and_releases_on_a_later_tick() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(7), 600, 0);
        assert!(m
            .step(
                2,
                Observed {
                    target: seen(7),
                    scene_tick: Some(10),
                    ..Default::default()
                }
            )
            .contains(&Effect::Press));
        assert!(m
            .step(
                3,
                Observed {
                    target: seen(7),
                    scene_tick: Some(10),
                    down_logged: true,
                    ..Default::default()
                }
            )
            .is_empty());
        assert!(m
            .step(
                4,
                Observed {
                    target: seen(7),
                    scene_tick: Some(11),
                    down_logged: true,
                    ..Default::default()
                }
            )
            .contains(&Effect::Release));
        assert_eq!(
            m.step(
                6,
                Observed {
                    target: seen(7),
                    scene_tick: Some(11),
                    down_logged: true,
                    up_logged: true,
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Ok(()))]
        );
    }

    #[test]
    fn click_fails_when_another_entity_is_hit() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(7), 600, 0);
        let fx = m.step(
            2,
            Observed {
                target: seen(9),
                scene_tick: Some(1),
                entity_live: true,
                ..Default::default()
            },
        );
        assert_eq!(fx, vec![Effect::Finish(Err("blocked by entity 9".into()))]);
    }

    #[test]
    fn job_fails_when_the_entity_is_gone() {
        let mut m = ClickMachine::new(ClickKind::Hover(30), Some(7), 600, 0);
        assert_eq!(
            m.step(
                2,
                Observed {
                    entity_live: false,
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Err("entity not live".into()))]
        );
    }

    #[test]
    fn click_times_out_waiting_for_the_tick() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(7), 10, 0);
        m.step(
            2,
            Observed {
                target: seen(7),
                scene_tick: Some(10),
                entity_live: true,
                ..Default::default()
            },
        );
        assert_eq!(
            m.step(
                11,
                Observed {
                    target: seen(7),
                    scene_tick: Some(10),
                    down_logged: true,
                    entity_live: true,
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Err("timed out waiting for tick".into()))]
        );
    }

    #[test]
    fn hover_finishes_after_its_frames() {
        // Hover(n) finishes at started + 2 + n
        let mut m = ClickMachine::new(ClickKind::Hover(1), Some(7), 600, 0);
        assert!(m
            .step(
                2,
                Observed {
                    target: seen(7),
                    scene_tick: Some(1),
                    entity_live: true,
                    ..Default::default()
                }
            )
            .is_empty());
        assert_eq!(
            m.step(
                3,
                Observed {
                    target: seen(7),
                    scene_tick: Some(1),
                    entity_live: true,
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Ok(()))]
        );
    }

    #[test]
    fn job_refuses_to_replace_an_open_press() {
        assert_eq!(refuse_while_held(Some(7), "/hover_entity"), Some("a press is held on entity 7; send /click_entity 7 --event up or /hover_entity --clear".to_owned()));
        assert_eq!(refuse_while_held(None, "/hover_entity"), None);
    }

    #[test]
    fn job_refuses_a_gamepad_binding() {
        assert_eq!(
            binding_error("primary", &InputIdentifier::Gamepad(GamepadButton::South)),
            Some("primary is bound to a gamepad button; bind a key or mouse button".to_owned())
        );
        assert_eq!(
            binding_error("pointer", &InputIdentifier::Mouse(MouseButton::Left)),
            None
        );
        assert_eq!(
            binding_error("primary", &InputIdentifier::Key(KeyCode::KeyE)),
            None
        );
    }

    #[test]
    fn nothing_is_evaluated_before_the_aim_reaches_the_pointer() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(7), 600, 10);
        assert!(m
            .step(
                11,
                Observed {
                    entity_live: false,
                    ..Default::default()
                }
            )
            .is_empty());
        assert!(m
            .step(
                12,
                Observed {
                    target: seen(7),
                    ..Default::default()
                }
            )
            .contains(&Effect::Press));
    }

    #[test]
    fn an_ancestor_of_the_hit_matches_and_a_miss_waits() {
        let child = Some(TargetSeen {
            entity: 9,
            container: 9,
            ancestors_hit: vec![7],
        });
        let mut m = ClickMachine::new(ClickKind::Hover(0), Some(7), 600, 0);
        assert_eq!(
            m.step(
                2,
                Observed {
                    target: child,
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Ok(()))]
        );
        let mut m = ClickMachine::new(ClickKind::Click, Some(7), 600, 0);
        assert!(m.step(2, Observed::default()).is_empty());
        assert_eq!(
            m.step(600, Observed::default()),
            vec![Effect::Finish(Err("timed out waiting for target".into()))]
        );
        assert!(!m.holds_press());
    }

    #[test]
    fn target_waits_for_a_loading_collider() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(7), 600, 0);
        for frame in 2..6 {
            let fx = m.step(frame, Observed::default());
            assert!(fx.is_empty(), "frame {frame}: {fx:?}");
        }
        assert_eq!(
            m.step(
                6,
                Observed {
                    target: seen(7),
                    ..Default::default()
                }
            ),
            vec![Effect::Press]
        );
    }

    #[test]
    fn click_finishes_on_a_down_only_target() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(7), 600, 0);
        let frame = |tick, down_logged, release_played| Observed {
            target: seen(7),
            scene_tick: Some(tick),
            down_logged,
            release_played,
            ..Default::default()
        };
        assert_eq!(m.step(2, frame(10, false, false)), vec![Effect::Press]);
        assert!(m.step(3, frame(10, true, false)).is_empty());
        assert_eq!(m.step(4, frame(11, true, false)), vec![Effect::Release]);
        // no PetUp entry, so no PetUp is ever logged: the played release is enough
        assert!(m.step(5, frame(11, true, false)).is_empty());
        assert_eq!(
            m.step(6, frame(11, true, true)),
            vec![Effect::Finish(Ok(()))]
        );
        assert!(!m.holds_press());
    }

    #[test]
    fn down_finishes_holding_the_press_and_up_releases_it() {
        let mut down = ClickMachine::new(ClickKind::Down, Some(7), 600, 0);
        assert_eq!(
            down.step(
                2,
                Observed {
                    target: seen(7),
                    scene_tick: Some(1),
                    ..Default::default()
                }
            ),
            vec![Effect::Press]
        );
        assert!(down
            .step(
                3,
                Observed {
                    target: seen(7),
                    scene_tick: Some(1),
                    ..Default::default()
                }
            )
            .is_empty());
        assert_eq!(
            down.step(
                4,
                Observed {
                    target: seen(7),
                    scene_tick: Some(1),
                    down_logged: true,
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Ok(()))]
        );
        assert!(down.holds_press());

        let mut up = ClickMachine::new(ClickKind::Up, Some(7), 600, 10);
        assert!(up.holds_press());
        assert_eq!(up.step(12, Observed::default()), vec![Effect::Release]);
        assert!(!up.holds_press());
        assert!(up.step(13, Observed::default()).is_empty());
        assert_eq!(
            up.step(
                14,
                Observed {
                    up_logged: true,
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Ok(()))]
        );
    }

    #[test]
    fn timeouts_name_the_stage_and_a_failed_press_is_left_to_the_caller() {
        let mut target = ClickMachine::new(ClickKind::Click, Some(7), 2, 0);
        assert_eq!(
            target.step(
                2,
                Observed {
                    target: seen(7),
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Err("timed out waiting for target".into()))]
        );
        let mut release = ClickMachine::new(ClickKind::Up, Some(7), 5, 0);
        release.step(2, Observed::default());
        assert_eq!(
            release.step(5, Observed::default()),
            vec![Effect::Finish(Err("timed out waiting for release".into()))]
        );
        let mut gone = ClickMachine::new(ClickKind::Up, Some(7), 600, 0);
        assert_eq!(
            gone.step(
                2,
                Observed {
                    entity_live: false,
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Err(ENTITY_NOT_LIVE.into()))]
        );
        assert!(gone.holds_press());
        // a finished machine stays finished
        assert!(gone.step(3, Observed::default()).is_empty());
    }

    #[test]
    fn hover_of_1800_frames_finishes_within_its_deadline() {
        let mut m = ClickMachine::new(ClickKind::Hover(1800), Some(7), 600, 0);
        assert!(m
            .step(
                2,
                Observed {
                    target: seen(7),
                    ..Default::default()
                }
            )
            .is_empty());
        assert_eq!(
            m.step(
                1802,
                Observed {
                    target: seen(7),
                    ..Default::default()
                }
            ),
            vec![Effect::Finish(Ok(()))]
        );
    }

    #[test]
    fn frames_over_1800_are_refused() {
        assert_eq!(check_hover_frames(1800), Ok(()));
        assert_eq!(
            check_hover_frames(1801),
            Err("frames must be at most 1800".to_owned())
        );
    }

    fn at_point(point: PointHit, target: Option<TargetSeen>) -> Observed {
        Observed {
            target,
            scene_tick: Some(1),
            point: Some(point),
            ..Default::default()
        }
    }

    #[test]
    fn a_hit_at_the_point_holds_it_and_one_before_it_stands_in_front() {
        let camera = Vec3::ZERO;
        let point = Vec3::new(0.0, 0.0, -10.0);
        // the surface of a box whose centre is the point
        assert_eq!(
            classify_point(camera, point, Some(Vec3::new(0.0, 0.0, -9.5)), false),
            PointHit::At
        );
        // a big box: the surface is far from the centre, but the bounds hold it
        assert_eq!(
            classify_point(camera, point, Some(Vec3::new(0.0, 0.0, -7.0)), true),
            PointHit::At
        );
        assert_eq!(
            classify_point(camera, point, Some(Vec3::new(0.0, 0.0, -6.0)), false),
            PointHit::Before
        );
        assert_eq!(
            classify_point(camera, point, Some(Vec3::new(0.0, 0.0, -30.0)), false),
            PointHit::Beyond
        );
        assert_eq!(
            classify_point(camera, point, None, false),
            PointHit::Nothing
        );
    }

    #[test]
    fn a_point_aim_presses_whatever_entity_of_the_scene_is_at_the_point() {
        let mut m = ClickMachine::new(ClickKind::Click, None, 600, 0);
        assert_eq!(
            m.step(2, at_point(PointHit::At, seen(542))),
            vec![Effect::Press]
        );
    }

    #[test]
    fn nothing_at_the_point_refuses_without_pressing() {
        for miss in [PointHit::Nothing, PointHit::Beyond] {
            let mut m = ClickMachine::new(ClickKind::Click, None, 600, 0);
            let target = (miss == PointHit::Beyond).then(|| seen(9)).flatten();
            assert!(m.step(2, at_point(miss, target.clone())).is_empty());
            assert!(m.step(3, at_point(miss, target.clone())).is_empty());
            assert_eq!(
                m.step(4, at_point(miss, target)),
                vec![Effect::Refuse(NOTHING_AT_POINT.to_owned())]
            );
            assert!(!m.holds_press());
        }
    }

    #[test]
    fn a_point_found_before_the_miss_frames_pass_is_pressed() {
        let mut m = ClickMachine::new(ClickKind::Click, None, 600, 0);
        assert!(m.step(2, at_point(PointHit::Nothing, None)).is_empty());
        assert_eq!(
            m.step(3, at_point(PointHit::At, seen(542))),
            vec![Effect::Press]
        );
    }

    #[test]
    fn a_hit_in_front_of_the_point_blocks_it() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(548), 600, 0);
        assert_eq!(
            m.step(2, at_point(PointHit::Before, seen(549))),
            vec![Effect::Finish(Err("blocked by entity 549".into()))]
        );
    }

    #[test]
    fn a_point_holding_another_entity_than_the_one_named_is_refused() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(543), 600, 0);
        assert_eq!(
            m.step(2, at_point(PointHit::At, seen(542))),
            vec![Effect::Refuse(
                "the point holds entity 542, not entity 543".into()
            )]
        );
        let mut m = ClickMachine::new(ClickKind::Click, Some(542), 600, 0);
        assert_eq!(
            m.step(2, at_point(PointHit::At, seen(542))),
            vec![Effect::Press]
        );
    }

    #[test]
    fn an_avatar_or_another_scene_at_the_point_blocks_it() {
        let mut m = ClickMachine::new(ClickKind::Hover(30), None, 600, 0);
        assert_eq!(
            m.step(2, at_point(PointHit::At, seen(FOREIGN_HIT))),
            vec![Effect::Finish(Err(
                "blocked by an avatar or another scene's entity".into()
            ))]
        );
    }

    fn entry(event: &str, button: &str) -> HoverEntryReport {
        HoverEntryReport {
            button: button.to_owned(),
            event: event.to_owned(),
            hover_text: None,
            enabled: true,
            too_far: false,
        }
    }

    #[test]
    fn a_click_on_an_entity_without_the_buttons_entries_is_warned_per_edge() {
        let primary = [entry("PetDown", "IaPrimary")];
        assert_eq!(
            undeclared_warning(ClickKind::Click, "IaPointer", &primary).as_deref(),
            Some(
                "press: the entity declares no PointerEvents entry for PetDown/IaPointer; it listens for: PetDown/IaPrimary; \
                 release: the entity declares no PointerEvents entry for PetUp/IaPointer; it listens for: PetDown/IaPrimary"
            )
        );
        assert_eq!(
            undeclared_warning(ClickKind::Down, "IaPrimary", &primary),
            None
        );
        assert!(undeclared_warning(ClickKind::Up, "IaPrimary", &primary)
            .is_some_and(|w| w.starts_with("release:")));
        assert!(undeclared_warning(ClickKind::Down, "IaPointer", &[])
            .is_some_and(|w| w.ends_with("it listens for: nothing")));
    }

    #[test]
    fn an_any_button_entry_takes_every_button_and_a_hover_is_never_warned() {
        let any = [entry("PetDown", "IaAny"), entry("PetUp", "IaAny")];
        assert_eq!(
            undeclared_warning(ClickKind::Click, "IaSecondary", &any),
            None
        );
        assert_eq!(
            undeclared_warning(ClickKind::Hover(30), "IaPointer", &[]),
            None
        );
    }

    #[test]
    fn a_press_whose_entries_are_all_too_far_is_out_of_reach() {
        let far = HoverEntryReport {
            enabled: false,
            too_far: true,
            ..entry("PetDown", "IaPointer")
        };
        assert!(press_out_of_reach("IaPointer", std::slice::from_ref(&far)));
        assert!(!press_out_of_reach(
            "IaPointer",
            &[far.clone(), entry("PetDown", "IaAny")]
        ));
        // no entry for the button: nothing to be out of reach of (the warning covers it)
        assert!(!press_out_of_reach("IaSecondary", &[far]));
        assert!(!press_out_of_reach("IaPointer", &[]));
    }

    #[test]
    fn a_click_out_of_reach_ends_without_pressing() {
        let mut m = ClickMachine::new(ClickKind::Click, Some(7), 600, 0);
        let fx = m.step(
            2,
            Observed {
                target: seen(7),
                out_of_reach: true,
                ..Default::default()
            },
        );
        assert_eq!(fx, vec![Effect::Finish(Err(OUT_OF_REACH.into()))]);
        assert!(!m.holds_press());
    }
}
