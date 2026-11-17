//! `/movement_trace`: plays a scripted keyboard scenario on the player and records its movement
//! frame by frame, to compare locomotion across clients, frame rates and builds.
//!
//! A run places the player at a spawn point in the scene it stands in, as an SDK `movePlayerTo`
//! with a facing does, and waits until the player has stood still, grounded and in place, for the
//! scenario's `settle` seconds. The next frame is t = 0. With `--start placed` it waits only the
//! `settle` seconds from the placement and then the first frame the player rests on something
//! (ground, or a slope too steep to stand on), wherever it is, so a run can start on a ledge edge
//! or a steep slope the player slides off. From t = 0 the
//! scenario's keys are pressed by frame time and each frame is recorded until `duration`.
//!
//! The keys enter as window keyboard events ahead of Bevy's input systems, the way the window
//! delivers a real key: bindings, input priorities, the scenes' input events and the movement
//! scene all see what a keyboard would give them. Only the browser's or operating system's key
//! delivery is skipped. With `--input external` nothing is pressed: t = 0 is the first frame with
//! movement input, so a driver's real keys can be recorded on the same timeline.
//!
//! The trace is JSON Lines: a header, one line per frame, a footer (see [`Run::finish`]).
//! Positions are the SDK's, relative to the scene's base parcel; yaw is degrees clockwise from
//! north, in (-180, 180].

use std::f32::consts::FRAC_1_SQRT_2;

use bevy::{
    diagnostic::FrameCount,
    input::{
        keyboard::{Key, KeyboardInput, NativeKey},
        ButtonState, InputSystem,
    },
    prelude::*,
    time::TimeUpdateStrategy,
    window::PrimaryWindow,
};
use bevy_console::ConsoleCommand;
use common::{
    inputs::{Action, CommonInputAction, InputIdentifier, InputMap},
    rpc::{RpcCall, RpcCallEvent},
    sets::{PostUpdateSets, SceneSets},
    structs::{
        AvatarDynamicState, CurrentRealm, PrimaryCamera, PrimaryUser, SessionGraphics, Version,
    },
    util::RingBufferReceiver,
};
use console::{AddDebugSection, DebugSection, DoAddConsoleCommand};
use dcl_component::proto_components::common::Vector3;
use dcl_types::SceneLogMessage;
use input_manager::{InputManager, InputPriority};
use scene_runner::{
    initialize_scene::PARCEL_SIZE,
    renderer_context::{RendererSceneContext, SceneTickTiming},
    ContainingScene, OutOfWorld,
};
use serde::Deserialize;

use crate::{
    avatar_movement::{ActivePlayerComponent, AvatarMovement, LastMovementStep},
    camera::update_camera,
};

/// The trace format this module writes and the scenario format it reads.
pub const FORMAT: u32 = 1;
/// A frame within this much before an event's time reaches it, so an event on a frame boundary
/// (2.0 s at 60 fps) does not move between frames on rounding.
const EVENT_EPSILON: f64 = 1e-4;
/// A frame counts as dropped when it took longer than this many target frame intervals.
const DROPPED_FRAME_FACTOR: f64 = 1.5;
/// Still means slower than this, horizontally (m/s).
const STILL_SPEED: f32 = 1e-3;
/// In place means within this of the spawn point, horizontally (m), ...
const SPAWN_TOLERANCE: f32 = 0.02;
/// ... and facing within this of the spawn yaw (degrees).
const SPAWN_YAW_TOLERANCE: f64 = 1.0;
/// Placed again when still but not in place for this long after a placement (s).
const RESPAWN_AFTER: f64 = 1.0;
const MAX_SPAWNS: u32 = 5;
/// Gives up settling, or waiting for external input, after this long (s).
const WAIT_LIMIT: f64 = 30.0;
/// With `--start placed`, starts anyway when the player is still in the air this long after the
/// scenario's `settle` (s).
const GROUNDED_WAIT: f64 = 5.0;
/// The start of the console line in which the scene controlling movement prints its movement
/// model's state: `LOCO ` and a JSON object, from `console.log('LOCO ' + JSON.stringify(state))`.
const LOCOMOTION_LINE: &str = "LOCO ";

pub struct MovementTracePlugin;

impl Plugin for MovementTracePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementTrace>()
            .add_console_command::<MovementTraceCommand, _>(movement_trace_command)
            .add_systems(PreUpdate, press_scenario_keys.before(InputSystem))
            .add_systems(
                Update,
                (read_consumed_input, hold_camera_yaw.after(update_camera))
                    .in_set(SceneSets::Input),
            )
            .add_systems(
                PostUpdate,
                record_frame
                    .after(PostUpdateSets::CameraUpdate)
                    .before(PostUpdateSets::InverseKinematics),
            )
            .add_debug_section(DebugSection {
                title: "Movement trace",
                query: "/movement_trace status",
                fields: &[
                    ("Phase", "phase"),
                    ("Scenario", "scenario"),
                    ("Time (s)", "t"),
                    ("Frames", "frames"),
                    ("Dropped frames", "dropped"),
                    ("Input mismatch frames", "inputMismatchFrames"),
                    ("Error", "error"),
                ],
                actions: &[("Stop", "/movement_trace stop")],
                inputs: &[],
            });
    }
}

// --- scenario ---

/// A scripted input: a step function of key states over time, from t = 0 to `duration`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Scenario {
    pub format: u32,
    pub id: String,
    #[serde(default)]
    pub seed: i64,
    /// The frame rate to run at, unless the run gives its own
    pub fps: u32,
    /// The camera's yaw for the whole run; movement keys are camera-relative
    #[serde(default)]
    pub camera_yaw: f64,
    /// Seconds still before t = 0
    #[serde(default = "default_settle")]
    pub settle: f64,
    /// Seconds recorded from t = 0
    pub duration: f64,
    /// Each event's keys hold until the next event's time
    pub events: Vec<ScenarioEvent>,
}

fn default_settle() -> f64 {
    1.0
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct ScenarioEvent {
    pub t: f64,
    #[serde(default)]
    pub fwd: i8,
    #[serde(default)]
    pub right: i8,
    #[serde(default)]
    pub walk: bool,
    #[serde(default)]
    pub sprint: bool,
    #[serde(default)]
    pub jump: bool,
}

/// The keys held at a moment: `fwd` W (1) or S (-1), `right` D (1) or A (-1), the walk and
/// sprint modifiers and the jump key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct InputStep {
    pub fwd: i8,
    pub right: i8,
    pub walk: bool,
    pub sprint: bool,
    pub jump: bool,
}

impl InputStep {
    /// The gait these keys ask for: sprint wins over walk, and neither moves without a direction.
    pub fn gait(&self) -> Gait {
        Gait::of(self.fwd != 0 || self.right != 0, self.walk, self.sprint)
    }
}

impl From<&ScenarioEvent> for InputStep {
    fn from(event: &ScenarioEvent) -> Self {
        Self {
            fwd: event.fwd,
            right: event.right,
            walk: event.walk,
            sprint: event.sprint,
            jump: event.jump,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Gait {
    #[default]
    Idle,
    Walk,
    Jog,
    Run,
}

impl Gait {
    fn of(moving: bool, walk: bool, sprint: bool) -> Self {
        match (moving, sprint, walk) {
            (false, _, _) => Gait::Idle,
            (true, true, _) => Gait::Run,
            (true, false, true) => Gait::Walk,
            (true, false, false) => Gait::Jog,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Gait::Idle => "idle",
            Gait::Walk => "walk",
            Gait::Jog => "jog",
            Gait::Run => "run",
        }
    }
}

impl Scenario {
    pub fn parse(json: &str) -> Result<Self, String> {
        let scenario: Scenario =
            serde_json::from_str(json).map_err(|e| format!("not a scenario: {e}"))?;
        scenario.validate()?;
        Ok(scenario)
    }

    fn validate(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!("scenario format {} is not {FORMAT}", self.format));
        }
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err("the id must be letters, digits, '.', '_' or '-'".into());
        }
        if !(1..=1000).contains(&self.fps) {
            return Err(format!("fps {} is outside 1..=1000", self.fps));
        }
        if !(self.duration > 0.0 && self.duration <= 120.0) {
            return Err(format!("duration {} is outside (0, 120]", self.duration));
        }
        if !(0.0..=30.0).contains(&self.settle) {
            return Err(format!("settle {} is outside [0, 30]", self.settle));
        }
        if !self.camera_yaw.is_finite() {
            return Err("camera_yaw is not a number".into());
        }
        if self.events.is_empty() {
            return Err("no events".into());
        }
        let mut previous = 0.0;
        for event in &self.events {
            if event.t.is_nan() || event.t < previous {
                return Err(format!(
                    "event times must be non-negative and in order: {} after {previous}",
                    event.t
                ));
            }
            previous = event.t;
            if !(-1..=1).contains(&event.fwd) || !(-1..=1).contains(&event.right) {
                return Err(format!(
                    "fwd and right are -1, 0 or 1: event at {}",
                    event.t
                ));
            }
        }
        Ok(())
    }

