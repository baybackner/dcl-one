use std::f32::consts::TAU;

use bevy::{
    diagnostic::FrameCount,
    math::DVec3,
    platform::collections::{HashMap, HashSet},
    prelude::*,
};
use common::{
    dynamics::{
        GROUND_SNAP_DROP, MAX_GROUND_INCLINE, MAX_STEP_HEIGHT, PLAYER_COLLIDER_OVERLAP,
        PLAYER_COLLIDER_RADIUS, PLAYER_GROUND_THRESHOLD,
    },
    sets::{PostUpdateSets, SceneLoopSets, SceneSets},
    structs::{
        avatar_tilt_quat, AppConfig, AvatarDynamicState, EngineMovementControl, PrimaryPlayerRes,
        PrimaryUser, SceneDrivenAnim, SceneDrivenAnimationFeedback, SceneDrivenAnimationRequest,
    },
};
use dcl_component::{
    proto_components::{
        common::Vector3,
        sdk::components::{
            AvatarAnimationState, ColliderLayer, MovementAnimation, PbAvatarLocomotionSettings,
            PbAvatarMovement, PbAvatarMovementInfo, PbPhysicsCombinedForce,
            PbPhysicsCombinedImpulse,
        },
    },
    DclReader, DclWriter, SceneComponentId, SceneEntityId,
};
use dcl_types::interface::{ComponentPosition, CrdtType};
use ipfs::{
    ipfs_path::{IpfsPath, IpfsType},
    IpfsAssetServer,
};

use scene_runner::{
    initialize_scene::PARCEL_SIZE,
    renderer_context::RendererSceneContext,
    update_world::{
        avatar_modifier_area::InputModifier,
        mesh_collider::{
            ColliderId, PreviousColliderTransform, SceneColliderData, GROUND_COLLISION_MASK,
        },
        AddCrdtInterfaceExt,
    },
    ContainingScene, SceneEntity, SceneLoopSchedule, SceneUpdates,
};

pub struct AvatarMovementPlugin;

impl Plugin for AvatarMovementPlugin {
    fn build(&self, app: &mut App) {
        app.add_crdt_lww_component::<PbAvatarMovement, AvatarMovement>(
            SceneComponentId::AVATAR_MOVEMENT,
            ComponentPosition::EntityOnly,
        );
        app.add_crdt_lww_component::<PbAvatarLocomotionSettings, AvatarLocomotionSettings>(
            SceneComponentId::AVATAR_LOCOMOTION_SETTINGS,
            ComponentPosition::EntityOnly,
        );
        app.add_crdt_lww_component::<PbPhysicsCombinedImpulse, PhysicsCombinedImpulse>(
            SceneComponentId::PHYSICS_COMBINED_IMPULSE,
            ComponentPosition::EntityOnly,
        );
        app.add_crdt_lww_component::<PbPhysicsCombinedForce, PhysicsCombinedForce>(
            SceneComponentId::PHYSICS_COMBINED_FORCE,
            ComponentPosition::EntityOnly,
        );

        app.init_resource::<AvatarMovementInfo>();
        app.init_resource::<LastMovementStep>();
        app.register_required_components::<RendererSceneContext, MovementPeriod>();
        app.init_resource::<CollisionPush>();
        app.init_resource::<CentralCollisions>();
        app.init_resource::<SceneDrivenAnimationFeedback>();

        app.add_systems(Update, broadcast_movement_info.in_set(SceneSets::Init));
        app.world_mut()
            .resource_mut::<SceneLoopSchedule>()
            .add_systems(stage_redispatch_rotation.after(SceneLoopSets::UpdateWorld));

        app.add_systems(
            Update,
            (
                ActivePlayerComponent::<AvatarMovement>::pick_latest_frame_only_by_priority,
                ActivePlayerComponent::<AvatarLocomotionSettings>::pick_by_priority,
                ActivePlayerComponent::<InputModifier>::pick_by_priority,
                update_priority_scene.after(
                    ActivePlayerComponent::<AvatarMovement>::pick_latest_frame_only_by_priority,
                ),
                update_scene_driven_animation.after(
                    ActivePlayerComponent::<AvatarMovement>::pick_latest_frame_only_by_priority,
                ),
                start_movement_periods,
            )
                .in_set(SceneSets::PostLoop),
        );

        app.add_systems(
            PostUpdate,
            (
                apply_rotation,
                apply_ground_collider_movement,
                resolve_collisions,
                apply_impulses,
                apply_movement,
                record_ground_collider,
                accumulate_movement_periods,
            )
                .chain()
                .in_set(PostUpdateSets::PlayerUpdate),
        );
    }
}

fn update_priority_scene(
    player: Query<&ActivePlayerComponent<AvatarMovement>, With<PrimaryUser>>,
    mut updates: ResMut<SceneUpdates>,
) {
    let scene = player
        .single()
        .ok()
        .map(|active| active.scene())
        .filter(|&e| e != Entity::PLACEHOLDER);

    match scene {
        Some(scene) => {
            updates.priority_scenes.insert("movement_controller", scene);
        }
        None => {
            updates.priority_scenes.remove("movement_controller");
        }
    }
}

// Resolves the active scene's `MovementAnimation.src` against the scene content map
// (path -> content hash) and writes a ready-to-play request onto the primary player's
// `SceneDrivenAnim` component for the avatar animation system to consume.
fn update_scene_driven_animation(
    mut commands: Commands,
    player: Query<(Entity, &ActivePlayerComponent<AvatarMovement>), With<PrimaryUser>>,
    scenes: Query<&RendererSceneContext>,
    ipfas: IpfsAssetServer,
    mut logged_failures: Local<HashSet<String>>,
) {
    let Ok((primary, active)) = player.single() else {
        return;
    };
    let request = (|| {
        let anim = active.component.animation.as_ref()?;
        let scene_ent = active.scene();
        if scene_ent == Entity::PLACEHOLDER {
            return None;
        }
        let ctx = scenes.get(scene_ent).ok()?;
        let ipfs_path = IpfsPath::new(IpfsType::new_content_file(
            ctx.hash.to_string(),
            anim.src.to_lowercase(),
        ));
        let ipfs_ctx = ipfas.ipfs().context.blocking_read();
        let Some(content_hash) = ipfs_path.hash(&ipfs_ctx) else {
            if logged_failures.insert(anim.src.clone()) {
                warn!(
                    "scene-driven movement animation path not found in scene content map: {}",
                    anim.src
                );
            }
            return None;
        };

        // Resolve each scene-relative audio path to a content hash against the same
        // scene content map. Drop (and warn once per src) any path that doesn't resolve.
        let sounds = anim
            .sounds
            .iter()
            .filter_map(|sound_src| {
                let sound_path = IpfsPath::new(IpfsType::new_content_file(
                    ctx.hash.to_string(),
                    sound_src.to_lowercase(),
                ));
                match sound_path.hash(&ipfs_ctx) {
                    Some(h) => Some(h),
                    None => {
                        if logged_failures.insert(sound_src.clone()) {
                            warn!(
                                "scene-driven movement sound path not found in scene content map: {sound_src}"
                            );
                        }
                        None
                    }
                }
            })
            .collect();

        // The `-false` suffix is a fixed part of the scene-emote URN format here;
        // loop behavior is carried separately in SceneDrivenAnimationRequest.r#loop.
        let urn = format!(
            "urn:decentraland:off-chain:scene-emote:{}-{}-false",
            ctx.hash, content_hash
        );
        Some(SceneDrivenAnimationRequest {
            src: anim.src.clone(),
            urn,
            scene_hash: ctx.hash.to_string(),
            content_hash,
            r#loop: anim.r#loop,
            speed: anim.speed,
            idle: anim.idle,
            transition_seconds: anim.transition_seconds.unwrap_or(0.2),
            seek: anim.playback_time,
            sounds,
        })
    })();

    commands
        .entity(primary)
        .try_insert(SceneDrivenAnim { active: request });
}

