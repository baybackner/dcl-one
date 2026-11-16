import { engine, InputAction, inputSystem, Transform } from '@dcl/sdk/ecs'
import { Quaternion, Vector3 } from '@dcl/sdk/math';
import {
  ACCEL_CURVE_END, ACCEL_CURVE_START, AIR_ACCEL, AIR_DRAG, AIR_MAX_ACCEL, EXTERNAL_ENV_DRAG, EXTERNAL_GROUND_FRICTION,
  FIXED_STEP, GLIDER_TURN_MAX_DEGREES_SEC, GROUND_ACCEL, GROUND_MAX_ACCEL, INPUT_DELAY_TICKS, MAX_EXTERNAL_SPEED,
  MAX_FIXED_STEPS, RAMP_GUARD, TURN_FULL_TIME, TURN_MAX_DEGREES_SEC, VEC3_FORWARD, VEC3_HORIZONTAL_MASK, VEC3_RIGHT,
  VEC3_UP, VEC3_ZERO,
} from './constants';
import { horizontalResync, landRecoverUntil, playerRotation, prevActualVelocity, prevExternalVelocity, tickTime, time, velocity } from '.';
import { grounded } from './ground';
import { disableOrientation, glidingSpeed, jogSpeed, sprintSpeed, walkSpeed } from './parameters';
import { isGliding, launchAhead, predictedLandFrame } from './vertical';
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
type StepInput = { x: number; z: number; limit: number; grounded: boolean };
const input: StepInput = { x: 0, z: 0, limit: 0, grounded: true };
// Input of the previous tick: the steps that cover the time up to this tick's frame use it.
const prevInput: StepInput = { x: 0, z: 0, limit: 0, grounded: true };
const landInput: StepInput = { x: 0, z: 0, limit: 0, grounded: true };

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

  const stunned = time < landRecoverUntil;
  input.x = stunned ? 0 : snap(Vector3.dot(movementAxis, right));
  input.z = stunned ? 0 : snap(Vector3.dot(movementAxis, fwd));
  input.limit = isGliding ? glidingSpeed
    : inputSystem.isPressed(InputAction.IA_MODIFIER) ? sprintSpeed
    : inputSystem.isPressed(InputAction.IA_WALK) ? walkSpeed
    : jogSpeed;
  input.grounded = grounded;
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
// ex/ez the external (impulse) velocity.
type State = { ax: number; az: number; adx: number; adz: number; weight: number; vx: number; vz: number; ex: number; ez: number };
const state: State = { ax: 0, az: 0, adx: 0, adz: 0, weight: 0, vx: 0, vz: 0, ex: 0, ez: 0 };
const probe: State = { ax: 0, az: 0, adx: 0, adz: 0, weight: 0, vx: 0, vz: 0, ex: 0, ez: 0 };
// Time not yet covered by a step.
var pending = 0;
// Horizontal velocity of the last reply (ramp guard).
var lastReplyX = 0, lastReplyZ = 0;

function copyState(from: State, to: State) {
  to.ax = from.ax; to.az = from.az; to.adx = from.adx; to.adz = from.adz; to.weight = from.weight;
  to.vx = from.vx; to.vz = from.vz; to.ex = from.ex; to.ez = from.ez;
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
  const xTarget = c.limit * c.x;
  const zTarget = c.limit * c.z;
  s.weight = moveTowards(s.weight, xTarget !== 0 || zTarget !== 0 ? 1 : 0, h / Math.max(1e-3, settings.accelTimeGround));
  const k = accelCurve(s.weight);
  const accel = c.grounded ? GROUND_ACCEL + (GROUND_MAX_ACCEL - GROUND_ACCEL) * k : AIR_ACCEL + (AIR_MAX_ACCEL - AIR_ACCEL) * k;

  // A held axis keeps its speed when its key flips sign (strafe A/D, W/S reversal), then moves
  // toward the target; a released axis settles to 0 over the stop time.
  if (c.x !== 0) {
    s.ax = moveTowards(Math.sign(xTarget) * Math.abs(s.ax), xTarget, accel * h);
  } else {
    s.ax = smoothDamp(s.ax, 0, s.adx, settings.dampTimeGround, h);
    s.adx = rateOut[0];
  }
  if (c.z !== 0) {
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
  } else {
    // in the air it chases them at the air acceleration, against quadratic drag
    const dx = tx - s.vx, dz = tz - s.vz;
    const dl = Math.sqrt(dx * dx + dz * dz);
    const maxD = accel * h;
    if (dl <= maxD) { s.vx = tx; s.vz = tz; } else { s.vx += dx / dl * maxD; s.vz += dz / dl * maxD; }
    const sp = Math.sqrt(s.vx * s.vx + s.vz * s.vz);
    if (sp > 0) {
      const f = Math.max(0, 1 - AIR_DRAG * sp * h);
      s.vx *= f; s.vz *= f;
    }
  }

  const damping = c.grounded ? EXTERNAL_ENV_DRAG + EXTERNAL_GROUND_FRICTION : EXTERNAL_ENV_DRAG;
  const ef = Math.max(0, 1 - damping * h);
  s.ex *= ef; s.ez *= ef;
  const es = s.ex * s.ex + s.ez * s.ez;
  if (es < 1e-4) { s.ex = 0; s.ez = 0; }
  else if (es > MAX_EXTERNAL_SPEED * MAX_EXTERNAL_SPEED) { const f = MAX_EXTERNAL_SPEED / Math.sqrt(es); s.ex *= f; s.ez *= f; }
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
      const rest = advance(probe, input, pending + predictedLandFrame * frameTime);
      advance(probe, landInput, rest + (frames - predictedLandFrame) * frameTime);
    } else {
      advance(probe, input, pending + frames * frameTime);
    }
    ox = probe.vx + probe.ex; oz = probe.vz + probe.ez;
  } else if (launchAhead) {
    // the vertical reply already carries a launch the committed steps reach after this frame: so does this one,
    // with this tick's input for the launch step (a ground move), so a jump with the keys released goes straight up
    copyState(state, probe);
    advance(probe, input, Math.max(pending, FIXED_STEP));
    ox = probe.vx + probe.ex; oz = probe.vz + probe.ez;
  }

  // Ramp guard. The delay estimate follows the previous reply's delay, which does not predict
  // this one, so consecutive replies alternate between the predicted and the committed state.
  // During a ground acceleration that reads as a speed dip of up to one prediction horizon
  // (0.5-0.9 m/s), which the rigidly attached camera shows as a jerk. While the same input is
  // held on the ground and the model is still accelerating, a reply never slows below the last
  // one; the committed state catches up within a tick or two.
  if ((RAMP_GUARD & 1) && !horizontalResync && (input.x !== 0 || input.z !== 0) && input.grounded && prevInput.grounded
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
  lastReplyX = ox; lastReplyZ = oz;
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
