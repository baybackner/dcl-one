import { ColliderLayer, engine, Entity, InputAction, inputSystem, RaycastQueryType, RaycastShape, raycastSystem, RaycastSystemCallback, Transform } from '@dcl/sdk/ecs'
import { Vector3 } from '@dcl/sdk/math';
import { groundDistance, grounded, GROUNDED_ANGLE_Y_LEN, lastGroundTime, prevGrounded, probesAllowSnap, setGrounded } from './ground';
import {
  EXTERNAL_UP_DAMP_TIME, EXTERNAL_UP_DECEL, FIXED_STEP, GLIDE_COOLDOWN, GLIDE_MIN_GROUND_DISTANCE, GRAVITY,
  GRAVITY_RISE_FACTOR, GROUND_SNAP_HEIGHT, INPUT_DELAY_TICKS, JUMP_COYOTE_TIME, JUMP_GRACE_STEPS, JUMP_TO_GLIDE_TIME,
  LONG_JUMP_GRAVITY_SCALE, LONG_JUMP_TIME, MAX_FIXED_STEPS, MAX_STEP_HEIGHT, MAX_UNREQUESTED_LIFT, MIN_STEP_HEIGHT,
  PLAYER_COLLIDER_RADIUS, POSITION_CORRECTION_TIME, RAMP_GUARD, STEP_CLEARANCE_EXCESS, VEC3_ZERO,
  VERTICAL_PREDICT, VERTICAL_TRACK_BLOCK, VERTICAL_TRACK_MAX, VERTICAL_TRACK_TIME,
} from './constants';
import { engineDriven, horizontalResync, playerPosition, prevPlayerPosition, prevActualVelocity, prevExternalVelocity, prevRequestedVelocity, stepTime, tickTime, time, velocity } from '.';
import { movementAxis } from './horizontal';
import { glideEnabled, glidingFallingSpeed, jumpHeight, maxAirJumps, maxGroundJumps, sprintJumpHeight, sprintSpeed } from './parameters';
import { frameTime, replyDelay, replyDelaySmooth, tickAdvance } from './clock';

const G = -GRAVITY.y;

export function updateVerticalVelocity() {
  if (engineDriven) {
    // Hold the grounded state while the engine drives us (see engineDriven): a lerp lifting
    // us off the ground must not read as a fall, nor arrive as a landing. A jump press still
    // goes through so the player can cancel the move.
    // ... unless an upward push launches us off it
    setGrounded(isLaunched() ? false : prevGrounded);
    unrequestedLift = 0;
    applyJumpAndGravity(true);
    return;
  }
  stepUp();
  applyJumpAndGravity(false);
  snapToGround();
}

// ---------------------------------------------------------------- jump and gravity
// Integrated in fixed steps on the same clock as the horizontal model (see clock.ts).

// Height the current (or next) jump was launched for (a tapped jump's apex).
export var currentJumpHeight = 0;
// Feet height at launch while the jump is rising; undefined otherwise.
export var jumpStartHeight: number | undefined = undefined;
// In-air jumps are not supported; kept for the animation selector.
export var isDoubleJump = false;
export var isGliding = false;

type VState = {
  vy: number; // vertical velocity from gravity and jumps
  step: number; // steps run so far
  lastGroundedStep: number;
  triggerStep: number; // step the latest press is due on
  consumedStep: number; // step the latest jump launched on
  jumpCount: number; // jumps since the ground (falling past the grace window counts as one)
  glideWanted: boolean;
  gliding: boolean;
  glideEndStep: number;
  launched: boolean; // a jump launched during the last advance
  y: number; // model feet height: the ground height while on the ground, integrated in the air
};
const NEVER = -1e9;
const vstate: VState = { vy: 0, step: 0, lastGroundedStep: NEVER, triggerStep: NEVER, consumedStep: NEVER, jumpCount: 0, glideWanted: false, gliding: false, glideEndStep: NEVER, launched: false, y: 0 };
const vprobe: VState = { ...vstate };
var vpending = 0;
// Vertical velocity from external impulses and forces, kept apart from vy.
var externalY = 0;
var jumpWasPressed = false;
var prevJumpPressed = false;
var prevRunning = false;
// The published reply includes a launch that the committed steps have not reached yet.
var launchPending = false;
// A launch is published ahead of the committed steps this tick (read by the horizontal reply).
export var launchAhead = false;
// Frames from this tick's dispatch frame to the frame the avatar is predicted to touch the
// ground (0 = during this frame), within the frames this reply is held; -1 otherwise.
export var predictedLandFrame = -1;