#[derive(Component, Clone, Debug)]
pub struct AvatarMovement {
    pub velocity: Vec3,
    pub orientation: f32,
    pub ground_direction: Vec3,
    /// set for one frame when a walk_target ends: true = reached target, false = failed
    pub walk_success: Option<bool>,
    /// scene-driven movement animation request; if absent, engine falls back to velocity-based selection
    pub animation: Option<MovementAnimation>,
    /// render-only lean (degrees) composed on top of `orientation`; does not affect physics
    /// or the yaw broadcast to other clients. 0 = upright.
    pub tilt_pitch: f32,
    pub tilt_roll: f32,
}

impl Default for AvatarMovement {
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            orientation: 0.0,
            ground_direction: Vec3::NEG_Y,
            walk_success: None,
            animation: None,
            tilt_pitch: 0.0,
            tilt_roll: 0.0,
        }
    }
}

#[derive(Component, Clone, Debug)]
pub struct AvatarLocomotionSettings(pub(crate) PbAvatarLocomotionSettings);

impl From<PbAvatarLocomotionSettings> for AvatarLocomotionSettings {
    fn from(value: PbAvatarLocomotionSettings) -> Self {
        Self(value)
    }
}

impl FromConfig for AvatarLocomotionSettings {
    fn from_config(config: &AppConfig) -> Self {
        Self(PbAvatarLocomotionSettings {
            walk_speed: Some(config.player_settings.walk_speed),
            jog_speed: Some(config.player_settings.jog_speed),
            run_speed: Some(config.player_settings.run_speed),
            jump_height: Some(config.player_settings.jump_height),
            run_jump_height: Some(config.player_settings.run_jump_height),
            // unset: the movement scene keeps its own landing stun; 0 would remove it
            hard_landing_cooldown: None,
            double_jump_height: None,
            gliding_speed: None,
            gliding_falling_speed: None,
        })
    }
}

impl From<PbAvatarMovement> for AvatarMovement {
    fn from(value: PbAvatarMovement) -> Self {
        Self {
            velocity: value.velocity.unwrap_or_default().world_vec_to_vec3(),
            orientation: value.orientation,
            ground_direction: value
                .ground_direction
                .as_ref()
                .map(Vector3::world_vec_to_vec3)
                .map(Vec3::normalize_or_zero)
                .unwrap_or(Vec3::NEG_Y),
            walk_success: value.walk_success,
            animation: value.animation,
            tilt_pitch: value.tilt_pitch.unwrap_or_default(),
            tilt_roll: value.tilt_roll.unwrap_or_default(),
        }
    }
}

// generic wrapper component that tracks the active provider of singleton player components
// e.g. AvatarMovement, by picking from the highest priority scene or falling back to default
// if none is found.
#[derive(Component)]
pub struct ActivePlayerComponent<C: Component> {
    scene: Entity,
    entity: Entity,
    scene_last_update: u32,
    scene_start_tick: u32,
    scene_is_portable: bool,
    /// Engine dispatch time (`RendererSceneContext::last_sent`) of the scene tick
    /// that produced `component`. Used to reject AvatarMovement that was initiated
    /// before a `movePlayerTo`-imposed facing (and so read a stale transform).
    initiated_at: f64,
    pub component: C,
}

impl<C: Component> ActivePlayerComponent<C> {
    pub fn scene(&self) -> Entity {
        self.scene
    }
}

/// What the last completed movement step applied, in SDK world axes. [`AvatarMovementInfo`] holds
/// the same velocities only from the step until the next frame's broadcast to the scenes clears
/// them, which is before any console command or `Update` system can read them.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct LastMovementStep {
    /// The velocity the controlling scene asked for
    pub requested_velocity: Option<Vector3>,
    /// The displacement over the step's delta, after collisions; zero when the step did not move
    pub actual_velocity: Option<Vector3>,
    /// The SDK forces and impulses applied in the step
    pub external_velocity: Option<Vector3>,
}

impl<C: Component + FromConfig> FromConfig for ActivePlayerComponent<C> {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            scene: Entity::PLACEHOLDER,
            entity: Entity::PLACEHOLDER,
            scene_last_update: 0,
            scene_start_tick: 0,
            scene_is_portable: true,
            initiated_at: 0.0,
            component: C::from_config(config),
        }
    }
}

pub trait FromConfig {
    fn from_config(config: &AppConfig) -> Self;
}

impl<T: Default> FromConfig for T {
    fn from_config(_: &AppConfig) -> Self {
        Self::default()
    }
}

#[derive(Component)]
pub struct PhysicsCombinedForce(pub PbPhysicsCombinedForce);

impl From<PbPhysicsCombinedForce> for PhysicsCombinedForce {
    fn from(value: PbPhysicsCombinedForce) -> Self {
        Self(value)
    }
}

#[derive(Component)]
pub struct PhysicsCombinedImpulse(pub PbPhysicsCombinedImpulse);

impl From<PbPhysicsCombinedImpulse> for PhysicsCombinedImpulse {
    fn from(value: PbPhysicsCombinedImpulse) -> Self {
        Self(value)
    }
}

#[derive(Resource, Default)]
pub struct AvatarMovementInfo(pub PbAvatarMovementInfo);

// What was applied to the player since a scene was last sent an update. A scene can miss
// engine frames (its reply lands after the frame's scene-loop deadline), so each scene is
// told about the whole period since it last looked rather than the last frame only.
#[derive(Component, Default)]
pub(crate) struct MovementPeriod {
    time: f64,
    // requested and actual velocity are the latest frame's, not a total: scenes use them to
    // tell whether their own output is what was applied
    requested_velocity: Option<Vector3>,
    actual_velocity: Option<Vector3>,
    external_velocity: Option<Vector3>,
    // how far collision resolution pushed the player, and over how much time. Reported as the
    // mean push velocity, not a total: the engine holds a scene's velocity over the frames the
    // scene misses, so a collider pushing at a steady speed pushes again each frame and a total
    // would report a multiple of that speed
    collision_push: Vec3,
    collision_push_time: f64,
}

// How far this frame's collision resolution pushed the player out of colliders.
#[derive(Resource, Default)]
struct CollisionPush(Vec3);

