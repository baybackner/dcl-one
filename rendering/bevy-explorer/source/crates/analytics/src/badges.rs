//! The movement and emote events behind the node's client-event badges, named and shaped as its
//! webhook reads them: `used_emote` at most once a minute, `walked_distance` with the steps walked
//! since the last report, and `vertical_height_reached` once per login after a total elevation
//! gain of 500 m on foot. Only movement under the player's own control counts.
//! `move_to_parcel` is sent by the scene runner, which knows the scene at each parcel.
//! Each goes out only with a usable auth chain ([`SegmentConfig::client_event_common`]).

use bevy::prelude::*;
use common::structs::{
    AvatarDynamicState, MoveKind, OutOfWorld, PlayerEmoteTriggered, PlayerTeleported, PrimaryUser,
};

use wallet::Wallet;

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
/// The total elevation gain in one session that completes vertical voyager, in metres: the
/// badge is "a total elevation gain of at least 500m in one session", so every climb adds up.
pub const HEIGHT: f32 = 500.0;
/// A rise is counted once it passes this (jitter stays out) and only below the cap (a teleport
/// or a beam is not climbing), in metres.
pub const MIN_RISE: f32 = 0.01;
pub const MAX_RISE: f32 = 10.0;

/// The node counts emotes per distinct minute.
pub const EMOTE_INTERVAL_SECS: f64 = 60.0;

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
    /// Walking on the ground under its own control: a rise counts.
    Walking,
    /// Not moving under its own control (standing, carried by a lift, platform or scene, pushed):
    /// a rise is not climbing.
    Standing,
    /// Jumping under its own control: the rise is counted on landing.
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
    if dynamics.own_velocity == Vec3::ZERO {
        return Footing::Standing;
    }
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
    } else if dynamics.own_velocity.xz().length() > 0.1 {
        Footing::Walking
    } else {
        Footing::Standing
    }
}

/// One `used_emote` per player per [`EMOTE_INTERVAL_SECS`].
#[derive(Default)]
pub struct EmoteThrottle {
    address: Option<String>,
    last: Option<f64>,
}

