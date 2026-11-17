import { engine, InputAction, inputSystem, Transform } from '@dcl/sdk/ecs'
import { Quaternion, Vector3 } from '@dcl/sdk/math';
import {
  AIR_JUMP_IMPULSE, AIR_JUMP_STOP_WITHOUT_KEYS, ACCEL_CURVE_END, ACCEL_CURVE_START, AIR_ACCEL, AIR_DRAG, AIR_MAX_ACCEL, EXTERNAL_ENV_DRAG, EXTERNAL_GROUND_FRICTION,
  FIXED_STEP, GLIDER_TURN_MAX_DEGREES_SEC, GROUND_ACCEL, GROUND_MAX_ACCEL, INPUT_DELAY_TICKS, LANDING_STUN_KEEP_INPUT, MAX_EXTERNAL_SPEED,
  MAX_FIXED_STEPS, RAMP_GUARD, SLOPE_SPEED_FACTOR, SLOPE_SPEED_MAX_ANGLE, TURN_FULL_TIME, TURN_MAX_DEGREES_SEC, VEC3_FORWARD, VEC3_HORIZONTAL_MASK, VEC3_RIGHT,
  VEC3_UP, VEC3_ZERO,
} from './constants';
import { horizontalResync, playerRotation, prevActualVelocity, prevExternalVelocity, tickTime, velocity } from '.';
import { grounded, GROUNDED_ANGLE_Y_LEN, groundNormal, groundRayNormal } from './ground';
import { disableOrientation, glidingSpeed, jogSpeed, sprintSpeed, walkSpeed } from './parameters';
import { airLaunchStep, isGliding, landingStuns, launchAhead, predictedLandFrame, steepAhead, stunEndStep, wallContact } from './vertical';
import { getWalkAxis } from './walk';
import { frameTime, replyDelay, tickAdvance } from './clock';
import { settings } from './settings';

export var orientation = 0;
// World-space unit direction of the movement input (zero when there is none).
export var movementAxis = Vector3.Zero();
export var horizontalVelocity = Vector3.Zero();

// ---------------------------------------------------------------- input

// Camera-relative basis (horizontal, unit) and input of one tick.
var fwd = Vector3.Zero();
var right = Vector3.Zero();
// stun: a landing stun holds from the first step (the steps after a predicted touchdown)
type StepInput = { x: number; z: number; limit: number; grounded: boolean; stun: boolean };
const input: StepInput = { x: 0, z: 0, limit: 0, grounded: true, stun: false };
// Input of the previous tick: the steps that cover the time up to this tick's frame use it.
const prevInput: StepInput = { x: 0, z: 0, limit: 0, grounded: true, stun: false };
const landInput: StepInput = { x: 0, z: 0, limit: 0, grounded: true, stun: false };

function readInput() {
  const camera = Transform.get(engine.CameraEntity);
  Vector3.rotateToRef(VEC3_FORWARD, camera.rotation, fwd);
  Vector3.multiplyToRef(fwd, VEC3_HORIZONTAL_MASK, fwd);
  Vector3.normalizeToRef(fwd, fwd);
  Vector3.rotateToRef(VEC3_RIGHT, camera.rotation, right);
  Vector3.multiplyToRef(right, VEC3_HORIZONTAL_MASK, right);
  Vector3.normalizeToRef(right, right);

  // Auto-walk takes priority; getWalkAxis() returns null (and cancels the walk) on manual input.
  const walkAxis = getWalkAxis();
  if (walkAxis !== null) {
    Vector3.copyFrom(walkAxis, movementAxis);
  } else {
    Vector3.copyFrom(VEC3_ZERO, movementAxis);
    if (inputSystem.isPressed(InputAction.IA_LEFT)) Vector3.subtractToRef(movementAxis, right, movementAxis);
    if (inputSystem.isPressed(InputAction.IA_RIGHT)) Vector3.addToRef(movementAxis, right, movementAxis);
    if (inputSystem.isPressed(InputAction.IA_FORWARD)) Vector3.addToRef(movementAxis, fwd, movementAxis);
    if (inputSystem.isPressed(InputAction.IA_BACKWARD)) Vector3.subtractToRef(movementAxis, fwd, movementAxis);
    Vector3.normalizeToRef(movementAxis, movementAxis);
  }

  input.x = snap(Vector3.dot(movementAxis, right));
  input.z = snap(Vector3.dot(movementAxis, fwd));
  input.limit = isGliding ? glidingSpeed
    : inputSystem.isPressed(InputAction.IA_MODIFIER) ? sprintSpeed
    : inputSystem.isPressed(InputAction.IA_WALK) ? walkSpeed
    : jogSpeed;
  input.grounded = grounded;
  if (!isGliding) input.limit *= slopeSpeedFactor();
}