impl<C: Component + Clone + FromConfig> ActivePlayerComponent<C> {
    // pick from available of any write-time, based on priority
    #[allow(clippy::too_many_arguments)]
    fn pick_by_priority(
        mut commands: Commands,
        q: Query<(Entity, Ref<C>, &SceneEntity)>,
        mut removed_components: RemovedComponents<C>,
        scenes: Query<&RendererSceneContext>,
        containing_scenes: ContainingScene,
        mut player: Query<&mut ActivePlayerComponent<C>, With<PrimaryUser>>,
        player_res: Res<PrimaryPlayerRes>,
        config: Res<AppConfig>,
    ) {
        let containing_scenes = containing_scenes.get(player_res.0);

        let Ok(mut current_choice) = player.single_mut() else {
            commands
                .entity(player_res.0)
                .try_insert(ActivePlayerComponent::<C>::from_config(&config));
            return;
        };

        // clear current choice if we left the scene or the component was removed
        let current_choice_valid = containing_scenes.contains(&current_choice.scene)
            && !removed_components
                .read()
                .any(|e| e == current_choice.entity);

        if !current_choice_valid {
            *current_choice = FromConfig::from_config(&config);
        }

        // find best choice: parcel first, then portables by most-recently spawned
        for (entity, update, scene_ent) in q.iter().filter(|(_, _, scene_ent)| {
            scene_ent.id == SceneEntityId::PLAYER && containing_scenes.contains(&scene_ent.root)
        }) {
            // skip unchanged
            if current_choice.entity == entity && !update.is_changed() {
                continue;
            }

            let Ok(ctx) = scenes.get(scene_ent.root) else {
                continue;
            };

            // prioritise parcel scenes
            if !current_choice.scene_is_portable && ctx.is_portable {
                continue;
            }

            // prioritise newer portables
            if ctx.is_portable && ctx.start_tick < current_choice.scene_start_tick {
                continue;
            }

            *current_choice = ActivePlayerComponent {
                scene: scene_ent.root,
                entity,
                scene_last_update: ctx.last_update_frame,
                scene_start_tick: ctx.start_tick,
                scene_is_portable: ctx.is_portable,
                initiated_at: ctx.reply_sent,
                component: update.clone(),
            };

            debug!("{} chose {}", std::any::type_name::<C>(), ctx.title);
        }
    }