impl EmoteThrottle {
    /// Whether an emote played at `now` (seconds) is reported.
    pub fn allow(&mut self, address: &str, now: f64) -> bool {
        if self.address.as_deref() != Some(address) {
            *self = Self {
                address: Some(address.to_owned()),
                last: None,
            };
        }
        if self
            .last
            .is_some_and(|last| now - last < EMOTE_INTERVAL_SECS)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// The last parcel a `move_to_parcel` was considered for, per login and realm.
#[derive(Default)]
pub struct ParcelTracker(Option<(String, String, IVec2)>);

impl ParcelTracker {
    /// `Some(old)` when the player is somewhere new: a new parcel, or the same parcel under
    /// another login or realm, whose previous parcel is `None`.
    pub fn moved(&mut self, address: &str, realm: &str, parcel: IVec2) -> Option<Option<IVec2>> {
        let old = match &self.0 {
            Some((a, r, p)) if a == address && r == realm => {
                if *p == parcel {
                    return None;
                }
                Some(*p)
            }
            _ => None,
        };
        self.0 = Some((address.to_owned(), realm.to_owned(), parcel));
        Some(old)
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn report_badge_events(
    player: Query<(&GlobalTransform, &AvatarDynamicState, Has<OutOfWorld>), With<PrimaryUser>>,
    time: Res<Time>,
    config: Option<Res<SegmentConfig>>,
    metrics: Option<ResMut<SegmentMetricsEvents>>,
    wallet: Option<Res<Wallet>>,
    mut emotes: EventReader<PlayerEmoteTriggered>,
    mut teleports: EventReader<PlayerTeleported>,
    mut locals: Local<(StepCounter, ElevationGain, EmoteThrottle)>,
) {
    let (Some(config), Some(mut metrics)) = (config, metrics) else {
        emotes.clear();
        teleports.clear();
        return;
    };
    let (steps, elevation, throttle) = &mut *locals;
    // read per event: a refreshed delegation is used, an expired one is not
    let client_common = || wallet.as_ref().and_then(|w| config.client_event_common(w));
    let address = config.common().dcl_eth_address.clone();
    for emote in emotes.read() {
        if !throttle.allow(&address, time.elapsed_secs_f64()) {
            continue;
        }
        if let Some(common) = client_common() {
            metrics.add_event(
                SegmentEvent::UsedEmote(SegmentEventUsedEmote::new(&emote.urn, emote.source)),
                &common,
            );
        }
    }
    let teleported = teleports.read().count() > 0;
    let Ok((transform, dynamics, out_of_world)) = player.single() else {
        return;
    };
    let footing = if teleported || out_of_world {
        Footing::Moved
    } else {
        footing(dynamics)
    };

    let own_speed = match footing {
        Footing::Walking => dynamics.own_velocity.xz().length(),
        _ => 0.0,
    };
    if let Some(step_count) = steps.tick(&address, own_speed, time.delta_secs()) {
        if let Some(common) = client_common() {
            metrics.add_event(
                SegmentEvent::WalkedDistance(SegmentEventWalkedDistance { step_count }),
                &common,
            );
        }
    }
    if let Some(height) = elevation.tick(&address, transform.translation().y, footing) {
        if let Some(common) = client_common() {
            metrics.add_event(
                SegmentEvent::VerticalHeightReached(SegmentEventVerticalHeightReached { height }),
                &common,
            );
        }
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

    // Only rises walked count: a lift, platform or scene ride (no own movement), a jump's apex and
    // a teleport do not, a ledge climbed by jumping counts on landing, and the climbs add up to
    // one event per login.
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

    fn dynamics(own_velocity: Vec3, velocity: Vec3) -> AvatarDynamicState {
        AvatarDynamicState {
            own_velocity,
            velocity,
            ..default()
        }
    }

    // A conveyor, a vehicle or a scene's movement override moves the player with no movement of
    // its own: neither steps nor climb.
    #[test]
    fn only_the_players_own_movement_is_walking() {
        let carried = dynamics(Vec3::ZERO, Vec3::new(5.0, 2.0, 0.0));
        assert_eq!(footing(&carried), Footing::Standing);
        let walking = dynamics(Vec3::new(1.5, 0.0, 0.0), Vec3::new(1.5, 0.0, 0.0));
        assert_eq!(footing(&walking), Footing::Walking);
        let jumping = AvatarDynamicState {
            move_kind: MoveKind::Jump,
            ..dynamics(Vec3::new(1.5, 8.0, 0.0), Vec3::ZERO)
        };
        assert_eq!(footing(&jumping), Footing::Airborne);
    }

    // The node counts one emote a minute, so the rest are not sent; a new login starts over.
    #[test]
    fn used_emote_goes_once_a_minute_per_player() {
        let mut throttle = EmoteThrottle::default();
        assert!(throttle.allow("0xa", 10.0));
        assert!(!throttle.allow("0xa", 30.0));
        assert!(!throttle.allow("0xa", 69.9));
        assert!(throttle.allow("0xa", 70.0));
        assert!(throttle.allow("0xb", 71.0));
    }

    // A login or realm change sends the current parcel again from nowhere, so Genesis (0,0) to a
    // world's (0,0) is a move; standing still is not.
    #[test]
    fn a_move_is_per_login_realm_and_parcel() {
        let mut tracker = ParcelTracker::default();
        let genesis = "https://peer.decentraland.org/";
        let world = "https://worlds.example/world/a.dcl.eth/";
        assert_eq!(
            tracker.moved(UNAUTHENTICATED, genesis, IVec2::ZERO),
            Some(None)
        );
        assert_eq!(tracker.moved(UNAUTHENTICATED, genesis, IVec2::ZERO), None);
        assert_eq!(tracker.moved("0xa", genesis, IVec2::ZERO), Some(None));
        assert_eq!(
            tracker.moved("0xa", genesis, IVec2::X),
            Some(Some(IVec2::ZERO))
        );
        assert_eq!(tracker.moved("0xa", world, IVec2::X), Some(None));
        assert_eq!(tracker.moved("0xa", world, IVec2::X), None);
    }
}