// Gait speed factor of the slope along the move (SLOPE_SPEED_FACTOR), from the walkable ground the capsule rests on (on
// a step edge, the edge's slope), else the ground under the centre; the factor of the last ground holds in the air.
var slopeFactor = 1;
function slopeSpeedFactor(): number {
  const n = groundNormal.y >= GROUNDED_ANGLE_Y_LEN ? groundNormal : groundRayNormal;
  if (grounded && n.y >= GROUNDED_ANGLE_Y_LEN && Vector3.lengthSquared(movementAxis) > 0) {
    // the move direction projected onto the ground plane: its angle above the horizontal
    const un = movementAxis.x * n.x + movementAxis.z * n.z;
    const angle = Math.atan2(-un * n.y, Math.hypot(movementAxis.x - un * n.x, movementAxis.z - un * n.z)) * 180 / Math.PI;
    const clamped = Math.max(-SLOPE_SPEED_MAX_ANGLE, Math.min(SLOPE_SPEED_MAX_ANGLE, angle));
    slopeFactor = 1 - SLOPE_SPEED_FACTOR * clamped / SLOPE_SPEED_MAX_ANGLE;
  }
  return slopeFactor;
}

// Removes float noise from an axis so an unpressed axis is exactly 0.
function snap(a: number): number {
  return Math.abs(a) < 1e-4 ? 0 : a;
}

function copyInput(from: StepInput, to: StepInput) {
  to.x = from.x; to.z = from.z; to.limit = from.limit; to.grounded = from.grounded;
}

// ---------------------------------------------------------------- fixed-step model

// Integration state. ax/az are the camera-relative axis speeds (right, forward), adx/adz their
// smoothing rates, weight the input-hold weight (0..1), vx/vz the world-space move velocity,
// ex/ez the external (impulse) velocity, n the steps run so far (the vertical model's step count).
type State = { ax: number; az: number; adx: number; adz: number; weight: number; vx: number; vz: number; ex: number; ez: number; n: number };
const state: State = { ax: 0, az: 0, adx: 0, adz: 0, weight: 0, vx: 0, vz: 0, ex: 0, ez: 0, n: 0 };
const probe: State = { ax: 0, az: 0, adx: 0, adz: 0, weight: 0, vx: 0, vz: 0, ex: 0, ez: 0, n: 0 };
// Step the air jump launches on (vertical model), read once per tick.
var launchStep = -1;
// Time not yet covered by a step.
var pending = 0;
// Horizontal velocity of the last reply (ramp guard).
var lastReplyX = 0, lastReplyZ = 0;
// First step after the committed landing stun (vertical model), read once per tick.
var stunEnd = -1;
// Step the last reply was predicted for, and whether that step was stunned.
var lastReplyStep = -1;
var lastReplyStun = false;

function copyState(from: State, to: State) {
  to.ax = from.ax; to.az = from.az; to.adx = from.adx; to.adz = from.adz; to.weight = from.weight;
  to.vx = from.vx; to.vz = from.vz; to.ex = from.ex; to.ez = from.ez; to.n = from.n;
}

function moveTowards(current: number, target: number, maxDelta: number): number {
  return Math.abs(target - current) <= maxDelta ? target : current + Math.sign(target - current) * maxDelta;
}

function accelCurve(w: number): number {
  if (w <= ACCEL_CURVE_START) return 0;
  if (w >= ACCEL_CURVE_END) return 1;
  const s = (w - ACCEL_CURVE_START) / (ACCEL_CURVE_END - ACCEL_CURVE_START);
  return s * s * (3 - 2 * s);
}

// Critically damped spring toward target (returns the new value, writes the new rate to rate[0]).
const rateOut = [0];
function smoothDamp(current: number, target: number, rate: number, smoothTime: number, dt: number): number {
  smoothTime = Math.max(0.0001, smoothTime);
  const omega = 2 / smoothTime;
  const x = omega * dt;
  const e = 1 / (1 + x + 0.48 * x * x + 0.235 * x * x * x);
  const change = current - target;
  const temp = (rate + omega * change) * dt;
  let out = target + (change + temp) * e;
  rate = (rate - omega * temp) * e;
  if ((target - current > 0) === (out > target)) {
    out = target;
    rate = 0;
  }
  rateOut[0] = rate;
  return out;
}

