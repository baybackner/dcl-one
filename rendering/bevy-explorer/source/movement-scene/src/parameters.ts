import { DeepReadonlyObject, PBAvatarLocomotionSettings, PBInputModifier } from "@dcl/sdk/ecs";
import { settings } from "./settings";
import { AIR_JUMP_COUNT, DOUBLE_JUMP_HEIGHT, GLIDE_FALL_SPEED, GLIDE_HORIZONTAL_SPEED, JUMP_HOLD_SCALE, LANDING_STUN_TIME } from "./constants";

// Set once getExplorerConfiguration settles, resolved or failed: it tells how the engine's jump heights read, so no
// ground jump starts before it. Walking does not wait.
var explorerConfigured = false;
// The configuration has no avatarMovementAssist key, or failed: an engine that predates this model, whose own jump and
// run-jump heights are the apex of a held jump rather than a tapped one.
var engineHeightsHeld = false;

export function setExplorerConfiguration(announcesAssist: boolean) {
    explorerConfigured = true;
    engineHeightsHeld = !announcesAssist;
}

// Whether the locomotion settings are an older engine's own: with no parcel scene setting AvatarLocomotionSettings it
// sends every speed and jump height from its player settings, a hardLandingCooldown of 0 and no double-jump or glide
// value. A parcel scene's settings are taken as set.
function olderEngineOwnSettings(locomotion: DeepReadonlyObject<PBAvatarLocomotionSettings>): boolean {
    return locomotion.walkSpeed !== undefined && locomotion.jogSpeed !== undefined && locomotion.runSpeed !== undefined
        && locomotion.jumpHeight !== undefined && locomotion.runJumpHeight !== undefined
        && locomotion.hardLandingCooldown === 0 && locomotion.doubleJumpHeight === undefined
        && locomotion.glidingSpeed === undefined && locomotion.glidingFallingSpeed === undefined;
}

// The factor from an engine jump height to the tapped apex this model jumps to. A held jump rises about 1 /
// JUMP_HOLD_SCALE times as high as a tapped one, so an older engine's held 1.9 m / 2.95 m become 0.95 m / 1.475 m tapped
// and still reach about the same height held.
function engineJumpHeightScale(locomotion: DeepReadonlyObject<PBAvatarLocomotionSettings> | undefined): number {
    return engineHeightsHeld && locomotion !== undefined && olderEngineOwnSettings(locomotion) ? JUMP_HOLD_SCALE : 1;
}

export var sprintSpeed = 0;
export var jogSpeed = 0;
export var walkSpeed = 0;
export var jumpHeight = 0;
export var sprintJumpHeight = 0;
export var doubleJumpHeight = 0;
export var glidingSpeed = 0;
export var glidingFallingSpeed = 0;
export var landingStunTime = LANDING_STUN_TIME; // s without horizontal movement after a stunning landing
export var doubleJumpEnabled = false;
export var glideEnabled = false;
// Slots available between two grounded frames. applyJump() tracks usage in
// separate counters and falls through to a glide once the air slot is spent.
export var maxGroundJumps = 0; // 1 unless disableJump
export var maxAirJumps = 0;    // 1 unless disableDoubleJump
export var disableOrientation = false;

export function initParamters(
    locomotion: DeepReadonlyObject<PBAvatarLocomotionSettings> | undefined,
    modifiers: DeepReadonlyObject<PBInputModifier> | undefined
) {
    // Engine-provided locomotion values win; settings.ts supplies the defaults.
    const baseSprintSpeed = locomotion?.runSpeed ?? settings.sprintSpeed;
    const baseJogSpeed = locomotion?.jogSpeed ?? settings.jogSpeed;
    const baseWalkSpeed = locomotion?.walkSpeed ?? settings.walkSpeed;

    const jumpScale = engineJumpHeightScale(locomotion);
    const baseJumpHeight = (locomotion?.jumpHeight ?? settings.jumpHeight) * jumpScale;
    const baseSprintJumpHeight = (locomotion?.runJumpHeight ?? settings.sprintJumpHeight) * jumpScale;

    // Double-jump height and glide speeds have no entry in settings.ts; the constants
    // are the defaults, overridden by the engine locomotion component.
    doubleJumpHeight = locomotion?.doubleJumpHeight ?? DOUBLE_JUMP_HEIGHT;
    glidingSpeed = locomotion?.glidingSpeed ?? GLIDE_HORIZONTAL_SPEED;
    glidingFallingSpeed = locomotion?.glidingFallingSpeed ?? GLIDE_FALL_SPEED;
    // the scene's hardLandingCooldown as set: 0 or less leaves no stun
    landingStunTime = locomotion?.hardLandingCooldown ?? LANDING_STUN_TIME;

    const jogEnabled = !modifiers?.mode?.standard.disableJog && !modifiers?.mode?.standard.disableAll;
    const walkEnabled = !modifiers?.mode?.standard.disableWalk && !modifiers?.mode?.standard.disableAll;
    const runEnabled = !modifiers?.mode?.standard.disableRun && !modifiers?.mode?.standard.disableAll;
    const anyEnabled = jogEnabled || walkEnabled || runEnabled;

    const jumpEnabled = explorerConfigured && !modifiers?.mode?.standard.disableJump && !modifiers?.mode?.standard.disableAll;
    // Air-jump and glide are independent of the ground-jump gate so that, with
    // disableJump set, the player can still air-jump or glide after walking
    // off a cliff (the air-jump slot is replenished on every grounded frame).
    doubleJumpEnabled = !modifiers?.mode?.standard.disableDoubleJump && !modifiers?.mode?.standard.disableAll;
    glideEnabled = !modifiers?.mode?.standard.disableGliding && !modifiers?.mode?.standard.disableAll;

    jogSpeed = jogEnabled ? baseJogSpeed
        : walkEnabled ? baseWalkSpeed
        : runEnabled ? baseSprintSpeed
        : 0;

    walkSpeed = walkEnabled ? baseWalkSpeed : jogSpeed;
    sprintSpeed = runEnabled ? baseSprintSpeed : jogSpeed;

    jumpHeight = jumpEnabled ? baseJumpHeight : 0;
    sprintJumpHeight = jumpEnabled ? baseSprintJumpHeight : 0;

    maxGroundJumps = jumpEnabled ? 1 : 0;
    // One air jump per airtime; once it is spent a fresh airborne press goes to the glide.
    maxAirJumps = doubleJumpEnabled && doubleJumpHeight > 0 ? AIR_JUMP_COUNT : 0;

    disableOrientation = !anyEnabled;
}