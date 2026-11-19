use std::{f32::consts::TAU, ops::DerefMut};

use bevy::{
    diagnostic::FrameCount,
    ecs::{component::Tick, system::SystemChangeTick},
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
        avatar_tilt_quat, AppConfig, AvatarDynamicState, EngineMovementControl, LocomotionSpeeds,
        PlayerModifiers, PrimaryPlayerRes, PrimaryUser, SceneDrivenAnim,
        SceneDrivenAnimationFeedback, SceneDrivenAnimationRequest,
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
    renderer_context::{RendererSceneContext, SceneTickTiming},
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
        app.register_required_components::<PrimaryUser, LocomotionSpeeds>();
        app.register_required_components::<PrimaryUser, RiserPush>();
        app.register_required_components::<PrimaryUser, GroundAssist>();
        app.register_required_components::<RendererSceneContext, MovementPeriod>();
        app.init_resource::<CollisionPush>();
        app.init_resource::<CentralCollisions>();
        app.init_resource::<SceneDrivenAnimationFeedback>();
        app.init_resource::<RedispatchRotationScene>();
        app.init_resource::<MovementPicked>();

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
                update_locomotion_speeds
                    .after(ActivePlayerComponent::<AvatarLocomotionSettings>::pick_by_priority),
                update_priority_scene.after(
                    ActivePlayerComponent::<AvatarMovement>::pick_latest_frame_only_by_priority,
                ),
                record_movement_pick.after(
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

/// The speeds the active locomotion settings give; a speed they leave unset is the player's own
/// (configured speed with area modifiers).
fn locomotion_speeds(
    base: &PrimaryUser,
    settings: Option<&PbAvatarLocomotionSettings>,
) -> LocomotionSpeeds {
    LocomotionSpeeds {
        walk: settings
            .and_then(|s| s.walk_speed)
            .unwrap_or(base.walk_speed),
        jog: settings.and_then(|s| s.jog_speed).unwrap_or(base.jog_speed),
        run: settings.and_then(|s| s.run_speed).unwrap_or(base.run_speed),
    }
}

/// Keeps the player's [`LocomotionSpeeds`] in step with the active locomotion settings.
fn update_locomotion_speeds(
    mut player: Query<(
        &PrimaryUser,
        Option<&PlayerModifiers>,
        Option<&ActivePlayerComponent<AvatarLocomotionSettings>>,
        &mut LocomotionSpeeds,
    )>,
) {
    for (user, modifiers, active, mut speeds) in player.iter_mut() {
        let base = modifiers.map_or_else(|| user.clone(), |m| m.combine(user));
        let settings = active.map(|a| &a.component.0);
        speeds.set_if_neq(locomotion_speeds(&base, settings));
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
#[derive(Component, Clone)]
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
        let Ok(mut current_choice) = player.single_mut() else {
            commands
                .entity(player_res.0)
                .try_insert(ActivePlayerComponent::<C>::from_config(&config));
            return;
        };

        choose_latest_frame(
            &mut current_choice,
            q.iter(),
            &scenes,
            &containing_scenes.get(player_res.0),
            &config,
        );
    }
}

/// The choice [`ActivePlayerComponent::pick_latest_frame_only_by_priority`] makes from
/// `current_choice`, the one it made last, and the components `changed` since then. Writes to
/// `current_choice` only when the choice changes.
fn choose_latest_frame<'a, C: Component + Clone + FromConfig>(
    current_choice: &mut impl DerefMut<Target = ActivePlayerComponent<C>>,
    changed: impl Iterator<Item = (Entity, &'a C, &'a SceneEntity)>,
    scenes: &Query<&RendererSceneContext>,
    containing_scenes: &HashSet<Entity>,
    config: &AppConfig,
) {
    // clear current choice if we left the scene or it has updated
    let current_choice_valid = containing_scenes.contains(&current_choice.scene)
        && scenes
            .get(current_choice.scene)
            .is_ok_and(|ctx| ctx.last_update_frame == current_choice.scene_last_update);

    if !current_choice_valid {
        **current_choice = FromConfig::from_config(config);
    }

    // find best choice: parcel first, then portables by most-recently spawned
    for (entity, update, scene_ent) in changed.filter(|(_, _, scene_ent)| {
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

        **current_choice = ActivePlayerComponent {
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

/// The scene whose [`SceneTickTiming::redispatch_rotation`] is set, if any: only the scene that
/// drove the player when [`stage_redispatch_rotation`] last ran holds one.
#[derive(Resource, Default)]
struct RedispatchRotationScene(Option<Entity>);

/// The change tick of the last [`ActivePlayerComponent::pick_latest_frame_only_by_priority`] for
/// [`AvatarMovement`]: a movement changed after it is one the next pick sees as changed.
#[derive(Resource, Default)]
struct MovementPicked(Tick);

fn record_movement_pick(ticks: SystemChangeTick, mut picked: ResMut<MovementPicked>) {
    picked.0 = ticks.this_run();
}

/// After each pass of the scene loop, sets the movement scene's
/// [`SceneTickTiming::redispatch_rotation`]: the rotation [`apply_rotation`] will set this frame,
/// for the re-dispatch answering the scene's late reply that landed in this frame, which the next
/// pass sends before that. Runs after the pass's `AvatarMovement` updates are applied. The frame's
/// movement is chosen by [`choose_latest_frame`] from the movements changed so far, as the pick
/// after the scene loop chooses it: another scene's movement that takes over in this frame, or
/// none when the reply changed no movement or the player has left the scene. Nothing is staged
/// when the choice is not applied ([`movement_suppressed`]): the re-dispatch is then sent the
/// player's rotation, which [`apply_rotation`] keeps.
/// The scene that staged the last rotation is tracked in [`RedispatchRotationScene`]; when another
/// scene drives the player, or none does, that scene's rotation is cleared (a despawned scene has
/// nothing to clear). The engine applies no movement from it, so a movement scene that no longer
/// drives the player is not sent a rotation it staged while it did.
fn stage_redispatch_rotation(
    player: Query<&ActivePlayerComponent<AvatarMovement>, With<PrimaryUser>>,
    mut tick_timings: Query<&mut SceneTickTiming>,
    scenes: Query<&RendererSceneContext>,
    movements: Query<(Entity, Ref<AvatarMovement>, &SceneEntity)>,
    containing_scenes: ContainingScene,
    player_res: Res<PrimaryPlayerRes>,
    config: Res<AppConfig>,
    picked: Res<MovementPicked>,
    ticks: SystemChangeTick,
    movement_control: Res<EngineMovementControl>,
    frame: Res<FrameCount>,
    mut staged: ResMut<RedispatchRotationScene>,
) {
    let active = player.single().ok();
    let scene = active.map(|active| active.scene());
    if let Some(previous) = staged.0.take_if(|&mut previous| Some(previous) != scene) {
        if let Ok(mut tick_timing) = tick_timings.get_mut(previous) {
            tick_timing.redispatch_rotation = None;
        }
    }
    let Some((active, Ok(mut tick_timing))) =
        active.map(|active| (active, tick_timings.get_mut(active.scene())))
    else {
        return;
    };
    let rotation = if tick_timing.late_reply_in(frame.0) {
        let mut choice = active.clone();
        let changed = movements
            .iter()
            .filter(|(_, movement, _)| {
                movement
                    .last_changed()
                    .is_newer_than(picked.0, ticks.this_run())
            })
            .map(|(entity, movement, scene_entity)| (entity, movement.into_inner(), scene_entity));
        choose_latest_frame(
            &mut &mut choice,
            changed,
            &scenes,
            &containing_scenes.get(player_res.0),
            &config,
        );
        (!movement_suppressed(&movement_control, choice.initiated_at))
            .then(|| choice.component.rotation())
    } else {
        None
    };
    tick_timing.redispatch_rotation = rotation;
    staged.0 = rotation.map(|_| active.scene());
}

/// How far the player's slow move has pushed against a riser it has not stepped onto yet, up to
/// [`STEP_PUSH`]. Kept from frame to frame by [`apply_movement`].
#[derive(Component, Default)]
pub(crate) struct RiserPush(pub(crate) f64);

/// The player's step offsets and ground snaps since startup, each with the height of the last
/// one, for the debug panel. Kept by [`apply_movement`].
#[derive(Component, Default, Clone, Copy, Debug)]
pub(crate) struct GroundAssist {
    pub(crate) steps: u64,
    /// How far the last step offset rose (m).
    pub(crate) last_step_rise: Option<f32>,
    pub(crate) snaps: u64,
    /// How far the last ground snap dropped (m).
    pub(crate) last_snap_drop: Option<f32>,
}

pub(crate) fn apply_movement(
    mut player: Query<
        (
            &mut Transform,
            &mut AvatarDynamicState,
            &ActivePlayerComponent<AvatarMovement>,
            &mut RiserPush,
            &mut GroundAssist,
        ),
        With<PrimaryUser>,
    >,
    mut scenes: Query<(Entity, &mut SceneColliderData)>,
    containing_scenes: ContainingScene,
    time_res: Res<Time>,
    mut info: ResMut<AvatarMovementInfo>,
    mut last_step: ResMut<LastMovementStep>,
    mut jumping: Local<bool>,
    movement_control: Res<EngineMovementControl>,
    central: Res<CentralCollisions>,
) {
    let Ok((mut transform, mut dynamic_state, movement, mut riser_push, mut assist)) =
        player.single_mut()
    else {
        return;
    };
    // the casts below test only the scenes around the capsule: those it can reach this frame. A
    // scene moving the avatar more than a parcel in a frame sweeps the scenes a parcel around it.
    let reach = frame_reach(movement.component.velocity, time_res.delta_secs()).min(PARCEL_SIZE);
    let area =
        containing_scenes.get_area_position(transform.translation, PLAYER_COLLIDER_RADIUS + reach);
    let mut scenes = scenes
        .iter_mut()
        .filter(|(scene, _)| area.contains(scene))
        .collect::<Vec<_>>();
    let step_push = &mut riser_push.0;

    info.0.step_time = time_res.delta_secs();

    // Orientation was already set this frame by `apply_rotation` (pipeline step 1);
    // here we only apply velocity. The same suppression gate still governs velocity.
    let suppress = movement_suppressed(&movement_control, movement.initiated_at);
    // standing still: nothing moves, unless the avatar hovers over the ground it stands on and
    // the ground snap below settles it
    let settle = dynamic_state.ground_height > SETTLE_HEIGHT
        && dynamic_state.ground_height <= PLAYER_GROUND_THRESHOLD;

    if suppress || (movement.component.velocity == Vec3::ZERO && !settle) {
        // the move has stopped, so its push against a riser ends: the next riser takes its own
        *step_push = 0.0;
        dynamic_state.velocity = Vec3::ZERO;
        dynamic_state.ground_height = ground_height(&mut scenes, transform.translation);
        end_jump_on_landing(&mut dynamic_state);
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
    // on the ground and asking for no rise: not the step a jump takes off in
    let walking = grounded && movement.component.velocity.y <= 0.0;

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
        if walking
            && contact_normal != DVec3::ZERO
            && !walkable(contact_normal)
            && horizontal != DVec3::ZERO
            && velocity.y <= horizontal.length() * f64::from(MAX_GROUND_INCLINE.tan())
            && movement_control.suppress_clipping.is_empty()
        {
            // step offset: a riser up to MAX_STEP_HEIGHT is stepped onto, keeping the move. A move
            // slower than step_push_speed() first pushes against it for STEP_PUSH. Not as a jump
            // takes off: that step rises, and stepping would trade its rise for the riser's.
            let speed = horizontal.length();
            if speed < step_push_speed() && speed * time + *step_push < STEP_PUSH {
                *step_push += speed * time;
                held_at_riser = true;
            } else if let Some((stepped, used)) =
                step_up(&mut scenes, disabled, position, horizontal, time)
            {
                assist.steps += 1;
                assist.last_step_rise = Some((stepped.y - position.y) as f32);
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

    // a walking step follows the ground down
    if walking
        && !movement_control.deliberate_penetration
        && movement_control.suppress_clipping.is_empty()
    {
        let snapped = snap_to_ground(&mut scenes, disabled, start, position, followed);
        if position.y - snapped.y > SNAP_REPORTED {
            assist.snaps += 1;
            assist.last_snap_drop = Some((position.y - snapped.y) as f32);
        }
        position = snapped;
    }

    debug!(
        "move {:.7} + {:.7} = {:.7} ({steps} iterations)",
        transform.translation, movement.component.velocity, position
    );

    info.0.requested_velocity = Some(Vector3::world_vec_from_vec3(&movement.component.velocity));
    info.0.actual_velocity = Some(Vector3::world_vec_from_vec3(
        &actual_velocity(
            transform.translation.as_dvec3(),
            position,
            time_res.delta_secs_f64(),
        )
        .as_vec3(),
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
    end_jump_on_landing(&mut dynamic_state);
}

/// The velocity of a step that moved the avatar from `from` to `to` in `delta` seconds. A frame
/// with no elapsed time (the first) reports none, though the ground snap can still move the
/// avatar in it.
fn actual_velocity(from: DVec3, to: DVec3, delta: f64) -> DVec3 {
    if delta > 0.0 {
        (to - from) / delta
    } else {
        DVec3::ZERO
    }
}

/// The avatar capsule's nearest hit moving from `position` along `motion` for `span` (time when
/// `motion` is a velocity, distance when it is a unit direction), over the colliders of the
/// `scenes` around it: the span to the hit and the collider's normal there.
fn cast_avatar(
    scenes: &mut [(Entity, Mut<SceneColliderData>)],
    disabled: &HashMap<Entity, HashSet<ColliderId>>,
    position: DVec3,
    motion: DVec3,
    span: f64,
    skip_inside: bool,
) -> Option<(f64, DVec3)> {
    let mut nearest = None;
    let mut span = span;
    for (e, collider_data) in scenes.iter_mut() {
        if let Some(hit) = collider_data.cast_avatar_nearest(
            position,
            motion,
            span,
            ColliderLayer::ClPhysics as u32 | GROUND_COLLISION_MASK,
            skip_inside,
            false,
            disabled
                .get(&*e)
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
    scenes: &mut [(Entity, Mut<SceneColliderData>)],
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

/// Ground snap: the end `position` of a walking step from `start`, moved down onto the ground
/// when the ground falls away by at most [`GROUND_SNAP_DROP`] beyond the slope of that ground, of
/// the steepest ground the step `followed`, or of the ground it started on (the foot of a slope
/// the step ran down), over the distance moved.
fn snap_to_ground(
    scenes: &mut [(Entity, Mut<SceneColliderData>)],
    disabled: &HashMap<Entity, HashSet<ColliderId>>,
    start: DVec3,
    mut position: DVec3,
    followed: f64,
) -> DVec3 {
    let moved = (position - start).with_y(0.0).length();
    let reach = f64::from(GROUND_SNAP_DROP) + moved * f64::from(MAX_GROUND_INCLINE.tan());
    let within = |drop: f64, slope: f64| drop <= f64::from(GROUND_SNAP_DROP) + moved * slope;
    if let Some((drop, normal)) = cast_down(scenes, disabled, position, reach) {
        if walkable(normal)
            && drop > f64::from(PLAYER_COLLIDER_OVERLAP)
            && (within(drop, slope(normal).max(followed))
                || within(drop, start_slope(scenes, disabled, start)))
        {
            position.y -= drop - f64::from(PLAYER_COLLIDER_OVERLAP);
        }
    }
    position
}

/// Rise over run of ground with this `normal`.
fn slope(normal: DVec3) -> f64 {
    normal.with_y(0.0).length() / normal.y
}

/// The slope of the walkable ground a walking step starts on, within [`PLAYER_GROUND_THRESHOLD`]
/// below `start` (0 for none).
fn start_slope(
    scenes: &mut [(Entity, Mut<SceneColliderData>)],
    disabled: &HashMap<Entity, HashSet<ColliderId>>,
    start: DVec3,
) -> f64 {
    match cast_down(scenes, disabled, start, f64::from(PLAYER_GROUND_THRESHOLD)) {
        Some((_, normal)) if walkable(normal) => slope(normal),
        _ => 0.0,
    }
}

/// Height over the ground below which an avatar standing still is not settled onto it.
const SETTLE_HEIGHT: f32 = 1e-3;

/// A ground snap shorter than this is the sweep's rounding on flat ground, not a drop:
/// [`GroundAssist`] does not count it.
const SNAP_REPORTED: f64 = 1e-4;

/// Forward distance a step offset covers at least: from touching a riser's face to resting on its
/// corner within [`MAX_GROUND_INCLINE`], plus a margin. The step takes the avatar that far at once.
fn step_reach() -> f64 {
    f64::from(PLAYER_COLLIDER_RADIUS) * (1.0 - f64::from(MAX_GROUND_INCLINE.sin())) + 0.01
}
/// How far a move slower than [`step_push_speed`] pushes against a riser before it steps onto it:
/// the default walk (1.5 m/s) pauses about 0.1 s at each riser, so climbing reads as stepping.
const STEP_PUSH: f64 = 0.15;
/// The frame the speed below which a move pushes against a riser is set for.
const STEP_PUSH_FRAME: f64 = 1.0 / 60.0;
/// Horizontal speed below which a move pushes against a riser for [`STEP_PUSH`] before it steps
/// onto it: [`step_reach`] in a 60 Hz frame (about 5.5 m/s), so a walk pushes and a jog or a run
/// steps at once, at any frame rate.
fn step_push_speed() -> f64 {
    step_reach() / STEP_PUSH_FRAME
}

/// How far across the ground [`apply_movement`] can carry the capsule in a frame of `delta`
/// seconds at the scene's `velocity`: the sweep, which ground following lengthens by up to
/// 1 / cos [`MAX_GROUND_INCLINE`] and a slide can turn sideways, and one step offset's
/// [`step_reach`]. A run (10 m/s) at 60 fps reaches about 0.33 m.
fn frame_reach(velocity: Vec3, delta: f32) -> f32 {
    velocity.length() * delta / MAX_GROUND_INCLINE.cos() + step_reach() as f32
}

/// Step offset from a riser too steep to stand on: up by at most [`MAX_STEP_HEIGHT`] (less under a
/// ceiling), on along the horizontal move for the rest of the step's `time` and at least
/// [`step_reach`], down onto the ground. The new position and the time it used; None unless it
/// lands higher, on ground it can stand on, with the riser's top no higher than the step.
fn step_up(
    scenes: &mut [(Entity, Mut<SceneColliderData>)],
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

/// The feet's height above ground the player stands on, over the colliders of the `scenes` around
/// it; at most the height above the world floor.
fn ground_height(scenes: &mut [(Entity, Mut<SceneColliderData>)], feet: Vec3) -> f32 {
    scenes
        .iter_mut()
        .fold(feet.y, |ground, (_, collider_data)| {
            ground.min(
                collider_data
                    .get_ground(feet)
                    .map_or(f32::INFINITY, |(h, _)| h),
            )
        })
}

/// Forget the take-off time once the player is back on the ground and no longer rising, so that
/// `jump_time` marks a jump in flight (`<= 0.0` for none): the animation's take-off window and the
/// `jump_count` this client broadcasts both read it.
fn end_jump_on_landing(dynamic_state: &mut AvatarDynamicState) {
    if dynamic_state.ground_height < 0.2 && dynamic_state.velocity.y <= 0.0 {
        dynamic_state.jump_time = -1.0;
    }
}

// (scene entity, collider id) of collider player is standing on
#[derive(Component, Default)]
pub struct GroundCollider(pub Option<(Entity, ColliderId, GlobalTransform)>);

fn record_ground_collider(
    mut player: Query<(
        &Transform,
        &ActivePlayerComponent<AvatarMovement>,
        &mut GroundCollider,
    )>,
    containing_scenes: ContainingScene,
    mut scenes: Query<&mut SceneColliderData>,
) {
    let Ok((transform, movement, mut ground)) = player.single_mut() else {
        return;
    };

    ground.0 = None;

    if movement.component.ground_direction == Vec3::ZERO {
        return;
    }

    let mut best_height = PLAYER_GROUND_THRESHOLD;

    // around where the move left the player this frame
    let area = containing_scenes.get_area_position(transform.translation, PLAYER_COLLIDER_RADIUS);
    for scene in area {
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
        let (mut app, player) = movement_app(boxes, start);
        for &(velocity, steps) in phases {
            set_velocity(&mut app, player, velocity);
            for _ in 0..steps {
                app.update();
            }
        }
        app.world().get::<Transform>(player).unwrap().translation
    }

    fn set_velocity(app: &mut App, player: Entity, velocity: Vec3) {
        app.world_mut()
            .get_mut::<ActivePlayerComponent<AvatarMovement>>(player)
            .unwrap()
            .component
            .velocity = velocity;
    }

    /// An app running `apply_movement` over the floor (top at y 0) and the given boxes (centre,
    /// half extents, rotation) in steps of 20 ms after the first, which has no elapsed time, and
    /// its player, standing still at `start`.
    fn movement_app(boxes: &[(Vec3, Vec3, Quat)], start: Vec3) -> (App, Entity) {
        movement_app_at(boxes, start, std::time::Duration::from_millis(20))
    }

    /// [`movement_app`] in steps of `frame`.
    fn movement_app_at(
        boxes: &[(Vec3, Vec3, Quat)],
        start: Vec3,
        frame: std::time::Duration,
    ) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(TimeUpdateStrategy::ManualDuration(frame))
            .init_resource::<AvatarMovementInfo>()
            .init_resource::<LastMovementStep>()
            .init_resource::<CentralCollisions>()
            .register_required_components::<PrimaryUser, RiserPush>()
            .register_required_components::<PrimaryUser, GroundAssist>()
            .insert_resource(EngineMovementControl {
                accept_movement_after: -1.0,
                ..default()
            })
            .add_systems(PostUpdate, apply_movement);
        add_scene_resources(&mut app);
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
        let scene = app.world_mut().spawn(colliders).id();
        // the start's parcel and those around it
        let centre = scene_runner::vec3_to_parcel(start);
        let around = (-1..=1).flat_map(|x| (-1..=1).map(move |y| centre + IVec2::new(x, y)));
        place_scene(&mut app, scene, "floor", around);
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
        (app, player)
    }

    /// The resources mapping a position to the scenes around it, with no scene placed yet.
    fn add_scene_resources(app: &mut App) {
        use scene_runner::initialize_scene::{LiveScenes, PortableScenes, ScenePointers};

        app.init_resource::<LiveScenes>()
            .init_resource::<PortableScenes>()
            .init_resource::<ScenePointers>();
    }

    /// Makes `scene` the live scene `hash` on `parcels`.
    fn place_scene(
        app: &mut App,
        scene: Entity,
        hash: &str,
        parcels: impl IntoIterator<Item = IVec2>,
    ) {
        use scene_runner::initialize_scene::{LiveScenes, PointerResult, ScenePointers};

        let world = app.world_mut();
        world
            .resource_mut::<LiveScenes>()
            .scenes
            .insert(hash.to_owned(), scene);
        let mut pointers = world.resource_mut::<ScenePointers>();
        for parcel in parcels {
            pointers.insert(
                parcel,
                PointerResult::Exists {
                    hash: hash.to_owned(),
                    urn: None,
                },
            );
        }
    }

    /// Places a scene `hash` on `parcel` holding a wall: a box at `centre`, `half` its extents.
    fn add_wall_scene(app: &mut App, hash: &str, parcel: IVec2, centre: Vec3, half: Vec3) {
        let mut colliders = SceneColliderData::default();
        let id = ColliderId::new(SceneEntityId::ROOT, None, 0);
        colliders.set_wall(&id, centre, half, Quat::IDENTITY);
        let scene = app.world_mut().spawn(colliders).id();
        place_scene(app, scene, hash, [parcel]);
    }

    #[test]
    fn a_wall_of_the_next_parcels_scene_stops_the_avatar_crossing_into_it() {
        // the middle of parcel 0,0's east edge (x 16), and a wall 0.5 m into parcel 1,0
        let start = Vec3::new(15.5, 0.0, -8.0);
        let (mut app, player) = movement_app(&[], start);
        let wall = (Vec3::new(17.0, 1.0, -8.0), Vec3::new(0.5, 1.0, 8.0));
        add_wall_scene(&mut app, "next", IVec2::new(1, 0), wall.0, wall.1);
        set_velocity(&mut app, player, Vec3::new(5.0, 0.0, 0.0));
        for _ in 0..50 {
            app.update();
        }
        let end = app.world().get::<Transform>(player).unwrap().translation;
        // against the wall's face
        let face = 16.5 - PLAYER_COLLIDER_RADIUS;
        assert!((end.x - face).abs() < 0.01, "stopped at {end}");
    }

    #[test]
    fn the_colliders_of_a_scene_away_from_the_avatar_are_not_cast_against() {
        // a wall across the walk, held by a scene whose parcel is far away
        let start = Vec3::new(8.0, 0.0, -8.0);
        let (mut app, player) = movement_app(&[], start);
        let wall = (Vec3::new(10.5, 1.0, -8.0), Vec3::new(0.5, 1.0, 8.0));
        add_wall_scene(&mut app, "far", IVec2::new(5, 5), wall.0, wall.1);
        set_velocity(&mut app, player, Vec3::new(5.0, 0.0, 0.0));
        for _ in 0..50 {
            app.update();
        }
        let end = app.world().get::<Transform>(player).unwrap().translation;
        assert!(end.x > 11.0, "stopped at {end}");
    }

    #[test]
    fn a_frames_reach_covers_its_move_and_a_step_offset() {
        // a run at 60 fps
        let reach = frame_reach(Vec3::new(10.0, 0.0, 0.0), 1.0 / 60.0);
        assert!(reach > 10.0 / 60.0 + step_reach() as f32, "{reach}");
        assert!((reach - 0.33).abs() < 0.01, "{reach}");
        // standing still: a step offset
        assert_eq!(frame_reach(Vec3::ZERO, 1.0 / 60.0), step_reach() as f32);
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

    /// The highest the avatar's feet hover over the ground in a walk at `speed` m/s down a 45
    /// degree ramp from `x` onto the floor at its foot, in 60 Hz frames.
    fn hover_down_to_the_foot_of_a_ramp(x: f32, speed: f32) -> f32 {
        // standing on the ramp: its surface under the centre, plus the lower sphere's lift there
        let lift = PLAYER_COLLIDER_RADIUS * (std::f32::consts::SQRT_2 - 1.0);
        let start = Vec3::new(x, x - 1.0 + lift, 0.0);
        let frame = std::time::Duration::from_secs_f64(1.0 / 60.0);
        let (mut app, player) = movement_app_at(&[ramp(45.0)], start, frame);
        app.update();
        set_velocity(&mut app, player, Vec3::new(-speed, 0.0, 0.0));
        let mut hover = 0.0_f32;
        while app.world().get::<Transform>(player).unwrap().translation.x > 0.0 {
            app.update();
            let height = app
                .world()
                .get::<AvatarDynamicState>(player)
                .unwrap()
                .ground_height;
            hover = hover.max(height);
        }
        hover
    }

    #[test]
    fn walking_down_a_slope_onto_flat_ground_keeps_to_the_ground_at_the_foot() {
        // a jog and a run down a 45 degree ramp (the scene's 8 and 10 m/s, 1.29 times faster
        // downhill) take more than GROUND_SNAP_DROP of descent in a frame; from start points one
        // frame apart, so that the last frame on the ramp ends anywhere before its foot
        let mut hovered = Vec::new();
        for speed in [10.29, 12.86] {
            for k in 0..22 {
                let x = 4.0 + k as f32 * 0.01;
                let hover = hover_down_to_the_foot_of_a_ramp(x, speed);
                if hover > PLAYER_GROUND_THRESHOLD {
                    hovered.push((speed, x, hover));
                }
            }
        }
        assert!(hovered.is_empty(), "(m/s, from x, hovered m): {hovered:?}");
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
    fn settling_in_the_first_frame_reports_no_velocity() {
        let (mut app, player) = movement_app(&[], Vec3::new(0.0, 0.03, 0.0));
        app.update();
        let end = app.world().get::<Transform>(player).unwrap().translation;
        assert!(end.y < 1e-3, "hovers at {end}");
        let step = *app.world().resource::<LastMovementStep>();
        assert_eq!(
            step.actual_velocity,
            Some(Vector3::world_vec_from_vec3(&Vec3::ZERO))
        );
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
    fn standing_still_ends_the_push_against_a_riser() {
        let (mut app, player) = movement_app(&[riser(0.2)], Vec3::new(0.695, 0.0, 0.0));
        app.update();
        // a slow move held at the riser
        set_velocity(&mut app, player, Vec3::new(0.5, 0.0, 0.0));
        for _ in 0..5 {
            app.update();
        }
        let pushed = app.world().get::<RiserPush>(player).unwrap().0;
        assert!(pushed > 0.0, "not held at the riser");
        set_velocity(&mut app, player, Vec3::ZERO);
        app.update();
        assert_eq!(app.world().get::<RiserPush>(player).unwrap().0, 0.0);
    }

    /// The time a walk at 1.5 m/s from 0.15 m short of a 0.2 m riser takes to step onto it, at
    /// `fps` frames a second.
    fn time_to_step_up(fps: f64) -> f64 {
        let frame = std::time::Duration::from_secs_f64(1.0 / fps);
        let (mut app, player) = movement_app_at(&[riser(0.2)], Vec3::new(0.55, 0.0, 0.0), frame);
        app.update();
        set_velocity(&mut app, player, Vec3::new(1.5, 0.0, 0.0));
        let mut time = 0.0;
        while app.world().get::<Transform>(player).unwrap().translation.y < 0.1 {
            assert!(time < 2.0, "never stepped up at {fps} fps");
            app.update();
            time += frame.as_secs_f64();
        }
        time
    }

    #[test]
    fn a_walk_pushes_against_a_riser_for_the_same_time_at_any_frame_rate() {
        // 0.1 s to reach the riser, then the push
        let at_60 = time_to_step_up(60.0);
        let at_15 = time_to_step_up(15.0);
        assert!(at_60 > 0.18, "no push at 60 fps: {at_60} s");
        assert!(at_15 > 0.18, "no push at 15 fps: {at_15} s");
        assert!(
            (at_60 - at_15).abs() <= 1.0 / 15.0 + 1e-6,
            "{at_60} s at 60 fps, {at_15} s at 15"
        );
    }

    #[test]
    fn the_ground_assist_counts_its_step_offsets_and_snaps() {
        // onto a 0.1 m step, then down its far side
        let step = (
            Vec3::new(2.0, 0.05, 0.0),
            Vec3::new(1.0, 0.05, 5.0),
            Quat::IDENTITY,
        );
        let (mut app, player) = movement_app(&[step], Vec3::ZERO);
        app.update();
        set_velocity(&mut app, player, Vec3::new(1.5, 0.0, 0.0));
        let assist = |app: &App| *app.world().get::<GroundAssist>(player).unwrap();
        for _ in 0..100 {
            if app.world().get::<Transform>(player).unwrap().translation.y > 0.05 {
                break;
            }
            app.update();
        }
        let up = assist(&app);
        assert_eq!(up.steps, 1, "{up:?}");
        // onto the step's corner, which the walk then rounds
        let rise = up.last_step_rise.unwrap();
        assert!(rise > 0.05 && rise < 0.11, "{up:?}");
        // on the flat floor before it, nothing to snap down to
        assert_eq!(up.snaps, 0, "{up:?}");
        for _ in 0..150 {
            app.update();
        }
        let down = assist(&app);
        assert!(down.snaps > up.snaps, "{down:?}");
        assert!(down.last_snap_drop.unwrap() > 0.0, "{down:?}");
    }

    #[test]
    fn a_running_jump_into_a_riser_does_not_step_onto_it_as_it_takes_off() {
        // 0.05 m short of a 0.4 m riser, on the ground
        let (mut app, player) = movement_app(&[riser(0.4)], Vec3::new(0.65, 0.0, 0.0));
        app.update();
        // a run at 10 m/s takes off for a 1 m jump
        set_velocity(&mut app, player, Vec3::new(10.0, 8.94, 0.0));
        app.update();
        let end = app.world().get::<Transform>(player).unwrap().translation;
        // the take-off step rises at 8.94 m/s for its 20 ms, against the riser's face
        assert!((end.y - 8.94 * 0.02).abs() < 0.01, "rose to {end}");
        assert!(end.x < 0.71, "went onto the riser to {end}");
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

    fn yaw(rotation: Quat) -> f32 {
        rotation.to_euler(EulerRot::YXZ).0.to_degrees()
    }

    /// An app that stages the re-dispatch's rotation in `Update`, as after a pass of the scene
    /// loop, and chooses and applies the frame's movement in `PostUpdate`; and its player, in the
    /// middle of parcel 0,0, with no movement chosen yet. Movement initiated at or before
    /// `accept_movement_after` is not applied.
    fn rotation_app(accept_movement_after: f64) -> (App, Entity) {
        use scene_runner::initialize_scene::{LiveScenes, PortableScenes, ScenePointers};

        let mut app = App::new();
        app.insert_resource(EngineMovementControl {
            accept_movement_after,
            ..default()
        })
        .init_resource::<AppConfig>()
        .init_resource::<RedispatchRotationScene>()
        .init_resource::<MovementPicked>()
        .init_resource::<LiveScenes>()
        .init_resource::<PortableScenes>()
        .init_resource::<ScenePointers>()
        .insert_resource(FrameCount(0))
        .add_systems(Update, stage_redispatch_rotation)
        .add_systems(
            PostUpdate,
            (
                ActivePlayerComponent::<AvatarMovement>::pick_latest_frame_only_by_priority,
                record_movement_pick,
                apply_rotation,
            )
                .chain(),
        );
        let player = app
            .world_mut()
            .spawn((
                PrimaryUser::default(),
                ActivePlayerComponent::<AvatarMovement>::from_config(&default()),
                Transform::default(),
                GlobalTransform::from_translation(Vec3::new(8.0, 0.0, -8.0)),
            ))
            .id();
        app.insert_resource(PrimaryPlayerRes(player));
        (app, player)
    }

    /// A scene the player is in, `hash`, and the entity its player maps to, which holds the
    /// scene's `AvatarMovement` once a reply sets one: a portable started at `portable`'s tick, or
    /// the scene of parcel 0,0.
    fn add_scene(app: &mut App, hash: &str, portable: Option<u32>) -> (Entity, Entity) {
        use dcl_types::SceneId;
        use scene_runner::initialize_scene::{
            LiveScenes, PointerResult, PortableScenes, PortableSource, ScenePointers,
        };

        let scene = app.world_mut().spawn_empty().id();
        let scene_player = app
            .world_mut()
            .spawn(SceneEntity {
                root: scene,
                scene_id: SceneId::DUMMY,
                id: SceneEntityId::PLAYER,
            })
            .id();
        let mut context = RendererSceneContext::new(
            SceneId::DUMMY,
            hash.to_owned(),
            "storage_root".to_owned(),
            portable.is_some(),
            portable.unwrap_or_default(),
            hash.to_owned(),
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
        let world = app.world_mut();
        world.entity_mut(scene).insert(context);
        world
            .resource_mut::<LiveScenes>()
            .scenes
            .insert(hash.to_owned(), scene);
        if portable.is_some() {
            world.resource_mut::<PortableScenes>().insert(
                hash.to_owned(),
                PortableSource::new(hash.to_owned(), None, None, false),
            );
        } else {
            world.resource_mut::<ScenePointers>().insert(
                IVec2::ZERO,
                PointerResult::Exists {
                    hash: hash.to_owned(),
                    urn: None,
                },
            );
        }
        (scene, scene_player)
    }

    /// The reply to `scene`'s tick dispatched in `sent` lands in `frame`, setting the scene's
    /// movement to `orientation` when there is one. The engine clock reads 1 s at frame 0 and
    /// advances a second a frame.
    fn land_reply(
        app: &mut App,
        (scene, scene_player): (Entity, Entity),
        sent: u32,
        frame: u32,
        orientation: Option<f32>,
    ) {
        use scene_runner::renderer_context::{ReplyTiming, SentTiming};

        let world = app.world_mut();
        if let Some(orientation) = orientation {
            world.entity_mut(scene_player).insert(AvatarMovement {
                orientation,
                ..default()
            });
        }
        let at = web_time::Instant::now();
        let sent = SentTiming { at, frame: sent };
        world.get_mut::<SceneTickTiming>(scene).unwrap().reply =
            Some(ReplyTiming { sent, at, frame });
        let mut context = world.get_mut::<RendererSceneContext>(scene).unwrap();
        context.reply_sent = f64::from(sent.frame) + 1.0;
        context.last_update_frame = frame;
    }

    /// Runs `frame` once its replies have landed. Returns the yaw a dispatch of `scene` in the
    /// frame is sent, and the player's yaw after the frame's movement is applied.
    fn run_frame(app: &mut App, frame: u32, scene: Entity, player: Entity) -> (f32, f32) {
        app.insert_resource(FrameCount(frame));
        app.world_mut().run_schedule(Update);
        let world = app.world();
        let sent = world
            .get::<SceneTickTiming>(scene)
            .unwrap()
            .player_rotation_to_send(frame, world.get::<Transform>(player).unwrap().rotation);
        app.world_mut().run_schedule(PostUpdate);
        let applied = app.world().get::<Transform>(player).unwrap().rotation;
        (yaw(sent), yaw(applied))
    }

    /// A movement scene turning the player while each reply lands a frame late and the scene is
    /// dispatched again in the frame it lands. A tick with input turns 10 degrees toward 90 from
    /// the yaw it is sent; one without holds that yaw. The first `turning` ticks have input.
    /// With `imposed`, the player starts at that yaw and every reply was dispatched before it was
    /// imposed. Returns, for each frame, the yaw the tick is sent and the player's yaw after the
    /// frame.
    fn late_replies(turning: u32, frames: u32, imposed: Option<f32>) -> Vec<(f32, f32)> {
        let (mut app, player) = rotation_app(if imposed.is_some() { f64::MAX } else { 0.0 });
        let scene = add_scene(&mut app, "movement", Some(1));
        let start = imposed.unwrap_or_default();
        app.world_mut()
            .get_mut::<Transform>(player)
            .unwrap()
            .rotation = Quat::from_rotation_y(start.to_radians());
        // the scene has been driving the player
        app.world_mut().entity_mut(scene.1).insert(AvatarMovement {
            orientation: start,
            ..default()
        });

        let mut in_flight = None;
        let mut frames_out = Vec::new();
        for frame in 0..frames {
            if let Some(orientation) = in_flight.take() {
                land_reply(&mut app, scene, frame - 1, frame, Some(orientation));
            }
            let (sent, applied) = run_frame(&mut app, frame, scene.0, player);
            in_flight = Some(if frame < turning {
                sent + (90.0 - sent).clamp(-10.0, 10.0)
            } else {
                sent
            });
            frames_out.push((sent, applied));
        }
        frames_out
    }

    #[test]
    fn a_scene_no_longer_driving_the_player_is_not_sent_its_staged_rotation() {
        let (mut app, player) = rotation_app(0.0);
        let (movement_scene, _) = add_scene(&mut app, "movement", Some(1));
        // the parcel scene drives the player
        let parcel = add_scene(&mut app, "parcel", None);
        land_reply(&mut app, parcel, 0, 0, Some(0.0));
        run_frame(&mut app, 0, parcel.0, player);
        assert_eq!(
            app.world()
                .get::<ActivePlayerComponent<AvatarMovement>>(player)
                .unwrap()
                .scene(),
            parcel.0
        );
        // the movement scene still holds a rotation it staged while it drove the player
        app.world_mut()
            .get_mut::<SceneTickTiming>(movement_scene)
            .unwrap()
            .redispatch_rotation = Some(Quat::from_rotation_y(1.0));
        app.insert_resource(RedispatchRotationScene(Some(movement_scene)));
        run_frame(&mut app, 1, parcel.0, player);
        let tick_timing = app.world().get::<SceneTickTiming>(movement_scene).unwrap();
        assert_eq!(tick_timing.redispatch_rotation, None);
        assert_eq!(app.world().resource::<RedispatchRotationScene>().0, None);
    }

    #[test]
    fn a_despawned_scene_that_staged_a_rotation_is_forgotten() {
        let (mut app, _) = rotation_app(0.0);
        let (movement_scene, _) = add_scene(&mut app, "movement", Some(1));
        app.insert_resource(RedispatchRotationScene(Some(movement_scene)));
        app.world_mut().despawn(movement_scene);
        app.world_mut().run_schedule(Update);
        assert_eq!(app.world().resource::<RedispatchRotationScene>().0, None);
    }

    /// A parcel scene sets the player's movement in the frame the movement scene's late reply
    /// lands: the parcel scene's takes priority, so the movement scene's re-dispatch is sent the
    /// yaw the parcel scene sets, not the one of its own reply.
    #[test]
    fn a_redispatch_is_sent_the_rotation_of_a_scene_taking_over_in_its_frame() {
        let (mut app, player) = rotation_app(0.0);
        let movement = add_scene(&mut app, "movement", Some(1));
        let parcel = add_scene(&mut app, "parcel", None);
        land_reply(&mut app, movement, 0, 0, Some(10.0));
        run_frame(&mut app, 0, movement.0, player);

        land_reply(&mut app, movement, 0, 1, Some(30.0));
        land_reply(&mut app, parcel, 1, 1, Some(70.0));
        let (sent, applied) = run_frame(&mut app, 1, movement.0, player);
        assert!((applied - 70.0).abs() < 1e-3, "applied {applied}");
        assert!(
            (sent - applied).abs() < 1e-3,
            "sent {sent}, applied {applied}"
        );
    }

    /// A late reply that sets no movement leaves the player's rotation as it is (here turned
    /// from the scene's last movement by a rotating platform): the re-dispatch is sent that
    /// rotation, not the scene's last movement.
    #[test]
    fn a_late_reply_without_movement_redispatches_the_players_rotation() {
        let (mut app, player) = rotation_app(0.0);
        let movement = add_scene(&mut app, "movement", Some(1));
        land_reply(&mut app, movement, 0, 0, Some(10.0));
        run_frame(&mut app, 0, movement.0, player);
        app.world_mut()
            .get_mut::<Transform>(player)
            .unwrap()
            .rotation = Quat::from_rotation_y(40f32.to_radians());

        land_reply(&mut app, movement, 0, 1, None);
        let (sent, applied) = run_frame(&mut app, 1, movement.0, player);
        assert!((applied - 40.0).abs() < 1e-3, "applied {applied}");
        assert!(
            (sent - applied).abs() < 1e-3,
            "sent {sent}, applied {applied}"
        );
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

    #[test]
    fn unset_locomotion_speeds_use_the_players_own() {
        let base = PrimaryUser {
            walk_speed: 3.0,
            jog_speed: 7.0,
            run_speed: 9.0,
            ..Default::default()
        };
        assert_eq!(
            locomotion_speeds(&base, None),
            LocomotionSpeeds {
                walk: 3.0,
                jog: 7.0,
                run: 9.0
            }
        );
        let settings = PbAvatarLocomotionSettings {
            walk_speed: Some(1.0),
            run_speed: Some(20.0),
            ..Default::default()
        };
        assert_eq!(
            locomotion_speeds(&base, Some(&settings)),
            LocomotionSpeeds {
                walk: 1.0,
                jog: 7.0,
                run: 20.0
            }
        );
    }
}

#[cfg(test)]
mod jump_tests {
    use super::*;

    fn state(ground_height: f32, vertical_velocity: f32) -> AvatarDynamicState {
        AvatarDynamicState {
            velocity: Vec3::new(0.0, vertical_velocity, 0.0),
            ground_height,
            jump_time: 5.0,
            ..default()
        }
    }

    #[test]
    fn landing_ends_the_jump_in_flight() {
        let mut landed = state(0.0, 0.0);
        end_jump_on_landing(&mut landed);
        assert!(landed.jump_time <= 0.0);
    }

    #[test]
    fn the_take_off_frame_and_the_flight_keep_the_jump() {
        // still within the grounded height on the frame the jump starts, but rising
        let mut taking_off = state(0.1, 11.0);
        end_jump_on_landing(&mut taking_off);
        assert_eq!(taking_off.jump_time, 5.0);

        let mut airborne = state(1.5, -3.0);
        end_jump_on_landing(&mut airborne);
        assert_eq!(airborne.jump_time, 5.0);
    }
}