function step(s: State, c: StepInput, h: number) {
  s.n++;
  const stun = c.stun || s.n < stunEnd;
  const ignore = stun && !LANDING_STUN_KEEP_INPUT;
  const xTarget = ignore ? 0 : c.limit * c.x;
  const zTarget = ignore ? 0 : c.limit * c.z;
  s.weight = moveTowards(s.weight, xTarget !== 0 || zTarget !== 0 ? 1 : 0, h / Math.max(1e-3, settings.accelTimeGround));
  const k = accelCurve(s.weight);
  const accel = c.grounded ? GROUND_ACCEL + (GROUND_MAX_ACCEL - GROUND_ACCEL) * k : AIR_ACCEL + (AIR_MAX_ACCEL - AIR_ACCEL) * k;

  // A held axis keeps its speed when its key flips sign (strafe A/D, W/S reversal), then moves
  // toward the target; a released axis settles to 0 over the stop time.
  if (c.x !== 0 && !ignore) {
    s.ax = moveTowards(Math.sign(xTarget) * Math.abs(s.ax), xTarget, accel * h);
  } else {
    s.ax = smoothDamp(s.ax, 0, s.adx, settings.dampTimeGround, h);
    s.adx = rateOut[0];
  }
  if (c.z !== 0 && !ignore) {
    s.az = moveTowards(Math.sign(zTarget) * Math.abs(s.az), zTarget, accel * h);
  } else {
    s.az = smoothDamp(s.az, 0, s.adz, settings.dampTimeGround, h);
    s.adz = rateOut[0];
  }

  let tx = right.x * s.ax + fwd.x * s.az;
  let tz = right.z * s.ax + fwd.z * s.az;
  const tl = Math.sqrt(tx * tx + tz * tz);
  if (tl > c.limit) {
    const f = c.limit / tl;
    tx *= f; tz *= f;
  }

  if (c.grounded) {
    // on the ground the move velocity follows the axes directly
    s.vx = tx; s.vz = tz;
    if (s.n === launchStep) airLaunch(s, c);
  } else {
    // in the air it chases them at the air acceleration, against quadratic drag
    const dx = tx - s.vx, dz = tz - s.vz;
    const dl = Math.sqrt(dx * dx + dz * dz);
    const maxD = accel * h;
    if (dl <= maxD) { s.vx = tx; s.vz = tz; } else { s.vx += dx / dl * maxD; s.vz += dz / dl * maxD; }
    if (s.n === launchStep) airLaunch(s, c);
    const sp = Math.sqrt(s.vx * s.vx + s.vz * s.vz);
    if (sp > 0) {
      const f = Math.max(0, 1 - AIR_DRAG * sp * h);
      s.vx *= f; s.vz *= f;
    }
  }
  // a landing stun stops the move velocity (external pushes still apply)
  if (stun) { s.vx = 0; s.vz = 0; }

  const damping = c.grounded ? EXTERNAL_ENV_DRAG + EXTERNAL_GROUND_FRICTION : EXTERNAL_ENV_DRAG;
  const ef = Math.max(0, 1 - damping * h);
  s.ex *= ef; s.ez *= ef;
  const es = s.ex * s.ex + s.ez * s.ez;
  if (es < 1e-4) { s.ex = 0; s.ez = 0; }
  else if (es > MAX_EXTERNAL_SPEED * MAX_EXTERNAL_SPEED) { const f = MAX_EXTERNAL_SPEED / Math.sqrt(es); s.ex *= f; s.ez *= f; }
}

// Air jump launch: the move velocity turns to the keys held on this step, at least AIR_JUMP_IMPULSE fast.
function airLaunch(s: State, c: StepInput) {
  if (!AIR_JUMP_STOP_WITHOUT_KEYS && c.x === 0 && c.z === 0) return;
  const v = Math.max(AIR_JUMP_IMPULSE, Math.sqrt(s.vx * s.vx + s.vz * s.vz));
  s.ax = v * c.x; s.az = v * c.z; s.adx = 0; s.adz = 0;
  s.vx = right.x * s.ax + fwd.x * s.az;
  s.vz = right.z * s.ax + fwd.z * s.az;
}

