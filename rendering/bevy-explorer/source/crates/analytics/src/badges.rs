//! The movement and emote events behind the node's client-event badges, named and shaped as its
//! webhook reads them: `used_emote` per emote the player triggers, `walked_distance` with the
//! steps walked since the last report, and `vertical_height_reached` once per login after 500 m
//! of elevation gained on foot.
//! `move_to_parcel` is sent by the scene runner, which knows the scene at each parcel.

use bevy::prelude::*;
use common::structs::{
    AvatarDynamicState, MoveKind, OutOfWorld, PlayerEmoteTriggered, PlayerTeleported, PrimaryUser,
};

use crate::{
    data_definition::{
        SegmentEvent, SegmentEventUsedEmote, SegmentEventVerticalHeightReached,
        SegmentEventWalkedDistance,
    },
    segment_system::{SegmentConfig, SegmentMetricsEvents},
};

/// The avatar has no footstep events, so a step is this much grounded walking at the player's own
/// speed.
pub const STEP_METRES: f32 = 0.75;
/// Steps are reported this often, or as soon as more than [`SEND_AT_STEPS`] are counted; one
/// report stays far below the node's 10,000-step cap.
pub const STEP_SEND_SECS: f32 = 30.0;
pub const SEND_AT_STEPS: u64 = 100;
/// The elevation gain that completes vertical voyager, in metres.
pub const HEIGHT: f32 = 500.0;
/// A rise is counted once it passes this (jitter stays out) and only below the cap (a teleport
/// or a beam is not climbing), in metres.
pub const MIN_RISE: f32 = 0.01;
pub const MAX_RISE: f32 = 10.0;

const UNAUTHENTICATED: &str = "unauthenticated";

/// Steps per login, reported in chunks.
#[derive(Default)]
pub struct StepCounter {
    address: Option<String>,
    metres: f32,
    steps: u64,
    countdown: f32,
}

impl StepCounter {
    /// A frame of `dt` seconds at `grounded_speed` m/s of the player's own walking; the steps to
    /// report when a chunk closes.
    pub fn tick(&mut self, address: &str, grounded_speed: f32, dt: f32) -> Option<u64> {
        if self.address.as_deref() != Some(address) {
            *self = Self {
                address: Some(address.to_owned()),
                countdown: STEP_SEND_SECS,
                ..default()
            };
        }
        if address == UNAUTHENTICATED {
            return None;
        }
        self.metres += grounded_speed * dt;
        let whole = (self.metres / STEP_METRES).floor();
        self.steps += whole as u64;
        self.metres -= whole * STEP_METRES;
        self.countdown -= dt;
        if self.countdown > 0.0 && self.steps <= SEND_AT_STEPS {
            return None;
        }
        self.countdown = STEP_SEND_SECS;
        Some(std::mem::take(&mut self.steps)).filter(|steps| *steps > 0)
    }
}

/// How the player stands this frame, for the elevation gain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Footing {
    /// Walking on the ground under its own power: a rise counts.
    Walking,
    /// On the ground without moving: a rise is a lift or a platform, not climbing.
    Standing,
    /// Jumping or falling: the rise is counted on landing.
    Airborne,
    /// Teleported or behind the loading screen.
    Moved,
}

/// Elevation gained on foot per login, once.
#[derive(Default)]
pub struct ElevationGain {
    address: Option<String>,
    reference: Option<f32>,
    gain: f32,
    reached: bool,
}

impl ElevationGain {
    /// The player at height `y`; the gain once it first passes [`HEIGHT`].
    pub fn tick(&mut self, address: &str, y: f32, footing: Footing) -> Option<f32> {
        if self.address.as_deref() != Some(address) {
            *self = Self {
                address: Some(address.to_owned()),
                ..default()
            };
        }
        if self.reached || address == UNAUTHENTICATED {
            return None;
        }
        let reference = match (footing, self.reference) {
            (Footing::Airborne, _) => return None,
            (Footing::Moved, _) => {
                self.reference = None;
                return None;
            }
            (Footing::Standing, _) | (Footing::Walking, None) => {
                self.reference = Some(y);
                return None;
            }
            (Footing::Walking, Some(reference)) => reference,
        };
        let rise = y - reference;
        // a rise below the threshold builds up against the same reference
        if rise.abs() <= MIN_RISE {
            return None;
        }
        self.reference = Some(y);
        if rise < MAX_RISE {
            self.gain += rise.max(0.0);
        }
        self.reached = self.gain > HEIGHT;
        self.reached.then_some(self.gain)
    }
}

