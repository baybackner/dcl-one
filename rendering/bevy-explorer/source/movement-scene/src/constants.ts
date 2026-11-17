import { Vector3 } from "@dcl/sdk/math";

export const GRAVITY = Vector3.scale(Vector3.Down(), 10.0);

export const WALK_SPEED = 1.5; // max walk speed
export const JOG_SPEED = 8; // max run speed
export const SPRINT_SPEED = 10; // max sprint speed

// Horizontal input model. It is integrated in fixed steps inside each scene tick, so the result
// does not depend on the frame rate or on how often the scene gets to answer.
export const FIXED_STEP = 0.02; // s, length of one integration step (50 Hz)
export const MAX_FIXED_STEPS = 13; // integration steps per tick at most (bounds a hitch)
// Per input axis: a held key moves the axis speed toward its target at this acceleration (m/s^2).
// The acceleration blends from the low to the high value as the input-hold weight goes from
// ACCEL_CURVE_START to ACCEL_CURVE_END (smoothstep); the weight ramps over the accel time.
export const GROUND_ACCEL = 20;
export const GROUND_MAX_ACCEL = 25;
export const AIR_ACCEL = 15;
export const AIR_MAX_ACCEL = 20;
export const ACCEL_CURVE_START = 0.1;
export const ACCEL_CURVE_END = 0.9;
export const HORIZONTAL_ACCEL_TIME_GROUND = 0.5; // s for the input-hold weight to go 0 -> 1
// Smoothing time (s) of an axis whose key is released. 0 stops the axis within one step.
export const HORIZONTAL_DAMP_TIME_GROUND = 0;
// Quadratic horizontal drag while airborne (1/m): dv/dt = -AIR_DRAG * |v| * v.
export const AIR_DRAG = 0.2;
// Linear damping of external (impulse/force) horizontal velocity (1/s): always, plus on ground.
export const EXTERNAL_ENV_DRAG = 1.5;
export const EXTERNAL_GROUND_FRICTION = 4;
export const MAX_EXTERNAL_SPEED = 50;
// Steps up to the current frame use the input of the previous tick (1) or of this tick (0).
export const INPUT_DELAY_TICKS = 1;
// Reply prediction: the engine applies a reply from the frame it lands on until the next one.
// 0 = reply with the state at the dispatch frame; 1 = estimate the reply delay in frames from
// previousStepTime / stepTime and reply with the state predicted for the last frame it is held;
// 2 = as 1, with the ratio smoothed over ticks.
export const PREDICT_MODE: number = 1;
export const FRAME_TIME_SMOOTHING = 0.1; // weight of the newest sample in the frame-time average
export const DELAY_ROUNDING = 0; // subtracted from the frame ratio before rounding
export const MAX_TICK_TIME = 0.25; // s, longest tick the model integrates
// Ramp guard (bit flags), against the prediction switching between ticks:
// 1 = while the same input is held on the ground and the model accelerates, a reply never
//     slows below the previous one;
// 2 = between two airborne replies (no launch, glide, landing or push), vy never rises.
// Bit 2 applies after the height tracking below: a correction can slow the fall but never make vy rise in the air.
export const RAMP_GUARD: number = 3;
// Vertical reply in the air: 1 = the mean predicted velocity over the frames the reply is held, from the per-tick
// delay estimate; 2 = the velocity predicted 1.5 x the smoothed delay ahead (replyDelaySmooth), which only falls
// between replies. A launch from the ground always uses the per-tick estimate.
export const VERTICAL_PREDICT: number = 2;
// Height tracking during a jump: the engine integrates whatever velocity it is sent, so a reply that lands earlier
// or later than estimated leaves a height error that adds up over the jump. Each reply adds the error between the
// model's height and the avatar's, divided by VERTICAL_TRACK_TIME (s), at most VERTICAL_TRACK_MAX (m/s). 0 = off.
export const VERTICAL_TRACK_TIME: number = 0.15;
export const VERTICAL_TRACK_MAX: number = 2;
// Height tracking stops for the rest of the jump when the applied vertical velocity differs from the requested one by
// more than this (m/s): the avatar hit something.
export const VERTICAL_TRACK_BLOCK: number = 0.5;
// Upward external velocity (impulses) fades with this time constant plus a constant
// deceleration; downward external velocity is kept until the ground stops it.
export const EXTERNAL_UP_DAMP_TIME = 0.75;
export const EXTERNAL_UP_DECEL = 5;

// Hard cap on total velocity magnitude — applied after external velocity is
// added each frame, so runaway accumulation from repeated external impulses
// can't produce absurd speeds.
export const MAX_SPEED = 30;

