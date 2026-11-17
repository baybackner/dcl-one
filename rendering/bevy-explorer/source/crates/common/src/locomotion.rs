//! The movement model's state, `PBAvatarMovement.locomotion`, and what presentation derives from
//! it. The model publishes facts (gait, ground, jumps, glide, stun, slide); the avatar's animation
//! and the animation state other clients receive are derived here and never feed back.

use bevy::prelude::*;
use dcl_component::proto_components::sdk::components::{
    GlideState, LocomotionGait, LocomotionState,
};

use crate::structs::{MoveKind, PrimaryUser};

/// Vertical speed (m/s, up positive) below which an avatar off the ground is falling.
pub const FALLING_SPEED: f32 = -3.0;
/// Vertical speed (m/s, up positive) below which an avatar off the ground is falling far.
pub const LONG_FALLING_SPEED: f32 = -8.0;
/// A movement blend below this is idle.
const MIN_MOVEMENT_BLEND: f32 = 0.01;

/// The model state the scene controlling movement published with this frame's `AvatarMovement`,
/// on the primary player. `None` when that scene leaves `locomotion` unset, or no scene controls
/// movement: the engine then infers the avatar's state from its velocity.
#[derive(Component, Default, Clone, Debug, PartialEq)]
pub struct SceneLocomotion(pub Option<LocomotionState>);

/// Horizontal speeds (m/s) at which each gait reaches its full movement blend.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GaitSpeeds {
    pub walk: f32,
    pub jog: f32,
    pub run: f32,
}

impl Default for GaitSpeeds {
    fn default() -> Self {
        Self::of(&PrimaryUser::default())
    }
}

impl GaitSpeeds {
    pub fn of(user: &PrimaryUser) -> Self {
        Self {
            walk: user.walk_speed,
            jog: user.jog_speed,
            run: user.run_speed,
        }
    }

    fn full(&self, gait: LocomotionGait) -> f32 {
        match gait {
            LocomotionGait::LgIdle => 0.0,
            LocomotionGait::LgWalk => self.walk,
            LocomotionGait::LgJog => self.jog,
            LocomotionGait::LgRun => self.run,
        }
    }
}

/// The gait's place on the movement blend: idle 0, walk 1, jog 2, run 3.
fn gait_index(gait: LocomotionGait) -> f32 {
    gait as i32 as f32
}

/// The movement blend, 0 (idle) to 3 (run), that the gait and horizontal speed aim for: the speed's
/// share of the gait's full speed, times the gait's place on the blend; below 0.01 it is 0. This is
/// the target before any smoothing, which belongs to the presenter.
pub fn movement_blend(gait: LocomotionGait, horizontal_speed: f32, speeds: GaitSpeeds) -> f32 {
    let full = speeds.full(gait);
    if full <= 0.0 {
        return 0.0;
    }
    let blend = (horizontal_speed / full).clamp(0.0, 1.0) * gait_index(gait);
    if blend < MIN_MOVEMENT_BLEND {
        0.0
    } else {
        blend
    }
}

/// The glider is out: opening or gliding.
pub fn is_gliding(state: &LocomotionState) -> bool {
    matches!(
        state.glide_state(),
        GlideState::GsOpening | GlideState::GsGliding
    )
}

/// Off the ground and falling (below [`FALLING_SPEED`]).
pub fn is_falling(state: &LocomotionState, vertical_speed: f32) -> bool {
    !state.grounded && vertical_speed < FALLING_SPEED
}

/// Off the ground and falling far (below [`LONG_FALLING_SPEED`]).
pub fn is_long_falling(state: &LocomotionState, vertical_speed: f32) -> bool {
    !state.grounded && vertical_speed < LONG_FALLING_SPEED
}