    // pick based on priority, only from scenes that updated the component
    // in the last available scene-tick response
    fn pick_latest_frame_only_by_priority(
        mut commands: Commands,
        q: Query<(Entity, &C, &SceneEntity), Changed<C>>,
        scenes: Query<&RendererSceneContext>,
        containing_scenes: ContainingScene,
        mut player: Query<&mut ActivePlayerComponent<C>, With<PrimaryUser>>,
        player_res: Res<PrimaryPlayerRes>,
        config: Res<AppConfig>,
    ) {
        let containing_scenes = containing_scenes.get(player_res.0);

        let Ok(mut current_choice) = player.single_mut() else {
            commands
                .entity(player_res.0)
                .try_insert(ActivePlayerComponent::<C>::from_config(&config));
            return;
        };

        // clear current choice if we left the scene or it has updated
        let current_choice_valid = containing_scenes.contains(&current_choice.scene)
            && scenes
                .get(current_choice.scene)
                .is_ok_and(|ctx| ctx.last_update_frame == current_choice.scene_last_update);

        if !current_choice_valid {
            *current_choice = FromConfig::from_config(&config);
        }

        // find best choice: parcel first, then portables by most-recently spawned
        for (entity, update, scene_ent) in q.iter().filter(|(_, _, scene_ent)| {
            scene_ent.id == SceneEntityId::PLAYER && containing_scenes.contains(&scene_ent.root)
        }) {
            // prioritise parcel scenes
            if !current_choice.scene_is_portable {
                continue;
            }

            let Ok(ctx) = scenes.get(scene_ent.root) else {
                continue;
            };

            // prioritise newer portables
            if ctx.is_portable && ctx.start_tick <= current_choice.scene_start_tick {
                continue;
            }

            *current_choice = ActivePlayerComponent {
                scene: scene_ent.root,
                entity,
                scene_last_update: ctx.last_update_frame,
                scene_start_tick: ctx.start_tick,
                scene_is_portable: ctx.is_portable,
                initiated_at: ctx.reply_sent,
                component: update.clone(),
            };

            debug!("{} chose {}", std::any::type_name::<C>(), ctx.title);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply_impulses(
    impulses: Query<(&PhysicsCombinedImpulse, &SceneEntity), Changed<PhysicsCombinedImpulse>>,
    forces: Query<(Ref<PhysicsCombinedForce>, &SceneEntity)>,
    player: Res<PrimaryPlayerRes>,
    containing_scenes: ContainingScene,
    mut info: ResMut<AvatarMovementInfo>,
    time: Res<Time>,
) {
    let containing_scenes = containing_scenes.get(player.0);

    for (impulse, entity) in impulses {
        if !containing_scenes.contains(&entity.root) {
            continue;
        }

        info.0.external_velocity = Some(
            info.0.external_velocity.unwrap_or_default() + impulse.0.vector.unwrap_or_default(),
        );
    }

    for (force, entity) in forces {
        if !containing_scenes.contains(&entity.root) {
            continue;
        }

        info.0.external_velocity = Some(
            info.0.external_velocity.unwrap_or_default()
                + force.0.vector.unwrap_or_default() * time.delta_secs(),
        );
    }
}

// Whether scene-driven movement should be suppressed this frame: either an explicit
// suppression is held (e.g. movePlayerTo interpolation), OR the active AvatarMovement
// was initiated before the most recent movePlayerTo-imposed facing — such a tick read a
// pre-teleport transform, so applying its orientation would clobber the imposed facing.
// Strict `>` (here as `<=` to suppress) treats a same-frame dispatch as stale, since
// `last_sent` is captured before the restricted-action runs and shares the frame's timestamp.
fn movement_suppressed(movement_control: &EngineMovementControl, initiated_at: f64) -> bool {
    !movement_control.suppress_avatar_physics.is_empty()
        || initiated_at <= movement_control.accept_movement_after
}

impl AvatarMovement {
    /// The player's rotation this movement sets. Yaw is authoritative (broadcast to other
    /// clients, used by physics); tilt is a render-only lean composed on top.
    /// `to_euler(YXZ).0` still recovers the yaw.
    fn rotation(&self) -> Quat {
        Quat::from_rotation_y(self.orientation / 360.0 * TAU)
            * avatar_tilt_quat(self.tilt_pitch, self.tilt_roll)
    }
}

// Movement pipeline step 1 (see the PBAvatarMovement proto comment): set the avatar
// orientation from the scene-driven movement info. Kept separate from velocity
// application (`apply_movement`) and ordered ahead of `apply_ground_collider_movement`
// so a rotating ground platform's rotation delta composes onto this orientation rather
// than being overwritten by it.
pub fn apply_rotation(
    mut player: Query<(&mut Transform, &ActivePlayerComponent<AvatarMovement>), With<PrimaryUser>>,
    movement_control: Res<EngineMovementControl>,
) {
    let Ok((mut transform, movement)) = player.single_mut() else {
        return;
    };

    if !movement_suppressed(&movement_control, movement.initiated_at) {
        transform.rotation = movement.component.rotation();
    }
}

/// After each pass of the scene loop, sets the movement scene's `redispatch_rotation`: the
/// rotation [`apply_rotation`] will set from its late reply that landed in this frame, for its
/// re-dispatch, which the next pass sends before that. Runs after the pass's `AvatarMovement`
/// updates are applied.
fn stage_redispatch_rotation(
    player: Query<&ActivePlayerComponent<AvatarMovement>, With<PrimaryUser>>,
    mut scenes: Query<&mut RendererSceneContext>,
    movements: Query<&AvatarMovement>,
    movement_control: Res<EngineMovementControl>,
    frame: Res<FrameCount>,
) {
    let Ok(active) = player.single() else {
        return;
    };
    let Ok(mut context) = scenes.get_mut(active.scene()) else {
        return;
    };
    let applied = context.tick_timing.late_reply_in(frame.0)
        && !movement_suppressed(&movement_control, context.reply_sent);
    context.redispatch_rotation = context
        .bevy_entity(SceneEntityId::PLAYER)
        .and_then(|entity| movements.get(entity).ok())
        .filter(|_| applied)
        .map(AvatarMovement::rotation);
}

#[allow(clippy::too_many_arguments)]
pub fn apply_movement(
    mut player: Query<
        (
            &mut Transform,
            &mut AvatarDynamicState,
            &ActivePlayerComponent<AvatarMovement>,
        ),
        With<PrimaryUser>,
    >,
    mut scenes: Query<(Entity, &mut SceneColliderData)>,
    time_res: Res<Time>,
    mut info: ResMut<AvatarMovementInfo>,
    mut last_step: ResMut<LastMovementStep>,
    mut jumping: Local<bool>,
    mut step_push: Local<f64>,
    movement_control: Res<EngineMovementControl>,
    central: Res<CentralCollisions>,
) {
    let Ok((mut transform, mut dynamic_state, movement)) = player.single_mut() else {
        return;
    };

    info.0.step_time = time_res.delta_secs();

    // Orientation was already set this frame by `apply_rotation` (pipeline step 1);
    // here we only apply velocity. The same suppression gate still governs velocity.
    let suppress = movement_suppressed(&movement_control, movement.initiated_at);
    // standing still: nothing moves, unless the avatar hovers over the ground it stands on and
    // the ground snap below settles it
    let settle = dynamic_state.ground_height > SETTLE_HEIGHT
        && dynamic_state.ground_height <= PLAYER_GROUND_THRESHOLD;

    if suppress || (movement.component.velocity == Vec3::ZERO && !settle) {
        dynamic_state.velocity = Vec3::ZERO;
        dynamic_state.ground_height = ground_height(&mut scenes, transform.translation);
        *last_step = LastMovementStep {
            requested_velocity: Some(Vector3::world_vec_from_vec3(&movement.component.velocity)),
            actual_velocity: Some(Vector3::world_vec_from_vec3(&Vec3::ZERO)),
            external_velocity: info.0.external_velocity,
        };
        return;
    };

    let disabled = &central.0;

    if !disabled.is_empty() {
        warn!("move disabling {} colliders", disabled.len());
    }

    let start = transform.translation.as_dvec3();
    let mut position = start;
    let mut time = time_res.delta_secs_f64();
    let mut velocity = movement.component.velocity.as_dvec3();
    let mut steps = 0;
    // the steepest walkable slope the move was turned along (ground following), for the snap
    let mut followed = 0.0_f64;
    // a slow move held at a riser this step (step offset)
    let mut held_at_riser = false;
    // on the ground at the end of the last step: the assists below hold for a walking avatar
    let grounded = dynamic_state.ground_height <= PLAYER_GROUND_THRESHOLD;

    // deliberately penetrating (placed here by movePlayerTo): don't move deeper into
    // whatever we're inside, but otherwise let the scene walk us out
    if movement_control.deliberate_penetration {
        let (eject_min, eject_max) = movement_control.penetration_bounds;
        let floor = eject_min.cmpgt(DVec3::ZERO);
        let cap = eject_max.cmplt(DVec3::ZERO);
        velocity = DVec3::select(floor, velocity.max(DVec3::ZERO), velocity);
        velocity = DVec3::select(cap, velocity.min(DVec3::ZERO), velocity);
    }

    // a scene standing still sweeps nothing, but the ground snap below still settles it
    while velocity != DVec3::ZERO && steps < 60 && time > 1e-10 {
        steps += 1;
        let mut step_time = time;
        let mut contact_normal = DVec3::ZERO;
        if movement_control.suppress_clipping.is_empty() {
            if let Some(hit) = cast_avatar(
                &mut scenes,
                disabled,
                position,
                velocity,
                step_time,
                movement_control.deliberate_penetration,
            ) {
                (step_time, contact_normal) = hit;
            }
        }

        position += velocity * step_time + contact_normal * PLAYER_COLLIDER_OVERLAP as f64;
        time -= step_time;
        let horizontal = velocity.with_y(0.0);
        if grounded
            && contact_normal != DVec3::ZERO
            && !walkable(contact_normal)
            && horizontal != DVec3::ZERO
            && velocity.y <= horizontal.length() * f64::from(MAX_GROUND_INCLINE.tan())
            && movement_control.suppress_clipping.is_empty()
        {
            // step offset: a riser up to MAX_STEP_HEIGHT is stepped onto, keeping the move. A move
            // slower than step_reach() a frame first pushes against it for STEP_PUSH.
            let speed = horizontal.length();
            if speed * time_res.delta_secs_f64() < step_reach()
                && speed * time + *step_push < STEP_PUSH
            {
                *step_push += speed * time;
                held_at_riser = true;
            } else if let Some((stepped, used)) =
                step_up(&mut scenes, disabled, position, horizontal, time)
            {
                position = stepped;
                time -= used;
                *step_push = 0.0;
                continue;
            }
        }
        if grounded && contact_normal != DVec3::ZERO && !walkable(contact_normal) {
            // slope limit: on the ground, a surface too steep to stand on stops the move into it
            // as a wall does, so the avatar does not walk up it
            let across = contact_normal.with_y(0.0).normalize_or_zero();
            velocity -= velocity.dot(across).min(0.0) * across;
        } else if grounded && contact_normal != DVec3::ZERO && velocity.dot(contact_normal) < 0.0 {
            // ground following: on ground it can stand on, the move keeps its horizontal speed,
            // turned along the ground, instead of losing the part into it
            velocity.y -= velocity.dot(contact_normal) / contact_normal.y;
            followed = followed.max(contact_normal.with_y(0.0).length() / contact_normal.y);
            continue;
        }
        velocity = velocity - (velocity.dot(contact_normal).min(0.0) * contact_normal);
    }

    if !held_at_riser {
        *step_push = 0.0;
    }

    // ground snap: a walking step that asked for no rise follows the ground down when it falls
    // away by at most GROUND_SNAP_DROP beyond the slope of that ground or of the ground it
    // followed, over the distance moved
    if grounded
        && movement.component.velocity.y <= 0.0
        && !movement_control.deliberate_penetration
        && movement_control.suppress_clipping.is_empty()
    {
        let moved = (position - start).with_y(0.0).length();
        let reach = f64::from(GROUND_SNAP_DROP) + moved * f64::from(MAX_GROUND_INCLINE.tan());
        if let Some((drop, normal)) = cast_down(&mut scenes, disabled, position, reach) {
            let slope = normal.with_y(0.0).length() / normal.y;
            if walkable(normal)
                && drop > f64::from(PLAYER_COLLIDER_OVERLAP)
                && drop <= f64::from(GROUND_SNAP_DROP) + moved * slope.max(followed)
            {
                position.y -= drop - f64::from(PLAYER_COLLIDER_OVERLAP);
            }
        }
    }

    debug!(
        "move {:.7} + {:.7} = {:.7} ({steps} iterations)",
        transform.translation, movement.component.velocity, position
    );

    info.0.requested_velocity = Some(Vector3::world_vec_from_vec3(&movement.component.velocity));
    info.0.actual_velocity = Some(Vector3::world_vec_from_vec3(
        &((position - transform.translation.as_dvec3()) / time_res.delta_secs_f64()).as_vec3(),
    ));
    *last_step = LastMovementStep {
        requested_velocity: info.0.requested_velocity,
        actual_velocity: info.0.actual_velocity,
        external_velocity: info.0.external_velocity,
    };

    let position = position.as_vec3();
    let velocity = velocity.as_vec3();

    transform.translation = position.with_y(position.y.max(0.0));

    // for now we hack in the old dynamic state values for animations
    dynamic_state.velocity = velocity;
    if movement.component.velocity.y > 10.0 {
        if !*jumping {
            dynamic_state.jump_time = time_res.elapsed_secs_f64();
            *jumping = true;
        }
    } else {
        *jumping = false;
    }
    dynamic_state.ground_height = ground_height(&mut scenes, transform.translation);
}

/// The avatar capsule's nearest hit moving from `position` along `motion` for `span` (time when
/// `motion` is a velocity, distance when it is a unit direction), over every scene's colliders:
/// the span to the hit and the collider's normal there.
fn cast_avatar(
    scenes: &mut Query<(Entity, &mut SceneColliderData)>,
    disabled: &HashMap<Entity, HashSet<ColliderId>>,
    position: DVec3,
    motion: DVec3,
    span: f64,
    skip_inside: bool,
) -> Option<(f64, DVec3)> {
    let mut nearest = None;
    let mut span = span;
    for (e, mut collider_data) in scenes.iter_mut() {
        if let Some(hit) = collider_data.cast_avatar_nearest(
            position,
            motion,
            span,
            ColliderLayer::ClPhysics as u32 | GROUND_COLLISION_MASK,
            skip_inside,
            false,
            disabled
                .get(&e)
                .map(|d| d.iter().collect())
                .unwrap_or_default(),
            -PLAYER_COLLIDER_OVERLAP,
        ) {
            span = hit.toi as f64;
            nearest = Some((span, hit.normal.as_dvec3()));
        }
    }
    nearest
}

/// [`cast_avatar`] straight down from `position` for `reach`, from just off any wall the capsule
/// touches. The position is stored as f32, so an avatar resting against a wall can sit inside it
/// by a rounding error larger than the sweep's overlap (micrometres a few hundred metres from the
/// origin), and the cast would stop on the wall at once.
fn cast_down(
    scenes: &mut Query<(Entity, &mut SceneColliderData)>,
    disabled: &HashMap<Entity, HashSet<ColliderId>>,
    position: DVec3,
    reach: f64,
) -> Option<(f64, DVec3)> {
    let overlap = f64::from(PLAYER_COLLIDER_OVERLAP);
    // the f32 rounding of the largest coordinate, and the overlap beyond it
    let off = position.abs().max_element() * f64::from(f32::EPSILON) + overlap;
    let mut from = position;
    let mut hit = None;
    // off a wall, or the two walls of a corner
    for _ in 0..3 {
        hit = cast_avatar(scenes, disabled, from, DVec3::NEG_Y, reach, false);
        match hit {
            Some((drop, normal)) if drop <= overlap && !walkable(normal) => {
                from += normal.with_y(0.0).normalize_or_zero() * off;
            }
            _ => break,
        }
    }
    hit
}

/// Height over the ground below which an avatar standing still is not settled onto it.
const SETTLE_HEIGHT: f32 = 1e-3;

/// Forward distance a step offset covers at least: from touching a riser's face to resting on its
/// corner within [`MAX_GROUND_INCLINE`], plus a margin. The step takes the avatar that far at once.
fn step_reach() -> f64 {
    f64::from(PLAYER_COLLIDER_RADIUS) * (1.0 - f64::from(MAX_GROUND_INCLINE.sin())) + 0.01
}
/// How far a move slower than [`step_reach`] a frame pushes against a riser before it steps onto
/// it: a walk pauses at each riser about as long as the reference client's.
const STEP_PUSH: f64 = 0.15;

/// Step offset from a riser too steep to stand on: up by at most [`MAX_STEP_HEIGHT`] (less under a
/// ceiling), on along the horizontal move for the rest of the step's `time` and at least
/// [`step_reach`], down onto the ground. The new position and the time it used; None unless it
/// lands higher, on ground it can stand on, with the riser's top no higher than the step.
fn step_up(
    scenes: &mut Query<(Entity, &mut SceneColliderData)>,
    disabled: &HashMap<Entity, HashSet<ColliderId>>,
    position: DVec3,
    horizontal: DVec3,
    time: f64,
) -> Option<(DVec3, f64)> {
    const MIN_RISE: f64 = 1e-3;
    let overlap = f64::from(PLAYER_COLLIDER_OVERLAP);
    let height = f64::from(MAX_STEP_HEIGHT);
    let radius = f64::from(PLAYER_COLLIDER_RADIUS);
    let lift = cast_avatar(scenes, disabled, position, DVec3::Y, height, false)
        .map_or(height, |(toi, _)| (toi - overlap).max(0.0));
    if lift <= MIN_RISE {
        return None;
    }
    let raised = position + DVec3::Y * lift;
    let reach = time.max(step_reach() / horizontal.length());
    let (ahead, wall) = cast_avatar(scenes, disabled, raised, horizontal, reach, false)
        .unwrap_or((reach, DVec3::ZERO));
    if horizontal.length() * ahead < 1e-4 {
        return None;
    }
    let moved = raised + horizontal * ahead + wall * overlap;
    let (drop, ground) = cast_avatar(scenes, disabled, moved, DVec3::NEG_Y, lift, false)?;
    let landed = moved - DVec3::Y * (drop - overlap);
    // where the capsule's bottom sphere touches the riser's top
    let top = landed.y + radius * (1.0 - ground.y);
    (walkable(ground) && landed.y - position.y > MIN_RISE && top - position.y <= height + MIN_RISE)
        .then_some((landed, ahead.min(time)))
}

/// A contact normal of a surface the avatar can stand on ([`MAX_GROUND_INCLINE`]).
fn walkable(normal: DVec3) -> bool {
    normal.y >= f64::from(MAX_GROUND_INCLINE.cos())
}

/// The feet's height above ground the player stands on, over every scene's colliders; at most the
/// height above the world floor.
fn ground_height(scenes: &mut Query<(Entity, &mut SceneColliderData)>, feet: Vec3) -> f32 {
    scenes.iter_mut().fold(feet.y, |ground, (_, mut collider_data)| {
        ground.min(collider_data.get_ground(feet).map_or(f32::INFINITY, |(h, _)| h))
    })
}

// (scene entity, collider id) of collider player is standing on
#[derive(Component, Default)]
pub struct GroundCollider(pub Option<(Entity, ColliderId, GlobalTransform)>);

fn record_ground_collider(
    mut player: Query<(
        Entity,
        &Transform,
        &ActivePlayerComponent<AvatarMovement>,
        &mut GroundCollider,
    )>,
    containing_scenes: ContainingScene,
    mut scenes: Query<&mut SceneColliderData>,
) {
    let Ok((player_ent, transform, movement, mut ground)) = player.single_mut() else {
        return;
    };

    ground.0 = None;

    if movement.component.ground_direction == Vec3::ZERO {
        return;
    }

    let mut best_height = PLAYER_GROUND_THRESHOLD;

    for scene in containing_scenes.get_area(player_ent, PLAYER_COLLIDER_RADIUS) {
        let Ok(mut collider_data) = scenes.get_mut(scene) else {
            continue;
        };

        if let Some((height, collider_id)) = collider_data.get_ground(transform.translation) {
            if height < best_height {
                if let Some(entity) = collider_data.get_collider_entity(&collider_id) {
                    best_height = height;
                    ground.0 = Some((entity, collider_id.clone(), Default::default()));
                }
            }
        }
    }
}

fn apply_ground_collider_movement(
    ground_transforms: Query<(&GlobalTransform, &PreviousColliderTransform)>,
    mut player: Query<(&mut Transform, &GroundCollider), With<PrimaryUser>>,
    frame: Res<FrameCount>,
    // mut info: ResMut<AvatarMovementInfo>,
    // time: Res<Time>,
    movement_control: Res<EngineMovementControl>,
) {
    if !movement_control.suppress_avatar_physics.is_empty() {
        return;
    }

    let Ok((mut transform, GroundCollider(Some((ground_entity, _, _))))) = player.single_mut()
    else {
        return;
    };

    let Ok((
        new_global_transform,
        PreviousColliderTransform {
            prev_transform,
            updated,
        },
    )) = ground_transforms.get(*ground_entity)
    else {
        return;
    };

    if *updated == frame.0 {
        // update rotation
        let rotation_change = new_global_transform.to_scale_rotation_translation().1
            * prev_transform.to_scale_rotation_translation().1.inverse();
        // clamp to x/z plane to avoid twisting around
        let new_facing =
            ((rotation_change * Vec3::from(transform.forward())) * (Vec3::X + Vec3::Z)).normalize();
        transform.look_to(new_facing, Vec3::Y);

        // calculate updated translation
        let player_global_transform = GlobalTransform::from(*transform);
        let relative_position = player_global_transform.reparented_to(prev_transform);
        let new_transform = new_global_transform.mul_transform(relative_position);
        let new_translation = new_transform.translation();

        debug!(
            "ground collider {} + ? = {}",
            transform.translation, new_translation
        );

        if (new_translation - transform.translation).length() < 5.0 {
            // don't add ground collider movement to external_velocity, else we bounce/slide off everything
            transform.translation = new_translation;
        } else {
            debug!("skipped");
        }
    }
}

// below this required correction a deliberate penetration is considered resolved
const DELIBERATE_PENETRATION_CLEAR: f64 = 1e-3;
// consecutive resolved frames before the flag clears: a gltf reload (e.g. a scene
// changing a chair's collision mask when the player sits) drops its colliders for a
// frame before the replacements are registered
const DELIBERATE_PENETRATION_CLEAR_FRAMES: u32 = 2;

// (scene entity -> collider ids) intersecting the avatar's central segment, at the
// post-depenetration position. Computed once in `resolve_collisions`; `apply_movement`
// excludes them from the sweep since they can't be resolved by pushing out.
#[derive(Resource, Default)]
pub struct CentralCollisions(HashMap<Entity, HashSet<ColliderId>>);

fn resolve_collisions(
    mut player: Query<(&mut Transform, &ActivePlayerComponent<AvatarMovement>), With<PrimaryUser>>,
    mut scenes: Query<(Entity, &mut SceneColliderData)>,
    mut push: ResMut<CollisionPush>,
    time: Res<Time>,
    mut movement_control: ResMut<EngineMovementControl>,
    mut central: ResMut<CentralCollisions>,
    mut resolved_frames: Local<u32>,
) {
    if !movement_control.suppress_clipping.is_empty()
        || !movement_control.suppress_avatar_physics.is_empty()
    {
        return;
    }

    let Ok((mut transform, movement)) = player.single_mut() else {
        return;
    };

    let mut constraint_min = DVec3::NEG_INFINITY;
    let mut constraint_max = DVec3::INFINITY;

    let mut prev = DVec3::INFINITY;
    let mut current_offset = DVec3::ZERO;
    let mut iteration = 0;
    while (prev - current_offset).length() > PLAYER_COLLIDER_OVERLAP as f64 * 0.01 && iteration < 60
    {
        prev = current_offset;

        for (_, mut collider_data) in scenes.iter_mut() {
            // Note: collisions that intersect the avatar central segment are automatically excluded here
            let (scene_min, scene_max) =
                collider_data.avatar_constraints(transform.translation.as_dvec3() + current_offset);

            constraint_min = constraint_min.max(scene_min + current_offset);
            constraint_max = constraint_max.min(scene_max + current_offset);
        }

        // vertical: satisfy floor over ceiling
        current_offset.y = current_offset.y.min(constraint_max.y).max(constraint_min.y);

        // x/z: average if squashed
        if constraint_min.x > constraint_max.x {
            current_offset.x = (constraint_min.x + constraint_max.x) * 0.5;
        } else {
            current_offset.x = current_offset.x.clamp(constraint_min.x, constraint_max.x);
        }

        if constraint_min.z > constraint_max.z {
            current_offset.z = (constraint_min.z + constraint_max.z) * 0.5;
        } else {
            current_offset.z = current_offset.z.clamp(constraint_min.z, constraint_max.z);
        }

        iteration += 1;
    }

    if (constraint_min, constraint_max) != (DVec3::NEG_INFINITY, DVec3::INFINITY) {
        debug!(
            "constraining {:.7} to ({:.7}, {:.7}) -> {:.7} ({iteration} iterations)",
            transform.translation,
            constraint_min,
            constraint_max,
            transform.translation + current_offset.as_vec3()
        );
    }

    let resolved = current_offset.length() < DELIBERATE_PENETRATION_CLEAR;

    if movement_control.deliberate_penetration {
        movement_control.penetration_bounds = (constraint_min, constraint_max);
        // vertical only, and only once the scene starts moving us: a floor sink
        // self-heals on the first step, a chair seat pops us up when we stand
        let v = movement.component.velocity;
        let moving = v.x != 0.0 || v.z != 0.0 || v.y > 0.0;
        if moving {
            transform.translation.y += current_offset.y as f32;
        }
        debug!(
            "deliberate penetration: offset {current_offset:.4} moving {moving} bounds ({constraint_min:.4}, {constraint_max:.4})"
        );
    } else {
        let current_offset = current_offset.as_vec3();

        if current_offset != Vec3::ZERO {
            push.0 = current_offset;
            debug!(
                "depenetration external velocity {:.4}",
                current_offset / time.delta_secs()
            );

            transform.translation += current_offset;
        }
    }

    central.0 = scenes
        .iter_mut()
        .flat_map(|(scene, mut collider_data)| {
            let results = collider_data.avatar_central_collisions(transform.translation.as_dvec3());
            if results.is_empty() {
                None
            } else {
                Some((scene, results))
            }
        })
        .collect();

    // the solve ignores colliders that intersect the central segment, so a deeply
    // embedded collider looks resolved until we start walking out of it
    if movement_control.deliberate_penetration && resolved && central.0.is_empty() {
        *resolved_frames += 1;
        if *resolved_frames >= DELIBERATE_PENETRATION_CLEAR_FRAMES {
            debug!("deliberate penetration cleared");
            movement_control.deliberate_penetration = false;
        }
    } else {
        *resolved_frames = 0;
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn broadcast_movement_info(
    mut info: ResMut<AvatarMovementInfo>,
    active_components: Query<
        (
            Option<&ActivePlayerComponent<AvatarLocomotionSettings>>,
            Option<&ActivePlayerComponent<InputModifier>>,
        ),
        With<PrimaryUser>,
    >,
    feedback: Res<SceneDrivenAnimationFeedback>,
    mut scenes: Query<(&mut RendererSceneContext, &MovementPeriod)>,
    time: Res<Time>,
    mut buf: Local<Vec<u8>>,
) {
    let (maybe_locomotion, maybe_modifier) = active_components.single().unwrap_or_default();

    info.0.active_avatar_locomotion_settings = maybe_locomotion.map(|l| l.component.0.clone());
    info.0.active_input_modifier = maybe_modifier.and_then(|l| l.component.0.clone());
    info.0.active_animation_state = feedback.state.as_ref().map(|s| AvatarAnimationState {
        src: s.src.clone(),
        r#loop: s.r#loop,
        speed: s.speed,
        idle: s.idle,
        playback_time: s.playback_time,
        duration: s.duration,
        loop_count: s.loop_count,
    });

    // the velocity a scene writes this tick is applied over this frame's delta
    info.0.step_time = time.delta_secs();

    debug!("broadcast {:?}", info.0);

    for (mut context, period) in scenes.iter_mut() {
        let mut scene_info = info.0.clone();
        scene_info.previous_step_time = period.time as f32;
        scene_info.requested_velocity = period.requested_velocity;
        scene_info.actual_velocity = period.actual_velocity;
        scene_info.external_velocity = if period.collision_push_time > 0.0 {
            let push = period.collision_push / period.collision_push_time as f32;
            Some(period.external_velocity.unwrap_or_default() + Vector3::world_vec_from_vec3(&push))
        } else {
            period.external_velocity
        };
        // walk_target is a world-space position, scenes read it relative to their origin
        if let Some(target) = scene_info.walk_target.as_mut() {
            target.x -= context.base.x as f32 * PARCEL_SIZE;
            target.z -= context.base.y as f32 * PARCEL_SIZE;
        }

        buf.clear();
        DclWriter::new(&mut buf).write(&scene_info);
        context.crdt_store.force_update(
            SceneComponentId::AVATAR_MOVEMENT_INFO,
            CrdtType::LWW_ANY,
            SceneEntityId::PLAYER,
            Some(&mut DclReader::new(&buf)),
        );
    }

    info.0 = PbAvatarMovementInfo {
        step_time: time.delta_secs(),
        previous_step_time: 0.0,
        requested_velocity: None,
        actual_velocity: None,
        external_velocity: None,
        active_avatar_locomotion_settings: None,
        active_input_modifier: None,
        walk_target: None,
        walk_threshold: None,
        active_animation_state: None,
    }
}

// A scene that was sent an update this frame has seen everything applied so far, so its
// period starts again.
fn start_movement_periods(
    mut scenes: Query<(&RendererSceneContext, &mut MovementPeriod)>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs_f64();
    for (context, mut period) in scenes.iter_mut() {
        if context.last_sent == now {
            *period = default();
        }
    }
}

fn accumulate_movement_periods(
    mut periods: Query<&mut MovementPeriod>,
    info: Res<AvatarMovementInfo>,
    mut push: ResMut<CollisionPush>,
    time: Res<Time>,
) {
    let dt = time.delta_secs_f64();
    let push = std::mem::take(&mut push.0);
    for mut period in periods.iter_mut() {
        period.time += dt;
        period.requested_velocity = info.0.requested_velocity;
        period.actual_velocity = info.0.actual_velocity;
        if let Some(external) = info.0.external_velocity.as_ref() {
            period.external_velocity =
                Some(period.external_velocity.unwrap_or_default() + *external);
        }
        if push != Vec3::ZERO {
            period.collision_push += push;
            period.collision_push_time += dt;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::{TimePlugin, TimeUpdateStrategy};
    use common::structs::EngineMovementControl;
    use scene_runner::update_world::mesh_collider::ColliderId;

    /// `apply_movement` over the floor (top at y 0) and the given boxes (centre, half extents,
    /// rotation), 50 steps of 20 ms at a constant scene velocity from `start`.
    fn walk(boxes: &[(Vec3, Vec3, Quat)], start: Vec3, velocity: Vec3) -> Vec3 {
        walk_phases(boxes, start, &[(velocity, 50)])
    }

    /// [`walk`] with the scene velocity changing: each phase's velocity for its number of steps.
    fn walk_phases(boxes: &[(Vec3, Vec3, Quat)], start: Vec3, phases: &[(Vec3, usize)]) -> Vec3 {
        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_millis(20),
            ))
            .init_resource::<AvatarMovementInfo>()
            .init_resource::<LastMovementStep>()
            .init_resource::<CentralCollisions>()
            .insert_resource(EngineMovementControl {
                accept_movement_after: -1.0,
                ..default()
            })
            .add_systems(PostUpdate, apply_movement);
        let mut colliders = SceneColliderData::default();
        let floor = (
            Vec3::new(start.x, -0.5, start.z),
            Vec3::new(50.0, 0.5, 50.0),
            Quat::IDENTITY,
        );
        for (index, (centre, half, rotation)) in [floor].iter().chain(boxes).enumerate() {
            let id = ColliderId::new(SceneEntityId::ROOT, None, index as u32);
            colliders.set_wall(&id, *centre, *half, *rotation);
        }
        app.world_mut().spawn(colliders);
        let movement = ActivePlayerComponent::<AvatarMovement>::from_config(&default());
        let player = app
            .world_mut()
            .spawn((
                PrimaryUser::default(),
                movement,
                // over the floor, as the last step left it
                AvatarDynamicState {
                    ground_height: start.y,
                    ..default()
                },
                Transform::from_translation(start),
            ))
            .id();
        for &(velocity, steps) in phases {
            app.world_mut()
                .get_mut::<ActivePlayerComponent<AvatarMovement>>(player)
                .unwrap()
                .component
                .velocity = velocity;
            for _ in 0..steps {
                app.update();
            }
        }
        app.world().get::<Transform>(player).unwrap().translation
    }

    /// A ramp rising along +x at `degrees`, its surface from x 1 at y 0.
    fn ramp(degrees: f32) -> (Vec3, Vec3, Quat) {
        let a = degrees.to_radians();
        let (half_length, half_thickness) = (5.0, 0.5);
        let centre = Vec3::new(
            1.0 + half_length * a.cos() + half_thickness * a.sin(),
            half_length * a.sin() - half_thickness * a.cos(),
            0.0,
        );
        (
            centre,
            Vec3::new(half_length, half_thickness, 5.0),
            Quat::from_rotation_z(a),
        )
    }

    #[test]
    fn walking_into_a_slope_too_steep_to_stand_on_stops_at_it() {
        let end = walk(&[ramp(60.0)], Vec3::ZERO, Vec3::new(5.0, 0.0, 0.0));
        assert!(end.y < 0.05, "climbed to {end}");
        assert!(end.x < 1.0, "went through to {end}");
    }

    #[test]
    fn walking_into_a_walkable_slope_climbs_it() {
        let end = walk(&[ramp(30.0)], Vec3::ZERO, Vec3::new(5.0, 0.0, 0.0));
        assert!(end.y > 0.5, "stayed at {end}");
    }

    /// A platform with its top at `height`, its east edge at x 1.
    fn platform(height: f32) -> (Vec3, Vec3, Quat) {
        (
            Vec3::new(-4.0, height * 0.5, 0.0),
            Vec3::new(5.0, height * 0.5, 5.0),
            Quat::IDENTITY,
        )
    }

    #[test]
    fn walking_up_a_walkable_slope_keeps_the_horizontal_speed() {
        let end = walk(&[ramp(30.0)], Vec3::ZERO, Vec3::new(5.0, 0.0, 0.0));
        assert!(end.x > 4.85, "slowed to {end}");
        let on_ramp = (end.x - 1.0) * 30f32.to_radians().tan();
        assert!((end.y - on_ramp).abs() < 0.05, "left the ramp at {end}");
    }

    #[test]
    fn walking_off_a_low_step_follows_it_down_and_off_a_high_one_does_not() {
        let start = Vec3::new(0.0, 0.1, 0.0);
        let end = walk(&[platform(0.1)], start, Vec3::new(1.5, 0.0, 0.0));
        assert!(end.y < 0.01, "stayed up at {end}");
        let start = Vec3::new(0.0, 0.3, 0.0);
        let end = walk(&[platform(0.3)], start, Vec3::new(1.5, 0.0, 0.0));
        // it rounds the corner as far as the corner can be stood on, then is left to fall
        assert!(end.y > 0.2, "followed a 0.3 m drop to {end}");
    }

    #[test]
    fn an_avatar_standing_just_over_the_ground_settles_onto_it() {
        let end = walk(&[], Vec3::new(0.0, 0.03, 0.0), Vec3::ZERO);
        assert!(end.y < 1e-3, "hovers at {end}");
    }

    #[test]
    fn an_avatar_landing_against_a_wall_settles_onto_the_ground() {
        // a wall facing -x at x 470: that far from the origin the stored f32 position of an
        // avatar against it rounds to within micrometres of the wall, either side
        let wall = (
            Vec3::new(470.25, 1.5, 0.0),
            Vec3::new(0.25, 1.5, 5.0),
            Quat::IDENTITY,
        );
        // a fall into the wall that ends 0.01-0.05 m over the floor, then the scene stands still
        // (the first update has no elapsed time)
        let fall = Vec3::new(7.74, -6.16, 0.0);
        for height in [0.0106, 0.0289, 0.0497] {
            let start = Vec3::new(469.6, height - fall.y * 0.02, 0.0);
            let phases = [(Vec3::ZERO, 1), (fall, 1), (Vec3::ZERO, 10)];
            let end = walk_phases(&[wall], start, &phases);
            assert!(end.x > 469.69, "{height} m: short of the wall at {end}");
            assert!(end.y < 1e-3, "{height} m: hovers at {end}");
        }
    }

    /// A riser of `height` whose face is at x 1.
    fn riser(height: f32) -> (Vec3, Vec3, Quat) {
        (
            Vec3::new(6.0, height * 0.5, 0.0),
            Vec3::new(5.0, height * 0.5, 5.0),
            Quat::IDENTITY,
        )
    }

    #[test]
    fn walking_into_a_riser_up_to_the_step_height_steps_onto_it() {
        for (height, speed) in [(0.2, 1.5), (0.45, 1.5), (0.3, 8.0)] {
            let end = walk(&[riser(height)], Vec3::ZERO, Vec3::new(speed, 0.0, 0.0));
            assert!(
                (end.y - height).abs() < 0.01,
                "{height} m at {speed} m/s: {end}"
            );
        }
    }

    #[test]
    fn a_riser_higher_than_the_step_height_stops_the_avatar() {
        let end = walk(&[riser(0.5)], Vec3::ZERO, Vec3::new(8.0, 0.0, 0.0));
        assert!(end.y < 0.01 && end.x < 1.0, "climbed to {end}");
    }

    #[test]
    fn a_steep_slope_still_deflects_an_avatar_in_the_air() {
        // a jump into it: not on the ground, the slope turns the move up it
        let end = walk(
            &[ramp(60.0)],
            Vec3::new(0.0, 0.5, 0.0),
            Vec3::new(5.0, 0.0, 0.0),
        );
        assert!(end.y > 1.0, "stopped at {end}");
    }

    #[test]
    fn without_a_scene_setting_the_hard_landing_cooldown_reaches_scenes_unset() {
        let settings = AvatarLocomotionSettings::from_config(&default()).0;
        assert_eq!(settings.hard_landing_cooldown, None);
        assert!(settings.walk_speed.is_some() && settings.jump_height.is_some());
    }

    /// A movement scene turning the player while each reply lands a frame late and the scene is
    /// dispatched again in the frame it lands. A tick with input turns 10 degrees toward 90 from
    /// the yaw it is sent; one without holds that yaw. The first `turning` ticks have input.
    /// With `imposed`, the player starts at that yaw and every reply was dispatched before it was
    /// imposed. Returns, for each frame, the yaw the tick is sent and the player's yaw after the
    /// frame.
    fn late_replies(turning: u32, frames: u32, imposed: Option<f32>) -> Vec<(f32, f32)> {
        use bevy::ecs::system::RunSystemOnce;
        use dcl_types::SceneId;
        use scene_runner::renderer_context::{ReplyTiming, SentTiming};

        let yaw = |rotation: Quat| rotation.to_euler(EulerRot::YXZ).0.to_degrees();
        let mut app = App::new();
        app.insert_resource(EngineMovementControl {
            accept_movement_after: if imposed.is_some() { f64::MAX } else { -1.0 },
            ..default()
        });
        let scene = app.world_mut().spawn_empty().id();
        let scene_player = app.world_mut().spawn(AvatarMovement::default()).id();
        let mut context = RendererSceneContext::new(
            SceneId::DUMMY,
            "hash".to_owned(),
            "storage_root".to_owned(),
            false,
            0,
            "movement".to_owned(),
            IVec2::ZERO,
            HashSet::from_iter([IVec2::ZERO]),
            vec![],
            default(),
            vec![],
            scene,
            0.0,
            false,
            "sdk_version",
            false,
            false,
        );
        context.associate_bevy_entity(SceneEntityId::PLAYER, scene_player);
        app.world_mut().entity_mut(scene).insert(context);
        let player = app
            .world_mut()
            .spawn((
                PrimaryUser::default(),
                Transform::from_rotation(Quat::from_rotation_y(
                    imposed.unwrap_or_default().to_radians(),
                )),
                ActivePlayerComponent {
                    scene,
                    entity: scene_player,
                    scene_last_update: 0,
                    scene_start_tick: 0,
                    scene_is_portable: false,
                    initiated_at: f64::MIN,
                    component: AvatarMovement::default(),
                },
            ))
            .id();

        let now = web_time::Instant::now();
        let mut in_flight = None;
        let mut frames_out = Vec::new();
        for frame in 0..frames {
            app.insert_resource(FrameCount(frame));
            if let Some(orientation) = in_flight.take() {
                // the reply to the previous frame's tick lands and is picked for this frame
                let world = app.world_mut();
                world
                    .get_mut::<AvatarMovement>(scene_player)
                    .unwrap()
                    .orientation = orientation;
                let mut context = world.get_mut::<RendererSceneContext>(scene).unwrap();
                let sent = SentTiming {
                    at: now,
                    frame: frame - 1,
                    loop_start: now,
                    deadline: now,
                    redispatch: frame > 1,
                };
                context.tick_timing.reply = Some(ReplyTiming {
                    sent,
                    at: now,
                    frame,
                });
                context.reply_sent = f64::from(frame - 1);
                let mut active = world
                    .get_mut::<ActivePlayerComponent<AvatarMovement>>(player)
                    .unwrap();
                active.component.orientation = orientation;
                active.initiated_at = f64::from(frame - 1);
            }
            app.world_mut()
                .run_system_once(stage_redispatch_rotation)
                .unwrap();
            let world = app.world();
            let sent = yaw(world
                .get::<RendererSceneContext>(scene)
                .unwrap()
                .player_rotation_to_send(frame, world.get::<Transform>(player).unwrap().rotation));
            in_flight = Some(if frame < turning {
                sent + (90.0 - sent).clamp(-10.0, 10.0)
            } else {
                sent
            });
            app.world_mut().run_system_once(apply_rotation).unwrap();
            let applied = yaw(app.world().get::<Transform>(player).unwrap().rotation);
            frames_out.push((sent, applied));
        }
        frames_out
    }

    #[test]
    fn a_redispatch_turns_on_from_the_late_reply_it_answers() {
        let frames = late_replies(3, 12, None);
        let yaws: Vec<f32> = frames.iter().map(|(_, applied)| *applied).collect();
        assert!(
            yaws.windows(2).all(|pair| pair[1] >= pair[0] - 1e-3),
            "turned back: {yaws:?}"
        );
        // every tick turns on from the one before it: three ticks with input turn 30 degrees
        assert!(
            yaws[4..].iter().all(|yaw| (yaw - 30.0).abs() < 1e-3),
            "settled at {yaws:?}"
        );
    }

    #[test]
    fn a_redispatch_is_sent_an_imposed_facing_its_late_reply_does_not_override() {
        let frames = late_replies(3, 6, Some(45.0));
        assert!(
            frames
                .iter()
                .all(|(sent, applied)| (sent - 45.0).abs() < 1e-3 && (applied - 45.0).abs() < 1e-3),
            "{frames:?}"
        );
    }
}