// Jump. Heights are those of a tapped jump; holding the key lowers gravity (below) and
// reaches about twice the height. A running jump blends toward JUMP_HEIGHT_SPRINT with speed.
export const JUMP_HEIGHT = 1;
export const JUMP_HEIGHT_SPRINT = 1.5;
export const GRAVITY_RISE_FACTOR = 4; // gravity multiplier while moving up (also sizes the launch speed)
export const LONG_JUMP_TIME = 0.52; // s after a jump's press during which a held key scales gravity (26 steps)
export const LONG_JUMP_GRAVITY_SCALE = 0.5;
// Steps a press is buffered before landing, and steps after leaving the ground that a jump
// is still allowed (one window for both; off while moving up).
export const JUMP_GRACE_STEPS = 8;
export const JUMP_COYOTE_TIME = 0.16; // s, the same window for the step-up check
// Air jump: one per airtime, by a fresh press in the air after the ground jump (or once the grace window of a fall
// off an edge has passed). The avatar holds its height for AIR_JUMP_HOVER_TIME, then launches to DOUBLE_JUMP_HEIGHT
// (any gait). Scenes turn it off with InputModifier disableDoubleJump.
export const DOUBLE_JUMP_HEIGHT = 2; // in-air jump height (m), overridden by the locomotion settings
export const AIR_JUMP_COUNT = 1; // air jumps per airtime
export const AIR_JUMP_COOLDOWN = 0.32; // s from the previous jump's press before an air jump can start (16 steps)
export const AIR_JUMP_HOVER_TIME = 0.2; // s the height is held between the press and the launch (whole steps)
export const AIR_JUMP_HOVER_SPEED = 0; // vertical speed during the hover (m/s)
// Horizontal speed at the launch (m/s): max(AIR_JUMP_IMPULSE, current speed) along the keys held on the launch step.
// 0 keeps the current speed (turned toward the keys).
export const AIR_JUMP_IMPULSE = 8;
// Behaviour switches:
// 1: a press whose step reaches the ground is lost (the landing cancels the air jump); 0: it is buffered to the ground.
export const AIR_JUMP_LOST_ON_TOUCHDOWN: number = 1;
// 1: no movement key on the launch step stops horizontal motion; 0: the horizontal velocity is kept.
export const AIR_JUMP_STOP_WITHOUT_KEYS: number = 1;
// 1: the held-key window (LONG_JUMP_TIME) counts from the air press, so the hover uses part of it; 0: from the launch.
export const AIR_JUMP_HOLD_FROM_PRESS: number = 1;
// 1: any jump key held in that window lowers gravity (a later, ignored press too); 0: only the air press held throughout.
export const AIR_JUMP_HOLD_ANY_PRESS: number = 1;
// Steps a press made while rising stays wanted for an air jump (0: a press before the cooldown while rising is dropped).
export const AIR_JUMP_RISE_BUFFER_STEPS: number = 0;
// Height tracking during the hover: 1 = both ways (the avatar drifts to the model's hover height), 0 = none (a flat hover;
// tracking resumes at the launch).
export const AIR_JUMP_HOVER_TRACK: number = 0;
// Glide: a fresh press in the air once the jumps are spent (the air jump included) opens the glider
// while held, once long enough after the latest jump's press and above the ground.
export const GLIDE_FALL_SPEED = 1; // max fall speed while gliding (m/s)
export const GLIDE_HORIZONTAL_SPEED = 6; // horizontal speed limit while gliding (m/s)
export const GLIDE_MIN_GROUND_DISTANCE = 0.12; // model height above the ground (m)
export const JUMP_TO_GLIDE_TIME = 0.52; // s from the latest jump's press (26 steps)
export const GLIDE_COOLDOWN = 0.22; // s after a glide ends (11 steps)
export const GLIDE_TILT_DAMP_TIME = 0.3; // time constant for the glide tilt (animation playback time) to drift toward its target
export const GLIDE_TILT_FULL_ANGLE = 20; // orientation delta (degrees) at which the glide tilt reaches its full-left/full-right pose
// Landing stun: a landing more than LANDING_STUN_DROP below the top of the airtime (the highest point since the ground;
// a glide restarts it from the glide's height) stops horizontal movement for LANDING_STUN_TIME. Jumping is still allowed.
export const LANDING_STUN_DROP = 8; // m
export const LANDING_STUN_TIME = 0.76; // s from the landing step (38 steps)
// 1: the keys held through the stun still accelerate the move, so the gait speed is back on the first step after it
// (in the air the move then builds up from 0 at the air acceleration); 0: the input is ignored and the speed builds up
// again after the stun.
export const LANDING_STUN_KEEP_INPUT: number = 1;
// 1: placing the avatar (a teleport) keeps the top: a fall from where it was placed counts from the ground it stood on
// before or from a top it rises to later, so a teleport high up never stuns; 0: the drop counts from where it was placed.
export const LANDING_STUN_TOP_KEPT_ON_PLACEMENT: number = 1;
// 1: the hard landing clips play only on a stunning landing out of the long fall loop (other stunning landings play the
// usual landing clip); 0: on every stunning landing. Animation only.
export const HARD_LANDING_ONLY_FROM_LONG_FALL: number = 1;