/// The engine's move kind for a model state, with the avatar's velocity: the glide, then in the air
/// a long fall, a second jump, a fall or a first jump (an avatar that left the ground without
/// jumping and is not yet falling keeps its gait), and on the ground the gait unless the movement
/// blend is idle. A stunned avatar stands.
pub fn move_kind(state: &LocomotionState, velocity: Vec3, speeds: GaitSpeeds) -> MoveKind {
    if is_gliding(state) {
        return MoveKind::Glide;
    }
    if !state.grounded {
        if is_long_falling(state, velocity.y) {
            return MoveKind::LongFalling;
        }
        if state.jump_count >= 2 {
            return MoveKind::DoubleJump;
        }
        if is_falling(state, velocity.y) {
            return MoveKind::Falling;
        }
        if state.jump_count == 1 {
            return MoveKind::Jump;
        }
    }
    if state.stunned {
        return MoveKind::Idle;
    }
    let gait = state.gait();
    if movement_blend(gait, velocity.xz().length(), speeds) == 0.0 {
        return MoveKind::Idle;
    }
    match gait {
        LocomotionGait::LgIdle => MoveKind::Idle,
        LocomotionGait::LgWalk => MoveKind::Walk,
        LocomotionGait::LgJog => MoveKind::Jog,
        LocomotionGait::LgRun => MoveKind::Run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(gait: LocomotionGait, grounded: bool, jump_count: u32) -> LocomotionState {
        LocomotionState {
            gait: gait as i32,
            grounded,
            jump_count,
            ..Default::default()
        }
    }

    fn kind(state: &LocomotionState, velocity: Vec3) -> MoveKind {
        move_kind(state, velocity, GaitSpeeds::default())
    }

    #[test]
    fn the_blend_is_the_speeds_share_of_the_gait_times_its_place() {
        let speeds = GaitSpeeds::default();
        assert_eq!(movement_blend(LocomotionGait::LgWalk, 1.5, speeds), 1.0);
        assert_eq!(movement_blend(LocomotionGait::LgWalk, 0.75, speeds), 0.5);
        assert_eq!(movement_blend(LocomotionGait::LgJog, 4.0, speeds), 1.0);
        assert_eq!(movement_blend(LocomotionGait::LgJog, 8.0, speeds), 2.0);
        assert_eq!(movement_blend(LocomotionGait::LgRun, 12.0, speeds), 3.0);
        assert_eq!(movement_blend(LocomotionGait::LgIdle, 5.0, speeds), 0.0);
        assert_eq!(movement_blend(LocomotionGait::LgWalk, 0.01, speeds), 0.0);
    }

    #[test]
    fn on_the_ground_the_gait_picks_the_kind_unless_the_avatar_stands() {
        let moving = Vec3::new(0.0, 0.0, -5.0);
        assert_eq!(
            kind(&state(LocomotionGait::LgWalk, true, 0), moving),
            MoveKind::Walk
        );
        assert_eq!(
            kind(&state(LocomotionGait::LgJog, true, 0), moving),
            MoveKind::Jog
        );
        assert_eq!(
            kind(&state(LocomotionGait::LgRun, true, 0), moving),
            MoveKind::Run
        );
        // pushing against a wall: the gait is asked for, the avatar stands
        assert_eq!(
            kind(&state(LocomotionGait::LgRun, true, 0), Vec3::ZERO),
            MoveKind::Idle
        );
        // carried by a force with no gait
        assert_eq!(
            kind(&state(LocomotionGait::LgIdle, true, 0), moving),
            MoveKind::Idle
        );
    }

    #[test]
    fn a_stunned_avatar_stands() {
        let mut stunned = state(LocomotionGait::LgRun, true, 0);
        stunned.stunned = true;
        assert_eq!(kind(&stunned, Vec3::new(5.0, 0.0, 0.0)), MoveKind::Idle);
    }

    #[test]
    fn in_the_air_jumps_and_falls_follow_the_count_and_the_vertical_speed() {
        let up = Vec3::new(0.0, 4.0, 0.0);
        let falling = Vec3::new(0.0, -4.0, 0.0);
        let falling_far = Vec3::new(0.0, -9.0, 0.0);
        assert_eq!(
            kind(&state(LocomotionGait::LgJog, false, 1), up),
            MoveKind::Jump
        );
        assert_eq!(
            kind(&state(LocomotionGait::LgJog, false, 2), up),
            MoveKind::DoubleJump
        );
        assert_eq!(
            kind(&state(LocomotionGait::LgJog, false, 1), falling),
            MoveKind::Falling
        );
        assert_eq!(
            kind(&state(LocomotionGait::LgJog, false, 2), falling_far),
            MoveKind::LongFalling
        );
        // walked off a ledge, not yet falling: keeps its gait
        assert_eq!(
            kind(
                &state(LocomotionGait::LgWalk, false, 0),
                Vec3::new(0.0, -1.0, 1.5)
            ),
            MoveKind::Walk
        );
        assert_eq!(
            kind(&state(LocomotionGait::LgWalk, false, 0), falling),
            MoveKind::Falling
        );
    }

    #[test]
    fn the_glide_wins_while_the_glider_is_out() {
        for (glide, expected) in [
            (GlideState::GsOpening, MoveKind::Glide),
            (GlideState::GsGliding, MoveKind::Glide),
            (GlideState::GsClosing, MoveKind::DoubleJump),
            (GlideState::GsClosed, MoveKind::DoubleJump),
        ] {
            let mut gliding = state(LocomotionGait::LgRun, false, 2);
            gliding.glide_state = glide as i32;
            assert_eq!(
                kind(&gliding, Vec3::new(0.0, -2.0, 6.0)),
                expected,
                "{glide:?}"
            );
        }
    }
}
