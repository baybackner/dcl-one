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
export const RAMP_GUARD: number = 3;
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
export const LONG_JUMP_TIME = 0.5; // s after the launch during which a held key scales gravity
export const LONG_JUMP_GRAVITY_SCALE = 0.5;
// Steps a press is buffered before landing, and steps after leaving the ground that a jump
// is still allowed (one window for both; off while moving up).
export const JUMP_GRACE_STEPS = 8;
export const JUMP_COYOTE_TIME = 0.16; // s, the same window for the step-up check
export const DOUBLE_JUMP_HEIGHT = 2; // in-air jump height (in-air jumps are disabled)
// Glide: a fresh press in the air after the jump (or after the grace window when falling)
// opens the glider while held, once long enough after the launch and above the ground.
export const GLIDE_FALL_SPEED = 1; // max fall speed while gliding (m/s)
export const GLIDE_HORIZONTAL_SPEED = 6; // horizontal speed limit while gliding (m/s)
export const GLIDE_MIN_GROUND_DISTANCE = 0.2;
export const JUMP_TO_GLIDE_TIME = 0.5;
export const GLIDE_COOLDOWN = 0.2;
export const GLIDE_TILT_DAMP_TIME = 0.3; // time constant for the glide tilt (animation playback time) to drift toward its target
export const GLIDE_TILT_FULL_ANGLE = 20; // orientation delta (degrees) at which the glide tilt reaches its full-left/full-right pose

export const TURN_MAX_DEGREES_SEC = 1040; // max degrees to turn (set to inf for TURN_FULL_TIME to apply)
export const TURN_FULL_TIME = 0.1; // time to turn fully towards target
export const GLIDER_TURN_MAX_DEGREES_SEC = 200; // max degrees to turn (set to inf for TURN_FULL_TIME to apply)

export const GROUNDED_HEIGHT = 0.05; // distance from surface at which player is considered "grounded"
export const GROUNDED_ANGLE = 47.5; // angle (0-90) from flat at which ground is considered ground (can jump / won't slide)
export const MAX_STEP_HEIGHT = 0.45; // highest step player can walk up (player may still walk up slightly higher steps if approached at an angle)
export const MIN_STEP_HEIGHT = 0.1; // landings closer than this to current ground don't need a step (the capsule slides over smaller obstacles)
export const STEP_CLEARANCE_EXCESS = 0.2; // clearance required at step height beyond the feet-level hit distance before stepping up
export const GROUND_SNAP_HEIGHT = 0.40 // height below which we snap to ground (if previously grounded)
// Sub-step ground probes: downward casts placed along the velocity at multiples of
// GROUND_PROBE_INTERVAL from the avatar. When a tick covers more ground than one 60fps step
// they let ground-snap check each sub-step's drop against GROUND_SNAP_HEIGHT, so stairs and
// slopes stay sticky at low frame rates while a real ledge still falls.
export const GROUND_PROBE_COUNT = 8;
export const GROUND_PROBE_INTERVAL = 1 / 60;
// Cap on the unrequested height gain that ground-snap will pull back down (see snapToGround).
export const MAX_UNREQUESTED_LIFT = 1.0;
// Time over which a step-up climbs to its landing. A velocity can be held for several engine
// frames when the scene misses ticks, so the climb is sized to a fixed time rather than a
// single frame, which would overshoot when held.
export const POSITION_CORRECTION_TIME = 0.1;


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