/// The player's footing from its movement state.
fn footing(dynamics: &AvatarDynamicState) -> Footing {
    let airborne = dynamics.ground_height > 0.2
        || matches!(
            dynamics.move_kind,
            MoveKind::Jump
                | MoveKind::DoubleJump
                | MoveKind::Falling
                | MoveKind::LongFalling
                | MoveKind::Glide
        );
    if airborne {
        Footing::Airborne
    } else if dynamics.velocity.xz().length() > 0.1 {
        Footing::Walking
    } else {
        Footing::Standing
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn report_badge_events(
    player: Query<(&GlobalTransform, &AvatarDynamicState, Has<OutOfWorld>), With<PrimaryUser>>,
    time: Res<Time>,
    config: Option<Res<SegmentConfig>>,
    metrics: Option<ResMut<SegmentMetricsEvents>>,
    mut emotes: EventReader<PlayerEmoteTriggered>,
    mut teleports: EventReader<PlayerTeleported>,
    mut steps: Local<StepCounter>,
    mut elevation: Local<ElevationGain>,
) {
    let (Some(config), Some(mut metrics)) = (config, metrics) else {
        emotes.clear();
        teleports.clear();
        return;
    };
    let common = config.common();
    for emote in emotes.read() {
        metrics.add_event(
            SegmentEvent::UsedEmote(SegmentEventUsedEmote::new(&emote.urn, emote.source)),
            common,
        );
    }
    let teleported = teleports.read().count() > 0;
    let Ok((transform, dynamics, out_of_world)) = player.single() else {
        return;
    };
    let address = &common.dcl_eth_address;
    let footing = if teleported || out_of_world {
        Footing::Moved
    } else {
        footing(dynamics)
    };

    let grounded_speed = match footing {
        Footing::Walking => dynamics.velocity.xz().length(),
        _ => 0.0,
    };
    if let Some(step_count) = steps.tick(address, grounded_speed, time.delta_secs()) {
        metrics.add_event(
            SegmentEvent::WalkedDistance(SegmentEventWalkedDistance { step_count }),
            common,
        );
    }
    if let Some(height) = elevation.tick(address, transform.translation().y, footing) {
        metrics.add_event(
            SegmentEvent::VerticalHeightReached(SegmentEventVerticalHeightReached { height }),
            common,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Walking at 1.5 m/s is two steps a second, reported every 30 s; a burst past 100 steps goes
    // out at once, and a quiet chunk sends nothing.
    #[test]
    fn steps_are_reported_per_chunk_or_past_the_threshold() {
        let mut counter = StepCounter::default();
        assert_eq!(counter.tick(UNAUTHENTICATED, 1.5, 40.0), None);
        for _ in 0..29 {
            assert_eq!(counter.tick("0xa", 1.5, 1.0), None);
        }
        assert_eq!(counter.tick("0xa", 1.5, 1.0), Some(60));
        assert_eq!(counter.tick("0xa", 10.0, 7.6), Some(101));
        assert_eq!(counter.tick("0xa", 0.0, 30.0), None);
        // a new login starts over
        counter.tick("0xa", 1.5, 1.0);
        assert_eq!(counter.tick("0xb", 0.0, 30.0), None);
    }

    // Only rises walked count: a lift ridden standing, a jump's apex and a teleport do not, a
    // ledge climbed by jumping counts on landing, and the event goes once per login.
    #[test]
    fn elevation_counts_rises_walked_once_per_login() {
        let mut gain = ElevationGain::default();
        assert_eq!(gain.tick("0xa", 0.0, Footing::Walking), None);
        // a lift
        assert_eq!(gain.tick("0xa", 300.0, Footing::Standing), None);
        // a jump onto a 2 m ledge, landing while walking on
        assert_eq!(gain.tick("0xa", 303.0, Footing::Airborne), None);
        assert_eq!(gain.tick("0xa", 302.0, Footing::Walking), None);
        assert_eq!(gain.gain, 2.0);
        // a teleport up
        assert_eq!(gain.tick("0xa", 900.0, Footing::Moved), None);
        assert_eq!(gain.tick("0xa", 900.0, Footing::Walking), None);
        // small rises build up against the same reference
        for _ in 0..4 {
            assert_eq!(gain.tick("0xa", 900.004, Footing::Walking), None);
        }
        assert_eq!(gain.gain, 2.0);
        let mut y = 900.0;
        let mut reached = None;
        while reached.is_none() {
            y += 0.5;
            reached = gain.tick("0xa", y, Footing::Walking);
        }
        assert!(reached.unwrap() > HEIGHT);
        assert_eq!(gain.tick("0xa", y + 600.0, Footing::Walking), None);
        // the next login climbs again
        assert_eq!(gain.tick("0xb", 0.0, Footing::Walking), None);
        assert!(!gain.reached);
    }
}