export const TURN_MAX_DEGREES_SEC = 1040; // max degrees to turn (set to inf for TURN_FULL_TIME to apply)
export const TURN_FULL_TIME = 0.1; // time to turn fully towards target
export const GLIDER_TURN_MAX_DEGREES_SEC = 200; // max degrees to turn (set to inf for TURN_FULL_TIME to apply)

export const GROUNDED_HEIGHT = 0.05; // distance from surface at which player is considered "grounded"
// Angle (0-90) from flat up to which ground is ground (can jump / won't slide): a 46 degree slope is walkable, 47 is
// steep (the half degree absorbs single-precision normals).
export const GROUNDED_ANGLE = 46.5;
// Speed on a slope: on the ground the gait speed is scaled by 1 - SLOPE_SPEED_FACTOR * angle / SLOPE_SPEED_MAX_ANGLE,
// with the angle of the ground along the move (positive uphill, clamped to +-SLOPE_SPEED_MAX_ANGLE): uphill slower,
// downhill faster, across unchanged. It scales the horizontal speed; on the ground the reply adds the vertical speed
// that keeps the avatar on the slope. In the air the factor of the last ground holds.
export const SLOPE_SPEED_FACTOR = 0.35;
export const SLOPE_SPEED_MAX_ANGLE = 55; // degrees
export const MAX_STEP_HEIGHT = 0.45; // highest step player can walk up (player may still walk up slightly higher steps if approached at an angle)
export const MIN_STEP_HEIGHT = 0.05; // rises up to this need no step (the capsule slides over them)
export const GROUND_SNAP_HEIGHT = 0.40 // height below which we snap to ground (if previously grounded)
// Behaviour switch, ground snap down a step: 1 = the snap follows the ground under the centre down at most
// GROUND_SNAP_STEP_DROP beyond the ground's slope, so a 0.1 m riser keeps the avatar on the stairs, while from 0.2 m
// walk falls tread by tread and jog/run fly over the whole staircase; 0 = it follows any drop within
// GROUND_SNAP_HEIGHT, so stairs up to that riser keep the avatar on them at any gait.
export const STEP_DOWN_FALLS: number = 1;
export const GROUND_SNAP_STEP_DROP = 0.15; // m
// Sub-step ground probes: downward casts placed along the velocity at multiples of
// GROUND_PROBE_INTERVAL from the avatar. When a tick covers more ground than one 60fps step
// they let ground-snap check each sub-step's drop against GROUND_SNAP_HEIGHT, so stairs and
// slopes stay sticky at low frame rates while a real ledge still falls.
export const GROUND_PROBE_COUNT = 8;
export const GROUND_PROBE_INTERVAL = 1 / 60;
// Cap on the unrequested height gain that ground-snap will pull back down (see snapToGround).
export const MAX_UNREQUESTED_LIFT = 1.0;
// Step-up. The forward casts reach STEP_UP_LOOKAHEAD past the collider, so a riser is found before the capsule
// reaches it. The climb starts when the riser would be reached before the next reply lands and ends as the centre
// comes within STEP_UP_END_DISTANCE of the riser, so the capsule passes over the riser's edge at gait speed instead of
// stopping against it; a riser the capsule already touches is climbed within one frame.
export const STEP_UP_LOOKAHEAD = 1.0; // m
export const STEP_UP_END_DISTANCE = 0.04; // m


// don't edit

export const PLAYER_COLLIDER_RADIUS = 0.3;
export const VEC3_ZERO = Vector3.Zero();
export const VEC3_UP = Vector3.Up();
export const VEC3_DOWN = Vector3.Down();
export const VEC3_FORWARD = Vector3.Forward();
export const VEC3_RIGHT = Vector3.Right();
export const VEC3_INF = Vector3.fromArray([Infinity, Infinity, Infinity]);
export const VEC3_NEG_INF = Vector3.fromArray([-Infinity, -Infinity, -Infinity]);
export const VEC3_HORIZONTAL_MASK = Vector3.create(1, 0, 1);