// Runs the whole steps that fit into `pending` (plus `extra`), returns the time left over.
function advance(s: State, c: StepInput, time: number): number {
  let n = 0;
  while (time >= FIXED_STEP - 1e-9 && n < MAX_FIXED_STEPS) {
    step(s, c, FIXED_STEP);
    time -= FIXED_STEP;
    n++;
  }
  return n === MAX_FIXED_STEPS ? 0 : time;
}

export function updateHorizontalVelocity() {
  readInput();
  launchStep = airLaunchStep();
  stunEnd = stunEndStep();

  if (horizontalResync) {
    // the engine moved us at another velocity: continue from what it applied
    state.vx = prevActualVelocity.x; state.vz = prevActualVelocity.z;
    state.ax = state.vx * right.x + state.vz * right.z;
    state.az = state.vx * fwd.x + state.vz * fwd.z;
    state.adx = 0; state.adz = 0; state.ex = 0; state.ez = 0;
  }
  state.ex += prevExternalVelocity.x;
  state.ez += prevExternalVelocity.z;

  // Cover the time up to the end of this frame with the previous tick's input.
  pending = advance(state, INPUT_DELAY_TICKS > 0 ? prevInput : input, pending + tickAdvance);

  // The engine holds the reply from the frame it lands on until the next reply lands; reply
  // with the state predicted (on this tick's input) for the last of those frames.
  let ox = state.vx + state.ex, oz = state.vz + state.ez;
  let replyStep = state.n, replyStun = false;
  if (replyDelay > 0 && frameTime > 0) {
    copyState(state, probe);
    const frames = 2 * replyDelay;
    if (!input.grounded && predictedLandFrame >= 0 && predictedLandFrame < replyDelay) {
      // the avatar touches the ground before the reply lands, so every frame it is held for
      // comes after the touchdown: the steps after that frame are ground steps (the first
      // grounded step sets the move velocity to its target). A touchdown inside the held
      // frames keeps the air velocity, so the touchdown frame still moves at air speed.
      copyInput(input, landInput);
      landInput.grounded = true;
      landInput.stun = landingStuns();
      const rest = advance(probe, input, pending + predictedLandFrame * frameTime);
      advance(probe, landInput, rest + (frames - predictedLandFrame) * frameTime);
      replyStun = landInput.stun;
    } else {
      advance(probe, input, pending + frames * frameTime);
    }
    ox = probe.vx + probe.ex; oz = probe.vz + probe.ez;
    replyStep = probe.n;
  } else if (launchAhead) {
    // the vertical reply already carries a launch the committed steps reach after this frame: so does this one,
    // with this tick's input for the launch step (a ground move), so a jump with the keys released goes straight up
    copyState(state, probe);
    advance(probe, input, Math.max(pending, FIXED_STEP));
    ox = probe.vx + probe.ex; oz = probe.vz + probe.ez;
    replyStep = probe.n;
  }
  // A reply from a stunned step: the guard below does not hold the speed from before the stun, only the end of the
  // stun once a reply has already passed it.
  replyStun = replyStun || replyStep < stunEnd;
  // a stun a reply has published holds: until the committed landing, a later reply that does not reach the touchdown
  // keeps it
  if (!replyStun && lastReplyStun && landingStuns()) {
    ox = state.ex; oz = state.ez;
    replyStun = true;
  }

  // Ramp guard. The delay estimate follows the previous reply's delay, which does not predict
  // this one, so consecutive replies alternate between the predicted and the committed state.
  // During a ground acceleration that reads as a speed dip of up to one prediction horizon
  // (0.5-0.9 m/s), which the rigidly attached camera shows as a jerk. While the same input is
  // held on the ground and the model is still accelerating, a reply never slows below the last
  // one; the committed state catches up within a tick or two.
  if ((RAMP_GUARD & 1) && !horizontalResync && !(replyStun && lastReplyStep < stunEnd) && (input.x !== 0 || input.z !== 0) && input.grounded && prevInput.grounded
    && input.x === prevInput.x && input.z === prevInput.z && input.limit === prevInput.limit
    && state.ex === 0 && state.ez === 0) {
    const committed = Math.sqrt(state.vx * state.vx + state.vz * state.vz);
    const out = Math.sqrt(ox * ox + oz * oz);
    const last = Math.sqrt(lastReplyX * lastReplyX + lastReplyZ * lastReplyZ);
    if (committed < input.limit - 1e-3 && out < last) {
      if (out < 1e-6) {
        // the committed steps have not started yet: keep the last reply
        const f = Math.min(1, input.limit / last);
        ox = lastReplyX * f; oz = lastReplyZ * f;
      } else if (ox * lastReplyX + oz * lastReplyZ > 0) {
        const f = Math.min(last, input.limit) / out;
        ox *= f; oz *= f;
      }
    }
  }
  // A slope ahead too steep to walk (steeper than GROUNDED_ANGLE) stops the move into it at the contact, where the
  // engine would slide the avatar up it: the speed toward it is cut to reach it no sooner than the next reply lands.
  const steep = input.grounded ? steepAhead() : null;
  if (steep !== null) {
    const into = -(ox * steep.x + oz * steep.z);
    const allowed = steep.distance / ((2 * replyDelay + 1) * (frameTime > 0 ? frameTime : FIXED_STEP));
    if (into > allowed) {
      ox += (into - allowed) * steep.x;
      oz += (into - allowed) * steep.z;
    }
  }

  // Against a wall on the ground the move slows with how squarely it meets the wall: by 1 - |cos| of the angle between
  // the move and the wall's normal, so head-on it stops and at 45 degrees it slides along the wall at 0.29 x 0.71 of
  // the speed. In the air a wall only blocks the move.
  const wall = input.grounded ? wallContact() : null;
  if (wall !== null) {
    const speed = Math.hypot(ox, oz);
    const into = speed > 0 ? -(ox * wall.x + oz * wall.z) / speed : 0;
    if (into > 0) { ox *= 1 - into; oz *= 1 - into; }
  }

  lastReplyX = ox; lastReplyZ = oz; lastReplyStep = replyStep; lastReplyStun = replyStun;
  copyInput(input, prevInput);

  velocity.x = ox;
  velocity.z = oz;
  Vector3.multiplyToRef(velocity, VEC3_HORIZONTAL_MASK, horizontalVelocity);
  setOrientation();
}