function copyVState(from: VState, to: VState) {
  to.vy = from.vy; to.step = from.step; to.lastGroundedStep = from.lastGroundedStep; to.triggerStep = from.triggerStep;
  to.consumedStep = from.consumedStep; to.jumpCount = from.jumpCount; to.glideWanted = from.glideWanted;
  to.gliding = from.gliding; to.glideEndStep = from.glideEndStep; to.launched = from.launched; to.y = from.y;
}

function launchSpeed(running: boolean): number {
  // Use the more conservative of prev requested vs prev actual so external forces (impulses,
  // moving platforms) don't inflate the running jump.
  const req = Math.sqrt(prevRequestedVelocity.x * prevRequestedVelocity.x + prevRequestedVelocity.z * prevRequestedVelocity.z);
  const act = Math.sqrt(prevActualVelocity.x * prevActualVelocity.x + prevActualVelocity.z * prevActualVelocity.z);
  const maxHeight = running ? sprintJumpHeight : jumpHeight;
  const k = sprintSpeed > 0 ? Math.min(1, Math.max(0, Math.min(req, act) / sprintSpeed)) : 0;
  currentJumpHeight = jumpHeight + (maxHeight - jumpHeight) * k;
  return Math.sqrt(2 * Math.max(0, currentJumpHeight) * G * GRAVITY_RISE_FACTOR);
}

function vstep(s: VState, pressed: boolean, running: boolean, onGroundTick: boolean, noGravity: boolean, h: number) {
  s.step++;
  const vy0 = s.vy;
  let justJumped = false;
  const onGround = onGroundTick && s.vy <= 0;
  if (onGround) {
    s.lastGroundedStep = s.step;
    s.jumpCount = 0;
  }

  // the grace window both buffers an early press and allows a late jump off an edge
  const grace = s.vy > 0 || s.step < JUMP_GRACE_STEPS ? 0 : JUMP_GRACE_STEPS;
  const coyote = s.step - s.lastGroundedStep < grace;
  if (!onGround && !coyote) s.jumpCount = Math.max(s.jumpCount, 1);
  const canJump = s.jumpCount === 0 && (onGround || coyote) && maxGroundJumps > 0 && jumpHeight > 0;
  const wants = s.step === s.triggerStep || s.step <= s.triggerStep + grace;
  if (canJump && wants) {
    s.vy = launchSpeed(running);
    justJumped = true;
    s.launched = true;
    s.jumpCount++;
    s.triggerStep = NEVER;
    s.consumedStep = s.step;
  }

  if (!onGround || justJumped) {
    if (!noGravity || s.vy > 0) {
      let g = G;
      if (pressed && (s.step - s.consumedStep) * h < LONG_JUMP_TIME) g *= LONG_JUMP_GRAVITY_SCALE;
      if (s.vy > 0) g *= GRAVITY_RISE_FACTOR;
      s.vy -= g * h;
      if (noGravity) s.vy = Math.max(0, s.vy);
    }
  } else {
    s.vy = 0;
  }

  // glide: a fresh press in the air once the ground jump is spent, held to keep gliding
  const canGlide = glideEnabled && s.jumpCount > maxAirJumps && !onGround && groundDistance > GLIDE_MIN_GROUND_DISTANCE;
  s.glideWanted = (s.glideWanted || (canGlide && s.step === s.triggerStep)) && pressed;
  const ready = (s.step - s.consumedStep) * h >= JUMP_TO_GLIDE_TIME && (s.step - s.glideEndStep) * h >= GLIDE_COOLDOWN;
  if (!s.gliding && s.glideWanted && canGlide && ready) {
    s.gliding = true;
  } else if (s.gliding && (!pressed || !canGlide)) {
    s.gliding = false;
    s.glideEndStep = s.step;
  }
  if (s.gliding) s.vy = Math.max(s.vy, -glidingFallingSpeed);
  // height: gravity is constant within a step, so the step moves the mean of its start and end velocity
  s.y += 0.5 * (vy0 + s.vy) * h;
}