    /// The keys held at `t`: the last event reached, else none.
    pub fn input_at(&self, t: f64) -> InputStep {
        self.events
            .iter()
            .rev()
            .find(|event| t >= event.t - EVENT_EPSILON)
            .map(InputStep::from)
            .unwrap_or_default()
    }

    /// Whether any event holds the jump key. Only such a scenario presses it, compares it and
    /// writes it in a frame's input, so a trace without jumps keeps its earlier form.
    pub fn uses_jump(&self) -> bool {
        self.events.iter().any(|event| event.jump)
    }

    /// Whether the frame at `t` is the last one recorded.
    pub fn ends_at(&self, t: f64) -> bool {
        t >= self.duration - EVENT_EPSILON
    }
}

// --- input ---

/// The movement input the scenes were given this frame, as the movement code reads it: a
/// diagonal is normalised, and the walk and sprint modifiers count only with a direction held.
/// The jump key counts with or without a direction.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct ConsumedInput {
    pub fwd: f32,
    pub right: f32,
    pub gait: Gait,
    pub jump: bool,
}

impl ConsumedInput {
    pub fn from_actions(
        forward: bool,
        backward: bool,
        left: bool,
        right: bool,
        walk: bool,
        sprint: bool,
    ) -> Self {
        let fwd = forward as i8 - backward as i8;
        let side = right as i8 - left as i8;
        let scale = if fwd != 0 && side != 0 {
            FRAC_1_SQRT_2
        } else {
            1.0
        };
        Self {
            fwd: fwd as f32 * scale,
            right: side as f32 * scale,
            gait: Gait::of(fwd != 0 || side != 0, walk, sprint),
            jump: false,
        }
    }

    pub fn with_jump(self, jump: bool) -> Self {
        Self { jump, ..self }
    }

    pub fn matches(&self, expected: &InputStep) -> bool {
        let sign = |v: f32| (v > 0.5) as i8 - (v < -0.5) as i8;
        sign(self.fwd) == expected.fwd
            && sign(self.right) == expected.right
            && self.gait == expected.gait()
            && self.jump == expected.jump
    }

    /// The frame's `in`: `fwd`, `right`, `walk` and `sprint`, and `jump` when the scenario jumps.
    pub fn to_json(&self, with_jump: bool) -> String {
        let jump = match (with_jump, self.jump) {
            (false, _) => "",
            (true, false) => ",\"jump\":false",
            (true, true) => ",\"jump\":true",
        };
        format!(
            "{{\"fwd\":{},\"right\":{},\"walk\":{},\"sprint\":{}{jump}}}",
            num(self.fwd as f64),
            num(self.right as f64),
            self.gait == Gait::Walk,
            self.gait == Gait::Run,
        )
    }
}

/// The keys bound to the movement actions, first keyboard binding of each. Only a scenario that
/// jumps needs a key bound to jump.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementKeys {
    forward: KeyCode,
    backward: KeyCode,
    left: KeyCode,
    right: KeyCode,
    walk: KeyCode,
    sprint: KeyCode,
    jump: Option<KeyCode>,
}

impl MovementKeys {
    pub fn from_map(map: &InputMap) -> Result<Self, String> {
        let bound = |action: CommonInputAction| {
            map.inputs.get(&Action::Scene(action)).and_then(|ids| {
                ids.iter().find_map(|id| match id {
                    InputIdentifier::Key(k) => Some(*k),
                    _ => None,
                })
            })
        };
        let key = |action: CommonInputAction| {
            bound(action).ok_or_else(|| format!("no key is bound to {action:?}"))
        };
        Ok(Self {
            forward: key(CommonInputAction::IaForward)?,
            backward: key(CommonInputAction::IaBackward)?,
            left: key(CommonInputAction::IaLeft)?,
            right: key(CommonInputAction::IaRight)?,
            walk: key(CommonInputAction::IaWalk)?,
            sprint: key(CommonInputAction::IaModifier)?,
            jump: bound(CommonInputAction::IaJump),
        })
    }

    /// These keys, refused when the scenario jumps and no key is bound to jump.
    pub fn for_scenario(self, scenario: &Scenario) -> Result<Self, String> {
        if scenario.uses_jump() && self.jump.is_none() {
            return Err(format!(
                "no key is bound to {:?}",
                CommonInputAction::IaJump
            ));
        }
        Ok(self)
    }

    pub fn pressed(&self, step: &InputStep) -> Vec<KeyCode> {
        let mut keys = Vec::new();
        match step.fwd {
            1 => keys.push(self.forward),
            -1 => keys.push(self.backward),
            _ => (),
        }
        match step.right {
            1 => keys.push(self.right),
            -1 => keys.push(self.left),
            _ => (),
        }
        if step.walk {
            keys.push(self.walk);
        }
        if step.sprint {
            keys.push(self.sprint);
        }
        if let (true, Some(jump)) = (step.jump, self.jump) {
            keys.push(jump);
        }
        keys
    }
}

/// The logical key a keyboard reports with these codes; others are unidentified, which no
/// binding reads.
fn logical_key(code: KeyCode) -> Key {
    let letter = format!("{code:?}")
        .strip_prefix("Key")
        .filter(|l| l.len() == 1)
        .map(str::to_ascii_lowercase);
    if let Some(letter) = letter {
        return Key::Character(letter.as_str().into());
    }
    match code {
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Key::Shift,
        KeyCode::ControlLeft | KeyCode::ControlRight => Key::Control,
        KeyCode::AltLeft | KeyCode::AltRight => Key::Alt,
        KeyCode::Space => Key::Space,
        KeyCode::ArrowUp => Key::ArrowUp,
        KeyCode::ArrowDown => Key::ArrowDown,
        KeyCode::ArrowLeft => Key::ArrowLeft,
        KeyCode::ArrowRight => Key::ArrowRight,
        _ => Key::Unidentified(NativeKey::Unidentified),
    }
}

// --- geometry ---

/// Yaw in the SDK's convention, degrees clockwise from north (+z) in (-180, 180], of something
/// that faces its local -z, as the player and the camera do.
pub fn sdk_yaw(rotation: Quat) -> f64 {
    let forward = rotation * Vec3::NEG_Z;
    normalize_yaw((forward.x as f64).atan2(-forward.z as f64).to_degrees())
}

fn normalize_yaw(yaw: f64) -> f64 {
    let yaw = yaw.rem_euclid(360.0);
    if yaw > 180.0 {
        yaw - 360.0
    } else {
        yaw
    }
}

fn yaw_difference(a: f64, b: f64) -> f64 {
    normalize_yaw(a - b)
}

/// The world direction an SDK yaw faces.
fn yaw_direction(yaw: f64) -> Vec3 {
    let yaw = yaw.to_radians();
    Vec3::new(yaw.sin() as f32, 0.0, -yaw.cos() as f32)
}

fn base_offset(base: IVec2) -> Vec3 {
    Vec3::new(base.x as f32, 0.0, base.y as f32) * PARCEL_SIZE
}

/// An engine translation in SDK axes relative to a scene's base parcel.
pub fn scene_position(translation: Vec3, base: IVec2) -> Vec3 {
    Vec3::new(translation.x, translation.y, -translation.z) - base_offset(base)
}

/// The engine translation of an SDK position relative to a scene's base parcel.
pub fn world_translation(position: Vec3, base: IVec2) -> Vec3 {
    let world = position + base_offset(base);
    Vec3::new(world.x, world.y, -world.z)
}

fn horizontal_speed(v: Option<Vector3>) -> f32 {
    v.map_or(0.0, |v| Vec2::new(v.x, v.z).length())
}

// --- formatting ---