// ---------------------------------------------------------------- orientation

export var targetOrientation = 0;
var scratch: Vector3 = Vector3.Zero();
var targetFacing = Quaternion.Identity();

// returns equivalent angle within 180 degrees of center
export function relativeDegrees(center: number, angle: number): number {
  return center + ((angle - center + 180) % 360 + 360) % 360 - 180;
}

function setOrientation() {
  // Always adopt the current avatar transform as the orientation baseline, even when
  // locked, so an engine-imposed facing (e.g. a movePlayerTo cameraTarget) takes effect
  // instead of freezing at the pre-teleport orientation.
  orientation = Quaternion.toEulerAngles(playerRotation).y;
  if (disableOrientation) {
    targetOrientation = orientation;
    return;
  }

  let turnSpeed = isGliding ? GLIDER_TURN_MAX_DEGREES_SEC : TURN_MAX_DEGREES_SEC;

  // While gliding, face the direction of travel rather than raw input, so a glider banks
  // instead of snapping to face the stick. Falls back to input direction when nearly stopped.
  let faceDir = movementAxis;
  if (isGliding) {
    Vector3.multiplyToRef(velocity, VEC3_HORIZONTAL_MASK, scratch);
    if (Vector3.lengthSquared(scratch) > 0.25) {
      Vector3.normalizeToRef(scratch, scratch);
      faceDir = scratch;
    }
  }

  if (Vector3.length(faceDir) != 0) {
    Quaternion.fromLookAtToRef(VEC3_ZERO, faceDir, VEC3_UP, targetFacing);
    targetOrientation = Quaternion.toEulerAngles(targetFacing).y;
  } else {
    targetOrientation = orientation;
  }

  if (targetOrientation != orientation) {
    if (Math.abs(targetOrientation - orientation) < 1) {
      orientation = targetOrientation;
    } else {
      orientation = relativeDegrees(targetOrientation, orientation);
      let perc = Math.min(tickTime / TURN_FULL_TIME, 1);
      orientation = Math.max(
        orientation - turnSpeed * tickTime,
        Math.min(
          orientation + turnSpeed * tickTime,
          targetOrientation * perc + orientation * (1 - perc)
        ));
      orientation = relativeDegrees(180, orientation);
    }
  }
}