function vadvance(s: VState, pressed: boolean, running: boolean, onGround: boolean, noGravity: boolean, t: number): number {
  let n = 0;
  while (t >= FIXED_STEP - 1e-9 && n < MAX_FIXED_STEPS) {
    vstep(s, pressed, running, onGround, noGravity, FIXED_STEP);
    t -= FIXED_STEP;
    n++;
    // after a launch the ground is left behind for the rest of the tick
    if (s.launched) onGround = false;
  }
  return n === MAX_FIXED_STEPS ? 0 : t;
}

function applyJumpAndGravity(noGravity: boolean) {
  const pressed = inputSystem.isPressed(InputAction.IA_JUMP);
  // a running jump: sprint held with a movement key down
  const running = inputSystem.isPressed(InputAction.IA_MODIFIER) && (
    inputSystem.isPressed(InputAction.IA_FORWARD) || inputSystem.isPressed(InputAction.IA_BACKWARD)
    || inputSystem.isPressed(InputAction.IA_LEFT) || inputSystem.isPressed(InputAction.IA_RIGHT)
    || Vector3.lengthSquared(movementAxis) > 0);

  if (horizontalResync) {
    // the engine moved us at another velocity: continue from what it applied
    vstate.vy = prevActualVelocity.y;
    externalY = 0;
  }

  // external vertical velocity: upward fades, downward is stopped by the ground
  externalY += prevExternalVelocity.y;
  if (externalY > 0) {
    externalY *= Math.exp(-tickTime / EXTERNAL_UP_DAMP_TIME);
    externalY = Math.max(0, externalY - EXTERNAL_UP_DECEL * tickTime);
  } else if (grounded) {
    externalY = 0;
  }

  // a published launch the committed steps have not reached yet: the model is still on the ground, even when the
  // published launch has already lifted the avatar past the grounded height
  const onGround = (grounded || launchPending) && !isLaunched();
  // on the ground the model stands where the avatar stands
  if (onGround && vstate.vy <= 0) vstate.y = playerPosition.y - Math.max(0, groundDistance);
  const stepPressed = INPUT_DELAY_TICKS > 0 ? prevJumpPressed : pressed;
  const stepRunning = INPUT_DELAY_TICKS > 0 ? prevRunning : running;
  vstate.launched = false;
  vpending = vadvance(vstate, stepPressed, stepRunning, onGround, noGravity, vpending + tickAdvance);
  if (vstate.launched) {
    jumpStartHeight = playerPosition.y;
    stepping = false;
    setGrounded(false);
  }

  // a press is due on the first step after this frame
  if (pressed && !jumpWasPressed) vstate.triggerStep = vstate.step + 1;
  jumpWasPressed = pressed;
  prevJumpPressed = pressed;
  prevRunning = running;
  if (vstate.launched || vstate.triggerStep === NEVER) launchPending = false;

  // The reply is held from the frame it lands on (replyDelay frames after this one) until the
  // next reply lands (2 * replyDelay frames after this one). Answer with the mean of the
  // predicted per-frame velocities over those frames, so the height the engine integrates
  // over them is the height the model reaches (a rise or fall is not cut short).
  // A launch the prediction has already published but the committed steps have not reached
  // yet (launchPending) is kept: the prediction then covers at least the next step.
  let vy = vstate.vy;
  const frames = frameTime > 0 ? 2 * replyDelay : 0;
  // the smoothed horizon once the committed model is airborne; a launch from the ground keeps the per-tick
  // delay (a smoothed horizon would publish a launch before the press frame's reply is due)
  const smooth = VERTICAL_PREDICT === 2 && frameTime > 0 && (vstate.vy !== 0 || vstate.launched);
  const ahead = smooth ? 1.5 * replyDelaySmooth * frameTime : 0;
  if (vstate.launched) jumpTracked = true;
  if ((smooth ? ahead > 1e-4 : frames > 0) || launchPending) {
    copyVState(vstate, vprobe);
    vprobe.launched = false;
    let ground = onGround && !vstate.launched;
    if (smooth) {
      vadvance(vprobe, pressed, running, ground, noGravity, vpending + Math.max(ahead, launchPending ? FIXED_STEP : 0));
      vy = vprobe.vy;
    } else if (frames > 0) {
      let acc = vpending, sum = 0, n = 0;
      for (let f = 1; f <= frames; f++) {
        acc = vadvance(vprobe, pressed, running, ground, noGravity, acc + frameTime);
        if (vprobe.launched) ground = false;
        if (f >= replyDelay) { sum += vprobe.vy; n++; }
      }
      vy = sum / n;
    } else {
      vadvance(vprobe, pressed, running, ground, noGravity, Math.max(vpending, FIXED_STEP));
      vy = vprobe.vy;
    }
    if (vprobe.launched && !vstate.launched) launchPending = true;
    if (vprobe.launched) jumpTracked = true;
  }
  if (launchPending && jumpStartHeight === undefined) {
    // published launch: no snap or step-up pulls the avatar back. The grounded state stays, so
    // the launch step's horizontal move is still a ground move.
    jumpStartHeight = playerPosition.y;
    stepping = false;
  }
  // Until the committed steps launch, the model is on the ground even when the published launch has already
  // lifted the avatar past the grounded height: the launch step's horizontal move stays a ground move.
  launchAhead = launchPending && !vstate.launched;
  if (launchAhead) setGrounded(true);

  // Height tracking (VERTICAL_TRACK_TIME): from a launch (committed or published) until the model lands.
  landedNow = jumpTracked && onGround && vstate.vy <= 0 && !vstate.launched && !launchPending;
  // Tracking stops when the engine did not move the avatar as asked: a rise cut short (a ceiling) or a lift it did not
  // ask for (a platform, a slope). The gap to the model is then geometry, not reply timing. In free flight the applied
  // vertical velocity equals the requested one; the ground stopping a fall is the landing, handled below.
  const blocked = (prevRequestedVelocity.y > 0 && prevActualVelocity.y < prevRequestedVelocity.y - VERTICAL_TRACK_BLOCK)
    || prevActualVelocity.y > Math.max(0, prevRequestedVelocity.y) + VERTICAL_TRACK_BLOCK;
  if (jumpTracked && (vstate.gliding || stepping || externalY !== 0 || horizontalResync || noGravity || blocked
    || (onGround && vstate.vy <= 0 && !vstate.launched && !launchPending))) jumpTracked = false;
  if (VERTICAL_TRACK_MAX > 0 && VERTICAL_TRACK_TIME > 0 && jumpTracked && frameTime > 0) {
    // compared at the start of this frame, where the avatar's height is known whichever reply the engine applies
    // over this frame (the model state is at the end of this frame minus the part of a step not yet run)
    const err = vstate.y + vstate.vy * (vpending - frameTime) - playerPosition.y;
    let c = Math.max(-VERTICAL_TRACK_MAX, Math.min(VERTICAL_TRACK_MAX, err / VERTICAL_TRACK_TIME));
    // never against the model's direction: no hop while it falls, no dip while it rises
    if (vy + c > 0 && vy <= 0) c = -vy;
    if (vy + c < 0 && vy > 0) c = -vy;
    // a correction upward never makes the reply rise above both the model's own reply and the last airborne reply
    // (the ramp guard below only covers ticks without a launch; this covers the launch ticks too)
    const model = vy;
    vy += c;
    if (c > 0 && lastReplyVy !== 0) vy = Math.min(vy, Math.max(model, lastReplyVy));
  }

  // Predicted landing: frames from this one until the engine puts the avatar on the ground,
  // with the previous reply held for the frames before this one's lands (-1: not in the window).
  predictedLandFrame = -1;
  if (!grounded && frameTime > 0 && !vstate.gliding && groundDistance < Infinity) {
    let h = groundDistance;
    for (let k = 0; k <= frames; k++) {
      h += (k < replyDelay ? prevActualVelocity.y : vy + externalY) * frameTime;
      if (h <= 0) { predictedLandFrame = k; break; }
    }
  }

  isGliding = vstate.gliding;
  if ((vstate.vy <= 0 && !launchPending) || isGliding) jumpStartHeight = undefined;

  // Vertical ramp guard: in the air gravity only lowers vy, but the switching delay estimate
  // makes consecutive replies alternate between the committed and the predicted state. Between
  // two airborne replies without a launch, glide, landing or push, a reply never rises above
  // the last one.
  const air = !grounded && !vstate.launched && !launchPending && !isGliding && externalY === 0
    && !horizontalResync && !stepping && predictedLandFrame < 0;
  if ((RAMP_GUARD & 2) && air && lastReplyAir) vy = Math.min(vy, lastReplyVy);
  lastReplyVy = vy;
  lastReplyAir = air;

  // a step-up owns the vertical velocity while it climbs
  if (stepping) return;
  velocity.y = vy + externalY;
}
// A jump is in progress (launched or published): replies track the model's height (VERTICAL_TRACK_MAX).
var jumpTracked = false;
// The model landed a tracked jump this tick (the avatar may still be up to the grounded height above the ground).
var landedNow = false;
// Vertical velocity of the last reply and whether it was airborne (ramp guard).
var lastReplyVy = 0;
var lastReplyAir = false;