/// The controlling scene's tick timing in a frame's `extra`, each key followed by a comma:
/// `scene_sent` (ms after the frame's scene loop started that the scene was dispatched; null when
/// it was not dispatched this frame), `scene_redispatch` (that dispatch answered a late reply
/// in the frame it landed; null without a dispatch), `scene_reply_late` (for the reply applied
/// this frame, ms after its dispatch frame's deadline that the engine received it, negative
/// before it; null without one) and `scene_reply_frames` (frames from that reply's dispatch to
/// this frame).
fn tick_timing_json(timing: Option<&SceneTickTiming>, frame: u32) -> String {
    let sent = timing
        .and_then(|t| t.sent)
        .filter(|sent| sent.frame == frame);
    let reply = timing
        .and_then(|t| t.reply)
        .filter(|reply| reply.frame == frame);
    format!(
        "\"scene_sent\":{},\"scene_redispatch\":{},\"scene_reply_late\":{},\"scene_reply_frames\":{},",
        sent.map_or("null".into(), |s| num(f64::from(s.after_start_ms()))),
        sent.map_or("null".into(), |s| s.redispatch.to_string()),
        reply.map_or("null".into(), |r| num(f64::from(r.after_deadline_ms()))),
        reply.map_or("null".into(), |r| r.frames().to_string()),
    )
}

fn num(v: f64) -> String {
    if !v.is_finite() {
        return "null".into();
    }
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0');
    let s = if s.ends_with('.') {
        format!("{s}0")
    } else {
        s.to_owned()
    };
    if s == "-0.0" {
        "0.0".into()
    } else {
        s
    }
}

fn vector(v: Vec3) -> String {
    format!(
        "[{},{},{}]",
        num(v.x as f64),
        num(v.y as f64),
        num(v.z as f64)
    )
}

fn sdk_vector(v: Option<Vector3>) -> String {
    let v = v.unwrap_or_default();
    vector(Vec3::new(v.x, v.y, v.z))
}

/// The movement model's state from a scene log line (see [`LOCOMOTION_LINE`]), as compact JSON;
/// `None` for any other line. The log keeps `console.log`'s level, `LOG `; the native runtime
/// writes a string argument JSON-quoted, the web one as it is.
fn locomotion_line(message: &str) -> Option<String> {
    let text = message.strip_prefix("LOG ")?;
    let unquoted;
    let text = if text.starts_with('"') {
        unquoted = serde_json::from_str::<String>(text).ok()?;
        unquoted.as_str()
    } else {
        text
    };
    let state: serde_json::Value =
        serde_json::from_str(text.strip_prefix(LOCOMOTION_LINE)?).ok()?;
    state.is_object().then(|| state.to_string())
}

fn utc_now() -> String {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::from_timestamp(now.as_secs() as i64, now.subsec_nanos())
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

// --- the run ---

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputSource {
    /// The run presses the scenario's keys
    Keys(MovementKeys),
    /// The run waits for movement input from outside, and starts on it
    External,
}

/// When a run's t = 0 comes after the placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StartCondition {
    /// Once the player has stood still, grounded and at the spawn for the scenario's `settle`;
    /// placed again when it does not stay there
    #[default]
    Still,
    /// The first frame resting on ground or a steep slope at least `settle` after the placement,
    /// or the first frame [`GROUNDED_WAIT`] later, wherever the player is: it may be sliding or
    /// falling
    Placed,
}

impl StartCondition {
    fn name(&self) -> &'static str {
        match self {
            StartCondition::Still => "still",
            StartCondition::Placed => "placed",
        }
    }
}

/// Whether a `--start placed` run starts in a frame `since_placed` seconds after the placement.
/// The placement's own frame never does: its ground state may still be the old position's.
/// `landed`: the player rests on ground or on a slope too steep to stand on.
fn placed_start(since_placed: f64, settle: f64, landed: bool) -> bool {
    since_placed > 0.0
        && since_placed >= settle
        && (landed || since_placed >= settle + GROUNDED_WAIT)
}

#[derive(Debug, PartialEq)]
enum Phase {
    /// The player is placed in the next frame's `PreUpdate`
    Spawning,
    Settling {
        placed_at: f64,
        still_since: Option<f64>,
    },
    /// Settled: the next frame is t = 0
    Starting,
    /// Settled: t = 0 is the first frame with movement input
    Armed {
        since: f64,
    },
    Running {
        t0: f64,
    },
}

impl Phase {
    fn name(&self) -> &'static str {
        match self {
            Phase::Spawning => "spawning",
            Phase::Settling { .. } => "settling",
            Phase::Starting => "starting",
            Phase::Armed { .. } => "armed",
            Phase::Running { .. } => "running",
        }
    }
}

struct Run {
    scenario: Scenario,
    fps: u32,
    repeat: u32,
    input: InputSource,
    start: StartCondition,
    fixed_dt: bool,
    /// The session frame-rate setting to put back afterwards
    previous_fps: Option<usize>,
    base: IVec2,
    spawn: Vec3,
    spawn_yaw: f64,
    realm: String,
    scene: String,
    client_version: String,
    animations: Option<bool>,
    camera_mode: &'static str,
    phase: Phase,
    spawns: u32,
    settle_started: f64,
    started_utc: String,
    /// this frame's input as read in `Update`
    consumed: Option<ConsumedInput>,
    frames: Vec<String>,
    dropped: u32,
    mismatches: u32,
    scene_ticks: u32,
    /// the log of the scene controlling movement, read for its movement model's state
    scene_log: Option<(Entity, RingBufferReceiver<SceneLogMessage>)>,
    /// the latest movement model state that scene printed (JSON), `None` before the first
    locomotion: Option<String>,
    /// where the player stood when the run started
    start_position: Option<Vec3>,
    last_position: Vec3,
    last_t: f64,
}

impl Run {
    fn position_label(&self) -> String {
        format!(
            "{} scene \"{}\" base {},{} spawn {},{},{} yaw {}",
            self.realm,
            self.scene,
            self.base.x,
            self.base.y,
            num(self.spawn.x as f64),
            num(self.spawn.y as f64),
            num(self.spawn.z as f64),
            num(self.spawn_yaw),
        )
    }

    /// The trace: a header line, the frames, a footer line.
    ///
    /// - header: `type`, `format`, `client` (`bevy`), `client_version`, `scenario` (its id),
    ///   `seed`, `fps_target`, `animations` (whether the player's own avatar animated), `position`
    ///   (realm, scene, base parcel and spawn), `started_utc` (t = 0) and `extra`: the repeat,
    ///   the scenario's `camera_yaw`, `settle` and `duration`, `input_seam` (`keyboard`: keys
    ///   pressed by the run; `external`: keys from outside), `dt_mode` (`real`, or `fixed` at
    ///   1/fps), `camera_mode`, `scene_base_parcel`, `spawn`, `spawn_yaw`, `start_pos` (where
    ///   the player stood before t = 0) and, for a run with `--start placed`, `start`
    ///   (`placed`: the player need not have been still or at the spawn at t = 0).
    /// - frame: `i`, `t` and `dt` (s, the frame clock), `in` (the movement input given this
    ///   frame: `fwd` and `right`, normalised on a diagonal, `walk` and `sprint` from the gait,
    ///   and `jump` when the scenario has a jump),
    ///   `pos` (after this frame's movement), `vel` (this frame's displacement over its delta),
    ///   `grounded` (the feet within 0.05 m of ground the player can stand on: not a wall it
    ///   grazes or a slope steeper than 46.5 degrees), `yaw_avatar`, `yaw_camera`, and `extra`:
    ///   the `kind` (gait), `vel_requested`
    ///   (by the movement scene), `vel_external` (SDK forces and impulses), `scene_tick` (1
    ///   when the controlling scene's reply landed this frame, 0 when the frame reused its last
    ///   one), `scene_sent`, `scene_reply_late` and `scene_reply_frames` (the scene's dispatch
    ///   and reply times against the frame's deadline, [`tick_timing_json`]) and `locomotion`
    ///   (the latest movement model state the controlling scene printed in a
    ///   `LOCO {...}` console line, as printed; `null` before the first).
    /// - footer: `frames`, `dropped` (frames after the first longer than 1.5 target intervals)
    ///   and `extra.input_mismatch_frames` (frames whose input differs from the scenario's) and
    ///   `extra.scene_tick_frames`.
    fn finish(&self) -> String {
        let s = &self.scenario;
        let animations = match self.animations {
            Some(enabled) => enabled.to_string(),
            None => "null".into(),
        };
        let header = format!(
            concat!(
                "{{\"type\":\"header\",\"format\":{},\"client\":\"bevy\",\"client_version\":{},",
                "\"scenario\":{},\"seed\":{},\"fps_target\":{},\"animations\":{},\"position\":{},",
                "\"started_utc\":{},\"extra\":{{\"repeat\":{},\"camera_yaw\":{},\"settle\":{},",
                "\"duration\":{},\"input_seam\":\"{}\",\"dt_mode\":\"{}\",\"camera_mode\":\"{}\",",
                "\"scene_base_parcel\":[{},{}],\"spawn\":{},\"spawn_yaw\":{},\"start_pos\":{}{}}}}}"
            ),
            FORMAT,
            serde_json::Value::from(self.client_version.as_str()),
            serde_json::Value::from(s.id.as_str()),
            s.seed,
            self.fps,
            animations,
            serde_json::Value::from(self.position_label()),
            serde_json::Value::from(self.started_utc.as_str()),
            self.repeat,
            num(s.camera_yaw),
            num(s.settle),
            num(s.duration),
            match self.input {
                InputSource::Keys(_) => "keyboard",
                InputSource::External => "external",
            },
            if self.fixed_dt { "fixed" } else { "real" },
            self.camera_mode,
            self.base.x,
            self.base.y,
            vector(self.spawn),
            num(self.spawn_yaw),
            self.start_position.map_or("null".into(), vector),
            // the earlier header form unless the run started on placement
            match self.start {
                StartCondition::Still => "",
                StartCondition::Placed => ",\"start\":\"placed\"",
            },
        );
        let footer = format!(
            "{{\"type\":\"footer\",\"frames\":{},\"dropped\":{},\"extra\":{{\"input_mismatch_frames\":{},\"scene_tick_frames\":{}}}}}",
            self.frames.len(),
            self.dropped,
            self.mismatches,
            self.scene_ticks,
        );
        let mut text = String::with_capacity(400 * (self.frames.len() + 2));
        for line in std::iter::once(&header)
            .chain(self.frames.iter())
            .chain(std::iter::once(&footer))
        {
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    fn file_name(&self) -> String {
        format!(
            "{}__fps{}__r{}.jsonl",
            self.scenario.id, self.fps, self.repeat
        )
    }

    fn summary(&self) -> serde_json::Value {
        let distance = self
            .start_position
            .map(|first| (self.last_position - first) * Vec3::new(1.0, 0.0, 1.0))
            .map(|d| d.length());
        serde_json::json!({
            "scenario": self.scenario.id,
            "repeat": self.repeat,
            "fps": self.fps,
            "dtMode": if self.fixed_dt { "fixed" } else { "real" },
            "frames": self.frames.len(),
            "t": self.last_t,
            "dropped": self.dropped,
            "inputMismatchFrames": self.mismatches,
            "sceneTickFrames": self.scene_ticks,
            "distance": distance,
            "file": self.file_name(),
        })
    }
}

struct Finished {
    text: String,
    summary: serde_json::Value,
}

/// The recorder's state: the run in progress, the last run's result and the keys it holds.
#[derive(Resource, Default)]
pub struct MovementTrace {
    run: Option<Run>,
    last: Option<Result<Finished, String>>,
    /// keys pressed by the run and not yet released
    held: Vec<KeyCode>,
}

impl MovementTrace {
    fn status(&self) -> serde_json::Value {
        let (phase, error) = match (&self.run, &self.last) {
            (Some(run), _) => (run.phase.name(), None),
            (None, Some(Ok(_))) => ("done", None),
            (None, Some(Err(e))) => ("failed", Some(e.clone())),
            (None, None) => ("idle", None),
        };
        let mut status = serde_json::json!({ "phase": phase, "error": error });
        let summary = match (&self.run, &self.last) {
            (Some(run), _) => Some(run.summary()),
            (None, Some(Ok(finished))) => Some(finished.summary.clone()),
            _ => None,
        };
        if let (Some(summary), Some(status)) = (summary, status.as_object_mut()) {
            if let Some(summary) = summary.as_object() {
                status.extend(summary.clone());
            }
        }
        status["unavailable"] = serde_json::json!({
            "": "no run yet: /movement_trace play <scenario>",
            "error": "no error",
        });
        status
    }

    /// Ends the run in progress, keeping `result` as the last one, and puts the session's frame
    /// rate and clock back. The keys are released in the next frame's `PreUpdate`.
    fn end(
        &mut self,
        result: Result<(), String>,
        session: Option<&mut SessionGraphics>,
        strategy: Option<&mut TimeUpdateStrategy>,
    ) {
        let Some(run) = self.run.take() else {
            return;
        };
        if let Some(session) = session {
            session.fps_target = run.previous_fps;
        }
        if let (true, Some(strategy)) = (run.fixed_dt, strategy) {
            *strategy = TimeUpdateStrategy::Automatic;
        }
        self.last = Some(result.map(|()| Finished {
            text: run.finish(),
            summary: run.summary(),
        }));
    }
}

// --- console ---

/// Play a movement scenario on the player and record each frame (`play`), report the run
/// (`status`), return the last trace as JSON Lines (`fetch`) or abandon the run (`stop`). `play`
/// answers once the run has started; poll `status` until `done` or `failed`
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/movement_trace")]
struct MovementTraceCommand {
    /// `play`, `status`, `fetch` or `stop`
    action: String,
    /// play: the frame rate, instead of the scenario's
    #[arg(long)]
    fps: Option<u32>,
    /// play: `real` frame times, or `fixed` (each frame advances the clock by exactly 1/fps)
    #[arg(long)]
    dt: Option<String>,
    /// play: `x,y,z[,yaw]` relative to the base parcel of the scene the player is in; yaw defaults
    /// to the scenario's camera yaw. Without it the player is placed where it stands
    #[arg(long, allow_hyphen_values = true)]
    spawn: Option<String>,
    /// play: the repeat number the trace carries (1 by default)
    #[arg(long)]
    repeat: Option<u32>,
    /// play: `keys` (the run presses the scenario's keys) or `external` (the run starts on the
    /// first movement input from outside)
    #[arg(long)]
    input: Option<String>,
    /// play: `still` (t = 0 once the player has stood still, grounded and at the spawn for the
    /// scenario's `settle`) or `placed` (t = 0 on the first frame resting on ground or a steep
    /// slope `settle` seconds after the placement, wherever the player is then)
    #[arg(long)]
    start: Option<String>,
    /// play: the scenario as JSON
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    json: Vec<String>,
}

fn parse_spawn(value: &str) -> Result<(Vec3, Option<f64>), String> {
    let parts = value
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("spawn `{value}` is not x,y,z[,yaw]"))?;
    match parts[..] {
        [x, y, z] => Ok((Vec3::new(x as f32, y as f32, z as f32), None)),
        [x, y, z, yaw] => Ok((Vec3::new(x as f32, y as f32, z as f32), Some(yaw))),
        _ => Err(format!("spawn `{value}` is not x,y,z[,yaw]")),
    }
}

#[allow(clippy::too_many_arguments)]
fn movement_trace_command(
    mut cmd: ConsoleCommand<MovementTraceCommand>,
    mut trace: ResMut<MovementTrace>,
    map: Res<InputMap>,
    time: Res<Time>,
    player: Query<(Entity, &Transform, Has<OutOfWorld>), With<PrimaryUser>>,
    camera: Query<&PrimaryCamera>,
    containing: ContainingScene,
    scenes: Query<&RendererSceneContext>,
    realm: Res<CurrentRealm>,
    version: Option<Res<Version>>,
    animations: Option<Res<avatar::animate::OwnAvatarAnimations>>,
    mut session: Option<ResMut<SessionGraphics>>,
    mut strategy: Option<ResMut<TimeUpdateStrategy>>,
) {
    let Some(Ok(command)) = cmd.take() else {
        return;
    };
    match command.action.as_str() {
        "status" => cmd.reply_ok(trace.status().to_string()),
        "fetch" => match &trace.last {
            Some(Ok(finished)) => cmd.reply_ok(finished.text.clone()),
            Some(Err(e)) => cmd.reply_failed(format!("the last run failed: {e}")),
            None => cmd.reply_failed("no finished run"),
        },
        "stop" => {
            if trace.run.is_some() {
                trace.end(
                    Err("stopped".into()),
                    session.as_deref_mut(),
                    strategy.as_deref_mut(),
                );
                cmd.reply_ok("stopped");
            } else {
                cmd.reply_ok("no run in progress");
            }
        }
        "play" => {
            if let Some(run) = &trace.run {
                cmd.reply_failed(format!(
                    "a run is {}: /movement_trace stop first",
                    run.phase.name()
                ));
                return;
            }
            let result = (|| {
                let scenario = Scenario::parse(&command.json.join(" "))?;
                let fps = command.fps.unwrap_or(scenario.fps);
                if !(1..=1000).contains(&fps) {
                    return Err(format!("fps {fps} is outside 1..=1000"));
                }
                let fixed_dt = match command.dt.as_deref() {
                    None | Some("real") => false,
                    Some("fixed") => true,
                    Some(other) => return Err(format!("dt `{other}`: use real or fixed")),
                };
                let input = match command.input.as_deref() {
                    None | Some("keys") => {
                        InputSource::Keys(MovementKeys::from_map(&map)?.for_scenario(&scenario)?)
                    }
                    Some("external") => InputSource::External,
                    Some(other) => return Err(format!("input `{other}`: use keys or external")),
                };
                let start = match command.start.as_deref() {
                    None | Some("still") => StartCondition::Still,
                    Some("placed") => StartCondition::Placed,
                    Some(other) => return Err(format!("start `{other}`: use still or placed")),
                };
                let (player, transform, out_of_world) =
                    player.single().map_err(|_| "no player".to_string())?;
                if out_of_world {
                    return Err("the player is not in the world yet".into());
                }
                // the parcel scene the player stands in; an empty parcel is its own base
                let scene = containing
                    .get(player)
                    .into_iter()
                    .filter_map(|e| scenes.get(e).ok())
                    .find(|ctx| !ctx.is_portable);
                let (base, scene_title) = match scene {
                    Some(ctx) => (ctx.base, ctx.title.clone()),
                    None => (
                        (Vec2::new(transform.translation.x, -transform.translation.z)
                            / PARCEL_SIZE)
                            .floor()
                            .as_ivec2(),
                        "empty parcel".to_string(),
                    ),
                };
                let (spawn, spawn_yaw) = match command.spawn.as_deref().map(parse_spawn) {
                    Some(parsed) => parsed?,
                    None => (scene_position(transform.translation, base), None),
                };
                let camera_mode = match camera.single() {
                    Ok(camera) if camera.distance < 0.05 => "first_person",
                    Ok(_) => "third_person",
                    Err(_) => "none",
                };
                Ok(Run {
                    spawn_yaw: spawn_yaw
                        .map(normalize_yaw)
                        .unwrap_or(normalize_yaw(scenario.camera_yaw)),
                    scenario,
                    fps,
                    repeat: command.repeat.unwrap_or(1),
                    input,
                    start,
                    fixed_dt,
                    previous_fps: session.as_ref().and_then(|s| s.fps_target),
                    base,
                    spawn,
                    realm: realm
                        .config
                        .realm_name
                        .clone()
                        .unwrap_or_else(|| realm.address.clone()),
                    scene: scene_title,
                    client_version: version.as_ref().map(|v| v.0.clone()).unwrap_or_default(),
                    animations: animations.as_ref().map(|a| a.enabled),
                    camera_mode,
                    phase: Phase::Spawning,
                    spawns: 0,
                    settle_started: time.elapsed_secs_f64(),
                    started_utc: String::new(),
                    consumed: None,
                    frames: Vec::new(),
                    dropped: 0,
                    mismatches: 0,
                    scene_ticks: 0,
                    scene_log: None,
                    locomotion: None,
                    start_position: None,
                    last_position: Vec3::ZERO,
                    last_t: 0.0,
                })
            })();
            match result {
                Ok(run) => {
                    if let Some(session) = session.as_deref_mut() {
                        session.fps_target = Some(run.fps as usize);
                    }
                    if let (true, Some(strategy)) = (run.fixed_dt, strategy.as_deref_mut()) {
                        *strategy = TimeUpdateStrategy::ManualDuration(
                            std::time::Duration::from_secs_f64(1.0 / run.fps as f64),
                        );
                    }
                    let reply = serde_json::json!({
                        "started": run.scenario.id,
                        "fps": run.fps,
                        "base": [run.base.x, run.base.y],
                        "scene": run.scene,
                        "spawn": [run.spawn.x, run.spawn.y, run.spawn.z],
                        "spawnYaw": run.spawn_yaw,
                        "start": run.start.name(),
                    });
                    trace.last = None;
                    trace.run = Some(run);
                    cmd.reply_ok(reply.to_string());
                }
                Err(e) => cmd.reply_failed(e),
            }
        }
        other => cmd.reply_failed(format!(
            "unknown action `{other}`: use play, status, fetch or stop"
        )),
    }
}

// --- systems ---

/// Before Bevy's input systems: places the player when a run asks, starts the clock at t = 0
/// and presses or releases keys so the keys held are the scenario's at this frame's time. Keys
/// the run holds are released when no run is pressing them.
fn press_scenario_keys(
    mut trace: ResMut<MovementTrace>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    window: Query<Entity, With<PrimaryWindow>>,
    mut keyboard: EventWriter<KeyboardInput>,
    mut rpc: EventWriter<RpcCallEvent>,
) {
    let now = time.elapsed_secs_f64();
    let trace = &mut *trace;
    let mut desired = Vec::new();
    if let Some(run) = trace.run.as_mut() {
        match run.phase {
            Phase::Spawning => {
                let to = world_translation(run.spawn, run.base);
                rpc.write(RpcCallEvent::engine(RpcCall::MovePlayer {
                    to,
                    looking_at: Some(to + yaw_direction(run.spawn_yaw)),
                    duration: None,
                    camera_rotation: None,
                    response: None,
                }));
                run.spawns += 1;
                run.phase = Phase::Settling {
                    placed_at: now,
                    still_since: None,
                };
            }
            Phase::Starting => {
                run.phase = Phase::Running { t0: now };
                run.started_utc = utc_now();
            }
            _ => (),
        }
        if let (Phase::Running { t0 }, InputSource::Keys(movement_keys)) = (&run.phase, &run.input)
        {
            desired = movement_keys.pressed(&run.scenario.input_at(now - t0));
        }
    }

    let window = window.single().unwrap_or(Entity::PLACEHOLDER);
    let mut event = |key_code: KeyCode, state: ButtonState| {
        keyboard.write(KeyboardInput {
            key_code,
            logical_key: logical_key(key_code),
            state,
            text: None,
            repeat: false,
            window,
        });
    };
    trace.held.retain(|key| {
        let keep = desired.contains(key);
        if !keep {
            event(*key, ButtonState::Released);
        }
        keep
    });
    for key in desired {
        // pressed again if something released it, such as the window losing focus
        if !keys.pressed(key) {
            event(key, ButtonState::Pressed);
        }
        if !trace.held.contains(&key) {
            trace.held.push(key);
        }
    }
}

/// The movement input the scenes are given this frame.
fn read_consumed_input(mut trace: ResMut<MovementTrace>, input: InputManager) {
    let Some(run) = trace.run.as_mut() else {
        return;
    };
    let down = |action| input.is_down(action, InputPriority::Scene);
    run.consumed = Some(
        ConsumedInput::from_actions(
            down(CommonInputAction::IaForward),
            down(CommonInputAction::IaBackward),
            down(CommonInputAction::IaLeft),
            down(CommonInputAction::IaRight),
            down(CommonInputAction::IaWalk),
            down(CommonInputAction::IaModifier),
        )
        .with_jump(down(CommonInputAction::IaJump)),
    );
}

/// Holds the camera at the scenario's yaw for the whole run, settling included.
fn hold_camera_yaw(trace: Res<MovementTrace>, mut camera: Query<&mut PrimaryCamera>) {
    let Some(run) = trace.run.as_ref() else {
        return;
    };
    if let Ok(mut camera) = camera.single_mut() {
        // the camera faces its local -z: an SDK yaw is the negated engine yaw
        camera.yaw = -(run.scenario.camera_yaw.to_radians() as f32);
    }
}

/// After the player's movement and the camera: settles, then records the frame.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn record_frame(
    mut trace: ResMut<MovementTrace>,
    time: Res<Time>,
    frame: Res<FrameCount>,
    player: Query<
        (
            &Transform,
            &AvatarDynamicState,
            &ActivePlayerComponent<AvatarMovement>,
            Has<OutOfWorld>,
        ),
        With<PrimaryUser>,
    >,
    camera: Query<&Transform, With<PrimaryCamera>>,
    step: Res<LastMovementStep>,
    scenes: Query<&RendererSceneContext>,
    mut session: Option<ResMut<SessionGraphics>>,
    mut strategy: Option<ResMut<TimeUpdateStrategy>>,
) {
    let now = time.elapsed_secs_f64();
    let Some(run) = trace.run.as_mut() else {
        return;
    };
    let Ok((transform, dynamics, movement, out_of_world)) = player.single() else {
        return;
    };
    if out_of_world {
        trace.end(
            Err("the player left the world".into()),
            session.as_deref_mut(),
            strategy.as_deref_mut(),
        );
        return;
    }
    let position = scene_position(transform.translation, run.base);
    let yaw_avatar = sdk_yaw(transform.rotation);
    let grounded = dynamics.ground_height <= common::dynamics::PLAYER_GROUND_THRESHOLD;
    // resting on something, ground or a slope too steep to stand on
    let landed = dynamics.contact_height <= common::dynamics::PLAYER_GROUND_THRESHOLD;
    let uses_jump = run.scenario.uses_jump();
    // a scenario without jumps neither compares nor writes the jump key
    let consumed = run.consumed.take().unwrap_or_default();
    let consumed = consumed.with_jump(uses_jump && consumed.jump);

    // the controlling scene's console: keep the latest movement model state it printed
    let scene = movement.scene();
    if run
        .scene_log
        .as_ref()
        .is_none_or(|(log_scene, _)| *log_scene != scene)
    {
        run.scene_log = scenes.get(scene).ok().map(|ctx| {
            let (_, backlog, receiver) = ctx.logs.read();
            if let Some(state) = backlog
                .iter()
                .rev()
                .find_map(|l| locomotion_line(&l.message))
            {
                run.locomotion = Some(state);
            }
            (scene, receiver)
        });
    }
    if let Some((_, log)) = run.scene_log.as_mut() {
        while let Ok(line) = log.try_recv() {
            if let Some(state) = locomotion_line(&line.message) {
                run.locomotion = Some(state);
            }
        }
    }

    let mut outcome = None;
    match run.phase {
        Phase::Spawning | Phase::Starting => (),
        Phase::Settling {
            placed_at,
            ref mut still_since,
        } => {
            let settled = match run.start {
                StartCondition::Placed => {
                    placed_start(now - placed_at, run.scenario.settle, landed)
                }
                StartCondition::Still => {
                    let still = grounded
                        && horizontal_speed(step.actual_velocity) < STILL_SPEED
                        && horizontal_speed(step.requested_velocity) < STILL_SPEED;
                    let in_place = (position.xz() - run.spawn.xz()).length() < SPAWN_TOLERANCE
                        && yaw_difference(yaw_avatar, run.spawn_yaw).abs() < SPAWN_YAW_TOLERANCE;
                    if still && in_place {
                        let since = *still_since.get_or_insert(now);
                        now - since >= run.scenario.settle
                    } else {
                        *still_since = None;
                        if still && now - placed_at > RESPAWN_AFTER {
                            if run.spawns >= MAX_SPAWNS {
                                outcome = Some(Err(format!(
                                    "the player does not stay at the spawn: at {} facing {}",
                                    vector(position),
                                    num(yaw_avatar)
                                )));
                            } else {
                                run.phase = Phase::Spawning;
                            }
                        }
                        false
                    }
                }
            };
            if settled {
                run.start_position = Some(position);
                run.phase = match run.input {
                    InputSource::Keys(_) => Phase::Starting,
                    InputSource::External => Phase::Armed { since: now },
                };
            }
            if now - run.settle_started > WAIT_LIMIT && outcome.is_none() {
                outcome = Some(Err(format!(
                    "the player did not settle in {WAIT_LIMIT} s: at {} facing {}, grounded {grounded}",
                    vector(position),
                    num(yaw_avatar)
                )));
            }
        }
        Phase::Armed { since } => {
            if consumed.gait != Gait::Idle || consumed.jump {
                run.phase = Phase::Running { t0: now };
                run.started_utc = utc_now();
            } else if now - since > WAIT_LIMIT {
                outcome = Some(Err(format!("no movement input in {WAIT_LIMIT} s")));
            }
        }
        Phase::Running { .. } => (),
    }

    if let Phase::Running { t0 } = run.phase {
        let t = now - t0;
        let dt = time.delta_secs_f64();
        let i = run.frames.len();
        let expected = run.scenario.input_at(t);
        if i > 0 && dt > DROPPED_FRAME_FACTOR / run.fps as f64 {
            run.dropped += 1;
        }
        if !consumed.matches(&expected) {
            run.mismatches += 1;
        }
        let scene_ctx = scenes.get(movement.scene()).ok();
        let scene_tick = scene_ctx.is_some_and(|ctx| ctx.last_update_frame == frame.0);
        run.scene_ticks += scene_tick as u32;
        let yaw_camera = camera
            .single()
            .map(|t| sdk_yaw(t.rotation))
            .unwrap_or(f64::NAN);
        run.frames.push(format!(
            concat!(
                "{{\"type\":\"frame\",\"i\":{},\"t\":{},\"dt\":{},\"in\":{},",
                "\"pos\":{},\"vel\":{},\"grounded\":{},",
                "\"yaw_avatar\":{},\"yaw_camera\":{},\"extra\":{{\"kind\":\"{}\",",
                "\"vel_requested\":{},\"vel_external\":{},\"scene_tick\":{},{}",
                "\"locomotion\":{}}}}}"
            ),
            i,
            num(t),
            num(dt),
            consumed.to_json(uses_jump),
            vector(position),
            sdk_vector(step.actual_velocity),
            grounded,
            num(yaw_avatar),
            num(yaw_camera),
            consumed.gait.name(),
            sdk_vector(step.requested_velocity),
            sdk_vector(step.external_velocity),
            scene_tick as u8,
            tick_timing_json(scene_ctx.map(|ctx| &ctx.tick_timing), frame.0),
            run.locomotion.as_deref().unwrap_or("null"),
        ));
        run.last_position = position;
        run.last_t = t;
        if run.scenario.ends_at(t) {
            outcome = Some(Ok(()));
        }
    }

    if let Some(outcome) = outcome {
        trace.end(outcome, session.as_deref_mut(), strategy.as_deref_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::InputPlugin;
    use std::time::Duration;

    const JOG: &str = r#"{
      "format": 1, "id": "jog-W-hold2.0-release", "seed": 1, "fps": 60, "camera_yaw": 0,
      "settle": 1.0, "duration": 4.0,
      "events": [
        { "t": 0.0, "fwd": 1, "right": 0, "walk": false, "sprint": false },
        { "t": 2.0, "fwd": 0, "right": 0, "walk": false, "sprint": false }
      ]
    }"#;

    #[test]
    fn a_scenario_parses_with_its_defaults() {
        let scenario = Scenario::parse(JOG).unwrap();
        assert_eq!(scenario.id, "jog-W-hold2.0-release");
        assert_eq!(scenario.fps, 60);
        assert_eq!(scenario.events.len(), 2);
        let bare = Scenario::parse(
            r#"{"format":1,"id":"x","fps":30,"duration":1,"events":[{"t":0,"right":-1}]}"#,
        )
        .unwrap();
        assert_eq!(bare.settle, 1.0);
        assert_eq!(bare.camera_yaw, 0.0);
        assert_eq!(
            bare.input_at(0.0),
            InputStep {
                right: -1,
                ..default()
            }
        );
    }

    #[test]
    fn a_malformed_scenario_is_refused() {
        let with = |field: &str, value: &str| {
            let mut json: serde_json::Value = serde_json::from_str(JOG).unwrap();
            json[field] = serde_json::from_str(value).unwrap();
            Scenario::parse(&json.to_string())
        };
        assert!(with("format", "2").is_err());
        assert!(with("id", "\"../x\"").is_err());
        assert!(with("fps", "0").is_err());
        assert!(with("duration", "0").is_err());
        assert!(with("events", "[]").is_err());
        assert!(with("events", r#"[{"t":1},{"t":0.5}]"#).is_err());
        assert!(with("events", r#"[{"t":0,"fwd":2}]"#).is_err());
        assert!(Scenario::parse("{").is_err());
    }

    #[test]
    fn an_event_is_reached_a_tenth_of_a_millisecond_early() {
        let scenario = Scenario::parse(JOG).unwrap();
        let w = InputStep {
            fwd: 1,
            ..default()
        };
        assert_eq!(scenario.input_at(1.9998), w);
        assert_eq!(scenario.input_at(1.99995), InputStep::default());
        assert_eq!(scenario.input_at(-1.0), InputStep::default());
        assert!(!scenario.ends_at(3.9998));
        assert!(scenario.ends_at(3.99995));
    }

    #[test]
    fn consumed_input_is_normalised_and_gaited_as_the_movement_code_reads_it() {
        let diagonal = ConsumedInput::from_actions(true, false, false, true, false, true);
        assert!((diagonal.fwd - FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((diagonal.right - FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(diagonal.gait, Gait::Run);
        assert!(diagonal.matches(&InputStep {
            fwd: 1,
            right: 1,
            walk: false,
            sprint: true,
            jump: false
        }));
        // a modifier alone moves nothing
        let walk_only = ConsumedInput::from_actions(false, false, false, false, true, false);
        assert_eq!(walk_only.gait, Gait::Idle);
        assert!(walk_only.matches(&InputStep {
            walk: true,
            ..default()
        }));
        // sprint wins over walk
        let both = ConsumedInput::from_actions(false, true, false, false, true, true);
        assert_eq!((both.fwd, both.gait), (-1.0, Gait::Run));
        assert!(!both.matches(&InputStep {
            fwd: -1,
            walk: true,
            ..default()
        }));
    }

    #[test]
    fn the_default_bindings_give_wasd_ctrl_shift_and_space() {
        let keys = MovementKeys::from_map(&InputMap::default()).unwrap();
        assert_eq!(
            keys.pressed(&InputStep {
                fwd: -1,
                right: 1,
                walk: true,
                sprint: true,
                jump: true
            }),
            vec![
                KeyCode::KeyS,
                KeyCode::KeyD,
                KeyCode::ControlLeft,
                KeyCode::ShiftLeft,
                KeyCode::Space
            ]
        );
        assert_eq!(logical_key(KeyCode::KeyW), Key::Character("w".into()));
        assert_eq!(logical_key(KeyCode::Space), Key::Space);
    }

    #[test]
    fn jump_is_a_held_key_off_by_default() {
        let scenario = Scenario::parse(
            r#"{"format":1,"id":"j","fps":60,"duration":2,
                "events":[{"t":0,"fwd":1},{"t":0.5,"fwd":1,"jump":true},{"t":0.6,"fwd":1,"jump":false}]}"#,
        )
        .unwrap();
        assert!(scenario.uses_jump());
        assert!(!Scenario::parse(JOG).unwrap().uses_jump());
        assert!(!scenario.input_at(0.4).jump);
        assert!(scenario.input_at(0.55).jump);
        assert!(!scenario.input_at(0.6).jump);
        let mut json: serde_json::Value = serde_json::from_str(JOG).unwrap();
        json["events"][0]["jump"] = "yes".into();
        assert!(Scenario::parse(&json.to_string()).is_err());
    }

    #[test]
    fn a_scenario_that_jumps_needs_a_jump_key() {
        let jumping = Scenario::parse(
            r#"{"format":1,"id":"j","fps":60,"duration":1,"events":[{"t":0,"jump":true}]}"#,
        )
        .unwrap();
        let mut map = InputMap::default();
        map.inputs.remove(&Action::Scene(CommonInputAction::IaJump));
        let unbound = MovementKeys::from_map(&map).unwrap();
        assert!(unbound.for_scenario(&jumping).is_err());
        assert!(unbound.for_scenario(&Scenario::parse(JOG).unwrap()).is_ok());
        let keys = MovementKeys::from_map(&InputMap::default()).unwrap();
        assert!(keys.for_scenario(&jumping).is_ok());
    }

    #[test]
    fn the_jump_key_counts_in_the_input_only_when_the_scenario_jumps() {
        let still = ConsumedInput::default();
        let jumping = still.with_jump(true);
        assert!(jumping.matches(&InputStep {
            jump: true,
            ..default()
        }));
        assert!(!jumping.matches(&InputStep::default()));
        assert!(!still.matches(&InputStep {
            jump: true,
            ..default()
        }));
        // without jumps, `in` keeps its earlier form
        let jog = ConsumedInput::from_actions(true, false, false, false, false, false);
        assert_eq!(
            jog.to_json(false),
            r#"{"fwd":1.0,"right":0.0,"walk":false,"sprint":false}"#
        );
        assert_eq!(
            jog.with_jump(true).to_json(true),
            r#"{"fwd":1.0,"right":0.0,"walk":false,"sprint":false,"jump":true}"#
        );
        assert_eq!(
            jog.to_json(true),
            r#"{"fwd":1.0,"right":0.0,"walk":false,"sprint":false,"jump":false}"#
        );
    }

    #[test]
    fn yaw_is_degrees_clockwise_from_north() {
        let facing = |yaw: f64| {
            sdk_yaw(
                Transform::IDENTITY
                    .looking_to(yaw_direction(yaw), Vec3::Y)
                    .rotation,
            )
        };
        assert!(facing(0.0).abs() < 1e-4);
        assert!((facing(90.0) - 90.0).abs() < 1e-4);
        assert!((facing(-45.0) + 45.0).abs() < 1e-4);
        assert_eq!(normalize_yaw(-180.0), 180.0);
        assert_eq!(normalize_yaw(270.0), -90.0);
        // the camera's engine yaw the recorder holds for an SDK yaw
        let camera = Quat::from_euler(EulerRot::YXZ, -(30f32.to_radians()), -0.3, 0.0);
        assert!((sdk_yaw(camera) - 30.0).abs() < 1e-3);
        // north is +z in the SDK, -z in the engine
        let base = IVec2::new(84, -6);
        let world = world_translation(Vec3::new(32.0, 0.0, 8.0), base);
        assert_eq!(world, Vec3::new(1376.0, 0.0, 88.0));
        assert_eq!(scene_position(world, base), Vec3::new(32.0, 0.0, 8.0));
    }

    #[test]
    fn a_frame_writes_the_movement_scenes_dispatch_and_reply_times() {
        use scene_runner::renderer_context::{ReplyTiming, SentTiming};
        use std::time::Duration;
        use std::time::Instant;

        // the keys as the frame line carries them, each followed by a comma
        let fields = |keys: String| -> serde_json::Value {
            serde_json::from_str(&format!("{{{}}}", keys.trim_end_matches(','))).unwrap()
        };
        let start = Instant::now();
        let ms = |v: u64| start + Duration::from_millis(v);
        let sent = |frame, at| SentTiming {
            at: ms(at),
            frame,
            loop_start: ms(0),
            deadline: ms(16),
            redispatch: false,
        };
        // dispatched in frame 7, 2 ms into the loop; frame 6's reply came 3 ms after its deadline
        let timing = SceneTickTiming {
            sent: Some(sent(7, 2)),
            reply: Some(ReplyTiming {
                sent: sent(6, 4),
                at: ms(19),
                frame: 7,
            }),
            held_back: None,
        };
        assert_eq!(
            fields(tick_timing_json(Some(&timing), 7)),
            serde_json::json!({"scene_sent": 2.0, "scene_redispatch": false, "scene_reply_late": 3.0, "scene_reply_frames": 1})
        );
        // the same late reply answered by a re-dispatch 3 ms into the loop
        let timing = SceneTickTiming {
            sent: Some(SentTiming {
                redispatch: true,
                ..sent(7, 3)
            }),
            ..timing
        };
        assert_eq!(
            fields(tick_timing_json(Some(&timing), 7)),
            serde_json::json!({"scene_sent": 3.0, "scene_redispatch": true, "scene_reply_late": 3.0, "scene_reply_frames": 1})
        );
        // a frame with neither
        assert_eq!(
            fields(tick_timing_json(Some(&timing), 8)),
            serde_json::json!({"scene_sent": null, "scene_redispatch": null, "scene_reply_late": null, "scene_reply_frames": null})
        );
        // a reply 5 ms before the deadline of the frame it was dispatched in
        let timing = SceneTickTiming {
            sent: Some(sent(7, 2)),
            reply: Some(ReplyTiming {
                sent: sent(7, 2),
                at: ms(11),
                frame: 7,
            }),
            held_back: None,
        };
        assert_eq!(
            fields(tick_timing_json(Some(&timing), 7)),
            serde_json::json!({"scene_sent": 2.0, "scene_redispatch": false, "scene_reply_late": -5.0, "scene_reply_frames": 0})
        );
        assert_eq!(
            fields(tick_timing_json(None, 7)),
            serde_json::json!({"scene_sent": null, "scene_redispatch": null, "scene_reply_late": null, "scene_reply_frames": null})
        );
    }

    #[test]
    fn numbers_are_written_short_and_finite() {
        assert_eq!(num(0.0), "0.0");
        assert_eq!(num(-0.0000001), "0.0");
        assert_eq!(num(1.0 / 60.0), "0.016667");
        assert_eq!(num(-2.5), "-2.5");
        assert_eq!(num(f64::NAN), "null");
    }

    /// Runs `frames` frames 1/60 s apart with a run already at t = 0 on the first, and returns
    /// whether W was down in each.
    fn w_by_frame(scenario: &str, frames: usize) -> Vec<bool> {
        key_by_frame(scenario, frames, KeyCode::KeyW)
    }

    /// Runs `frames` frames 1/60 s apart with a run already at t = 0 on the first, and returns
    /// whether `key` was down in each.
    fn key_by_frame(scenario: &str, frames: usize, key: KeyCode) -> Vec<bool> {
        let mut app = App::new();
        app.add_plugins((bevy::time::TimePlugin, InputPlugin))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                1.0 / 60.0,
            )))
            .init_resource::<MovementTrace>()
            .add_event::<RpcCallEvent>()
            .add_systems(PreUpdate, press_scenario_keys.before(InputSystem));
        app.update();
        let scenario = Scenario::parse(scenario).unwrap();
        let keys = MovementKeys::from_map(&InputMap::default()).unwrap();
        app.world_mut().resource_mut::<MovementTrace>().run = Some(test_run(scenario, keys));
        (0..frames)
            .map(|_| {
                app.update();
                app.world().resource::<ButtonInput<KeyCode>>().pressed(key)
            })
            .collect()
    }

    fn test_run(scenario: Scenario, keys: MovementKeys) -> Run {
        Run {
            fps: scenario.fps,
            scenario,
            repeat: 1,
            input: InputSource::Keys(keys),
            start: StartCondition::Still,
            fixed_dt: true,
            previous_fps: None,
            base: IVec2::ZERO,
            spawn: Vec3::ZERO,
            spawn_yaw: 0.0,
            realm: String::new(),
            scene: String::new(),
            client_version: String::new(),
            animations: None,
            camera_mode: "third_person",
            phase: Phase::Starting,
            spawns: 0,
            settle_started: 0.0,
            started_utc: String::new(),
            consumed: None,
            frames: Vec::new(),
            dropped: 0,
            mismatches: 0,
            scene_ticks: 0,
            scene_log: None,
            locomotion: None,
            start_position: None,
            last_position: Vec3::ZERO,
            last_t: 0.0,
        }
    }

    #[test]
    fn keys_follow_the_scenario_by_frame_time() {
        // a tap of 0.1 s at 60 fps: down at t = 0 .. 5/60, up from t = 6/60 = 0.1
        let tap = r#"{"format":1,"id":"tap","fps":60,"duration":0.5,
            "events":[{"t":0,"fwd":1},{"t":0.1}]}"#;
        let w = w_by_frame(tap, 9);
        assert_eq!(
            w,
            vec![true, true, true, true, true, true, false, false, false]
        );
    }

    #[test]
    fn the_jump_key_follows_the_scenario_by_frame_time() {
        // jump held from t = 0.05 (frame 3) to t = 0.1 (frame 6), while W stays down
        let tap = r#"{"format":1,"id":"jump-tap","fps":60,"duration":0.5,
            "events":[{"t":0,"fwd":1},{"t":0.05,"fwd":1,"jump":true},{"t":0.1,"fwd":1}]}"#;
        assert_eq!(
            key_by_frame(tap, 9, KeyCode::Space),
            vec![false, false, false, true, true, true, false, false, false]
        );
        assert_eq!(w_by_frame(tap, 9), vec![true; 9]);
        // a scenario without jumps never presses the jump key
        assert_eq!(
            key_by_frame(
                r#"{"format":1,"id":"w","fps":60,"duration":0.5,"events":[{"t":0,"fwd":1}]}"#,
                4,
                KeyCode::Space
            ),
            vec![false; 4]
        );
    }

    #[test]
    fn keys_the_run_holds_are_released_when_it_ends() {
        let hold = r#"{"format":1,"id":"hold","fps":60,"duration":5,"events":[{"t":0,"fwd":1}]}"#;
        let mut app = App::new();
        app.add_plugins((bevy::time::TimePlugin, InputPlugin))
            .init_resource::<MovementTrace>()
            .add_event::<RpcCallEvent>()
            .add_systems(PreUpdate, press_scenario_keys.before(InputSystem));
        let scenario = Scenario::parse(hold).unwrap();
        let keys = MovementKeys::from_map(&InputMap::default()).unwrap();
        app.world_mut().resource_mut::<MovementTrace>().run = Some(test_run(scenario, keys));
        app.update();
        app.update();
        assert!(app
            .world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW));
        app.world_mut()
            .resource_mut::<MovementTrace>()
            .end(Err("stopped".into()), None, None);
        app.update();
        assert!(!app
            .world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyW));
        assert!(app.world().resource::<MovementTrace>().held.is_empty());
    }

    #[test]
    fn a_trace_is_a_header_frames_and_a_footer() {
        let scenario = Scenario::parse(JOG).unwrap();
        let mut run = test_run(
            scenario,
            MovementKeys::from_map(&InputMap::default()).unwrap(),
        );
        run.frames.push("{\"type\":\"frame\",\"i\":0}".into());
        run.dropped = 1;
        run.mismatches = 2;
        let text = run.finish();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "header");
        assert_eq!(lines[0]["client"], "bevy");
        assert_eq!(lines[0]["scenario"], "jog-W-hold2.0-release");
        assert_eq!(lines[0]["fps_target"], 60);
        assert_eq!(lines[0]["extra"]["dt_mode"], "fixed");
        assert_eq!(lines[0]["extra"]["input_seam"], "keyboard");
        assert_eq!(lines[2]["type"], "footer");
        assert_eq!(lines[2]["frames"], 1);
        assert_eq!(lines[2]["dropped"], 1);
        assert_eq!(lines[2]["extra"]["input_mismatch_frames"], 2);
        assert_eq!(run.file_name(), "jog-W-hold2.0-release__fps60__r1.jsonl");
        // a run that waits for the player to stand still keeps the earlier header
        assert!(lines[0]["extra"].get("start").is_none());
        run.start = StartCondition::Placed;
        let header: serde_json::Value =
            serde_json::from_str(run.finish().lines().next().unwrap()).unwrap();
        assert_eq!(header["extra"]["start"], "placed");
    }

    #[test]
    fn the_movement_model_state_is_read_from_the_scenes_console_line() {
        let state = r#"{"t":42,"gait":"jog","grounded":false,"jump_count":1,"glide":"closed","stunned":false,"sliding":false}"#;
        // the web runtime writes the string as it is, the native one JSON-quoted
        assert_eq!(
            locomotion_line(&format!("LOG LOCO {state}")).as_deref(),
            Some(state)
        );
        let quoted = serde_json::Value::from(format!("LOCO {state}")).to_string();
        assert_eq!(
            locomotion_line(&format!("LOG {quoted}")).as_deref(),
            Some(state)
        );
        for other in [
            "LOG \"hello\"",
            "LOG LOCO",
            "LOG LOCO 3",
            "LOG LOCO {broken",
            &format!("ERROR LOCO {state}"),
        ] {
            assert_eq!(locomotion_line(other), None, "{other}");
        }
    }

    #[test]
    fn a_placed_start_waits_for_the_settle_and_the_ground_but_not_for_stillness() {
        // never the placement's own frame, even with no settle
        assert!(!placed_start(0.0, 0.0, true));
        assert!(placed_start(1.0 / 60.0, 0.0, true));
        // the settle counts from the placement
        assert!(!placed_start(0.5, 1.0, true));
        assert!(placed_start(1.0, 1.0, true));
        // in the air it waits for the ground, then starts anyway
        assert!(!placed_start(1.0, 0.0, false));
        assert!(!placed_start(GROUNDED_WAIT - 0.01, 0.0, false));
        assert!(placed_start(GROUNDED_WAIT, 0.0, false));
        assert!(placed_start(1.0 + GROUNDED_WAIT, 1.0, false));
    }
}