// An upward push stronger than gravity is meant to lift the avatar, so like a jump it leaves
// the ground rather than being snapped back down. A weaker one (externalVelocity is summed
// over the tick, so a force shows up as force * tickTime) stays on the ground.
function isLaunched(): boolean {
  return prevExternalVelocity.y > -GRAVITY.y * tickTime;
}

export var snapSpeed = 0;
// Net height gained beyond what we asked for. The engine's collide-and-slide deflects
// horizontal speed upward off step edges and bumps, and by the next tick that can carry the
// avatar past GROUND_SNAP_HEIGHT; it widens the snap range so the avatar is pulled back down.
var unrequestedLift = 0;
function snapToGround() {
  if (groundDistance < 1e-3 || jumpStartHeight !== undefined) {
    unrequestedLift = 0;
  } else {
    const rise = playerPosition.y - prevPlayerPosition.y;
    const requestedRise = Math.max(0, prevRequestedVelocity.y) * tickTime;
    unrequestedLift = Math.min(MAX_UNREQUESTED_LIFT, Math.max(0, unrequestedLift + rise - requestedRise));
  }

  const launched = isLaunched();

  if (
    jumpStartHeight === undefined // not jumping
    && !launched // not pushed upward
    && !stepping // not stepping
    && (prevGrounded || landedNow) // was grounded last frame, or a jump just landed (within the grounded height)
    && (groundDistance < GROUND_SNAP_HEIGHT + unrequestedLift || probesAllowSnap(GROUND_SNAP_HEIGHT + unrequestedLift)) // close enough
  ) {
    snapSpeed = Math.max(0, groundDistance / stepTime);
    velocity.y -= snapSpeed;
    setGrounded(true); // maintain prevGrounded for next frame
  } else {
    snapSpeed = 0;
    // no longer grounded for the next tick's snap either, or it would undo a small launch
    if (launched) setGrounded(false);
  }
}

var fwdCast: Entity;
var upCast: Entity;
var upFwdCast: Entity;
var landingCast: Entity;

// The landing probe starts above MAX_STEP_HEIGHT so a tread at exactly max
// step height is still hit, and just beyond the collider surface so it lands
// on the prospective tread rather than the riser face.
const LANDING_CAST_HEIGHT = MAX_STEP_HEIGHT + 0.1;
const LANDING_CAST_FORWARD = PLAYER_COLLIDER_RADIUS + 0.05;

// cast fwd
var fwdDistance: number = Infinity;
var fwdWalkable: boolean = true;
var fwdNormal: Vector3 = Vector3.Zero();
// cast up
var upDistance: number = Infinity;
// cast up at +step height
var upFwdDistance: number = Infinity;
// cast down onto the prospective landing
var landingHeight: number = -Infinity;
var landingWalkable: boolean = false;

export function initStepCasts() {
  function initCast(position: Vector3, local: boolean, opts: { direction: Vector3, maxDistance: number, shape: RaycastShape, queryType?: RaycastQueryType }, cb: RaycastSystemCallback): Entity {
    const e = engine.addEntity();
    Transform.create(e, { parent: engine.PlayerEntity, position });

    const raycastData = {
      entity: e,
      opts: {
        queryType: RaycastQueryType.RQT_HIT_FIRST,
        ...opts,
        continuous: true,
        collisionMask: ColliderLayer.CL_PHYSICS,
        includeWorld: true,
      }
    };
    // local directions follow avatar yaw (the cast entity is parented to the
    // player with identity rotation)
    if (local) {
      raycastSystem.registerLocalDirectionRaycast(raycastData, cb);
    } else {
      raycastSystem.registerGlobalDirectionRaycast(raycastData, cb);
    }
    return e;
  }

  fwdCast = initCast({ x: 0, y: 0, z: 0 }, true, { direction: Vector3.Forward(), maxDistance: PLAYER_COLLIDER_RADIUS, shape: RaycastShape.RS_AVATAR }, (hits) => {
    fwdDistance = Infinity;
    fwdWalkable = true;

    for (const hit of hits.hits) {
      fwdDistance = Math.min(fwdDistance, hit.length);
      fwdWalkable = (hit.normalHit?.y ?? 0) > GROUNDED_ANGLE_Y_LEN;
      Vector3.copyFrom(hit.normalHit ?? VEC3_ZERO, fwdNormal);
    }
  })

  // query-all so a wall/corner graze (filtered below) can't mask a real
  // ceiling on another collider behind it
  upCast = initCast({ x: 0, y: 0, z: 0 }, false, { direction: Vector3.Up(), maxDistance: MAX_STEP_HEIGHT, shape: RaycastShape.RS_AVATAR, queryType: RaycastQueryType.RQT_QUERY_ALL }, (hits) => {
    upDistance = Infinity;
    for (const hit of hits.hits) {
      // only count ceiling-like hits (normal facing down): the swept capsule
      // grazing a wall/step-corner beside us reports a ~horizontal normal at
      // ~zero distance, which would collapse the raised fwd probe to feet level
      if ((hit.normalHit?.y ?? 0) < -0.5) {
        upDistance = Math.min(upDistance, hit.length);
      }
    }
  })

  upFwdCast = initCast({ x: 0, y: MAX_STEP_HEIGHT, z: 0 }, true, { direction: Vector3.Forward(), maxDistance: PLAYER_COLLIDER_RADIUS + STEP_CLEARANCE_EXCESS, shape: RaycastShape.RS_AVATAR }, (hits) => {
    upFwdDistance = Infinity;
    for (const hit of hits.hits) {
      upFwdDistance = Math.min(upFwdDistance, hit.length);
    }
  })

  landingCast = initCast({ x: 0, y: LANDING_CAST_HEIGHT, z: LANDING_CAST_FORWARD }, false, { direction: Vector3.Down(), maxDistance: LANDING_CAST_HEIGHT, shape: RaycastShape.RS_RAY }, (hits) => {
    landingHeight = -Infinity;
    landingWalkable = false;
    for (const hit of hits.hits) {
      landingHeight = LANDING_CAST_HEIGHT - hit.length;
      landingWalkable = (hit.normalHit?.y ?? 0) >= GROUNDED_ANGLE_Y_LEN;
    }
  })
}

var stepping = false;
// feet height the current step episode is ascending to, from the landing probe
var stepTargetY = 0;
// climb speed for the current step episode, held until the landing is reached
var stepSpeed = 0;
var prevSteppingY = 0;

function stepUp() {
  // adjust fwdUp to start from ceiling (if any)
  Transform.getMutable(upFwdCast).position.y = Math.min(MAX_STEP_HEIGHT, upDistance);

  if (stepping) {
    if (
      Vector3.length(movementAxis) === 0 // input released
      || playerPosition.y >= stepTargetY - 1e-3 // reached the landing
      || playerPosition.y <= prevSteppingY + 1e-4 // blocked, making no progress
    ) {
      stepping = false;
      velocity.y = Math.min(0, velocity.y);
    } else {
      prevSteppingY = playerPosition.y;
      velocity.y = Math.min(stepSpeed, (stepTargetY - playerPosition.y) / tickTime);
    }
    // a step-up is ground locomotion: hold the grounded state through the
    // episode (and its final frame) so the animation doesn't see an
    // airborne flicker and play a landing bob on every stair step
    setGrounded(true);
    return;
  }

  if (
    !fwdWalkable && // angle doesn't allow normal walk/climb
    fwdDistance < PLAYER_COLLIDER_RADIUS * 0.25 && // can't move fwd
    upFwdDistance > fwdDistance + STEP_CLEARANCE_EXCESS && // can if we move up (excess rules out the recession of a steep planar slope)
    Vector3.dot(fwdNormal, movementAxis) < -0.85 && // ~facing the step
    landingWalkable && // a walkable landing exists ...
    landingHeight > MIN_STEP_HEIGHT && landingHeight <= MAX_STEP_HEIGHT && // ... above current ground, within step height
    (grounded || lastGroundTime > time - JUMP_COYOTE_TIME) // step from (near-)ground only: one step per landing
  ) {
    stepping = true;
    stepTargetY = playerPosition.y + landingHeight;
    prevSteppingY = -Infinity;
    stepSpeed = landingHeight / Math.max(POSITION_CORRECTION_TIME, tickTime);
    velocity.y = stepSpeed;
    setGrounded(true);
  }
}