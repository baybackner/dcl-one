//! The hover glow: a hovered avatar's opaque and alpha-tested meshes glow cyan-green through the
//! shared highlight pass (`scene_material::highlight`), fading in and out over a tenth of a second.

use bevy::prelude::*;
use scene_material::{highlight::Highlight, SceneMaterial};

const AVATAR_GLOW_COLOR: Color = Color::srgb(0.0, 1.0, 0.52395105);
/// the glow's opacity once it has faded in
const AVATAR_GLOW_OPACITY: f32 = 0.8;
/// A share of the view's height, as `Highlight::width`: 3.5 pixels at 1080p.
const AVATAR_GLOW_WIDTH: f32 = 3.5 / 1080.0;
const AVATAR_GLOW_FADE_SECONDS: f32 = 0.1;

/// On the player container: `target` 1.0 while hovered, 0.0 after; `alpha` moves toward it at a
/// constant rate, a full fade taking `AVATAR_GLOW_FADE_SECONDS`. Removed when both are 0.
#[derive(Component, Debug, PartialEq, Clone, Copy)]
pub(crate) struct HoverGlow {
    target: f32,
    alpha: f32,
}

fn step_glow(glow: HoverGlow, dt: f32) -> HoverGlow {
    let step = dt / AVATAR_GLOW_FADE_SECONDS;
    let alpha = if glow.alpha < glow.target {
        (glow.alpha + step).min(glow.target)
    } else {
        (glow.alpha - step).max(glow.target)
    };
    HoverGlow { alpha, ..glow }
}

/// The highlight of a glow faded to `alpha`. The highlight pass blends its colour as
/// premultiplied, so the colour is scaled by the opacity it carries.
fn glow_highlight(alpha: f32) -> Highlight {
    let opacity = AVATAR_GLOW_OPACITY * alpha;
    let color = AVATAR_GLOW_COLOR.to_linear();
    Highlight {
        color: LinearRgba::new(
            color.red * opacity,
            color.green * opacity,
            color.blue * opacity,
            opacity,
        ),
        width: AVATAR_GLOW_WIDTH,
    }
}

/// On a mesh of a wearable that opts out of outlines (`outlineCompatible: false`): it never glows.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct NoHoverGlow;

/// Marks whether `mesh` takes part in its avatar's glow. The avatar build calls it for every mesh
/// it dresses, both ways, so a mesh entity kept across a rebuild follows the wearable it now shows.
pub(crate) fn set_hover_glows(mesh: &mut EntityCommands, glows: bool) {
    if glows {
        mesh.try_remove::<NoHoverGlow>();
    } else {
        mesh.try_insert(NoHoverGlow);
    }
}

/// fades `container`'s glow toward `target`, from nothing when a glow starts
pub(crate) fn set_glow_target(
    commands: &mut Commands,
    glows: &mut Query<&mut HoverGlow>,
    container: Entity,
    target: f32,
) {
    match glows.get_mut(container) {
        Ok(mut glow) => glow.target = target,
        Err(_) if target > 0.0 => {
            commands
                .entity(container)
                .try_insert(HoverGlow { target, alpha: 0.0 });
        }
        Err(_) => (),
    }
}

/// The glow follows the opaque draw: opaque and alpha-tested meshes take it, blended ones never.
fn can_glow(alpha_mode: AlphaMode) -> bool {
    matches!(
        alpha_mode,
        AlphaMode::Opaque | AlphaMode::Mask(_) | AlphaMode::AlphaToCoverage
    )
}

/// steps each glow and writes its `Highlight` on the container's glowing meshes, walking them
/// every frame so a rebuilt avatar's new meshes pick it up; a finished glow takes its
/// `Highlight`s and itself away
pub(crate) fn apply_hover_glow(
    mut commands: Commands,
    time: Res<Time>,
    mut glows: Query<(Entity, &mut HoverGlow)>,
    children: Query<&Children>,
    mut meshes: Query<(
        &MeshMaterial3d<SceneMaterial>,
        Has<NoHoverGlow>,
        Option<&mut Highlight>,
    )>,
    materials: Res<Assets<SceneMaterial>>,
) {
    for (container, mut glow) in &mut glows {
        glow.set_if_neq(step_glow(*glow, time.delta_secs()));
        let faded = glow.alpha == 0.0 && glow.target == 0.0;
        if faded {
            commands.entity(container).try_remove::<HoverGlow>();
        }
        let highlight = glow_highlight(glow.alpha);
        for entity in children.iter_descendants(container) {
            let Ok((material, opted_out, current)) = meshes.get_mut(entity) else {
                continue;
            };
            let lit = !faded
                && !opted_out
                && materials
                    .get(material)
                    .is_some_and(|material| can_glow(material.base.alpha_mode));
            match (lit, current) {
                (true, Some(mut current)) => {
                    current.set_if_neq(highlight);
                }
                (true, None) => {
                    commands.entity(entity).try_insert(highlight);
                }
                (false, Some(_)) => {
                    commands.entity(entity).try_remove::<Highlight>();
                }
                (false, None) => (),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::{asset::AssetPlugin, time::TimeUpdateStrategy};
    use scene_material::SceneMaterialExt;

    use super::*;

    #[test]
    fn glow_fades_in_and_out_over_a_tenth_of_a_second() {
        let g = step_glow(
            HoverGlow {
                target: 1.0,
                alpha: 0.0,
            },
            0.05,
        );
        assert!((g.alpha - 0.5).abs() < 1e-6);
        let g = step_glow(
            HoverGlow {
                target: 1.0,
                alpha: 0.5,
            },
            1.0,
        );
        assert_eq!(g.alpha, 1.0);
        let g = step_glow(
            HoverGlow {
                target: 0.0,
                alpha: 1.0,
            },
            0.025,
        );
        assert!((g.alpha - 0.75).abs() < 1e-6);
        let g = step_glow(
            HoverGlow {
                target: 0.0,
                alpha: 0.5,
            },
            1.0,
        );
        assert_eq!(g.alpha, 0.0);
    }

    #[test]
    fn the_highlight_carries_the_glow_colour_at_the_faded_opacity() {
        let half = glow_highlight(0.5);
        assert!((half.color.alpha - 0.4).abs() < 1e-6);
        assert_eq!(half.width, AVATAR_GLOW_WIDTH);
        assert!((AVATAR_GLOW_WIDTH * 1080.0 - 3.5).abs() < 1e-6);

        // the highlight pass blends a premultiplied colour
        let full = glow_highlight(1.0);
        let linear = AVATAR_GLOW_COLOR.to_linear();
        assert!((full.color.alpha - AVATAR_GLOW_OPACITY).abs() < 1e-6);
        assert!((full.color.red - linear.red * AVATAR_GLOW_OPACITY).abs() < 1e-6);
        assert!((full.color.green - linear.green * AVATAR_GLOW_OPACITY).abs() < 1e-6);
        assert!((full.color.blue - linear.blue * AVATAR_GLOW_OPACITY).abs() < 1e-6);
        assert!((half.color.green - linear.green * 0.4).abs() < 1e-6);

        // a faded-out glow adds nothing to the frame
        assert_eq!(glow_highlight(0.0).color, LinearRgba::NONE);
    }

    /// the pointer target, as `update_avatar_collider_actions` sees it
    #[derive(Resource, Default)]
    struct Hovered(Option<Entity>);

    /// `update_avatar_collider_actions`' glow handling, without the `ForeignPlayer` and
    /// `PointerTarget` it reads them from
    fn hover(
        mut commands: Commands,
        hovered: Res<Hovered>,
        mut previous: Local<Option<Entity>>,
        mut glows: Query<&mut HoverGlow>,
    ) {
        if *previous == hovered.0 {
            return;
        }
        if let Some(old) = previous.take() {
            set_glow_target(&mut commands, &mut glows, old, 0.0);
        }
        if let Some(new) = hovered.0 {
            set_glow_target(&mut commands, &mut glows, new, 1.0);
        }
        *previous = hovered.0;
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<SceneMaterial>()
            .init_resource::<Hovered>()
            .add_systems(Update, (hover, apply_hover_glow).chain());
        // the first update starts the clock
        app.update();
        app
    }

    fn tick(app: &mut App, seconds: f32) {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            seconds,
        )));
        app.update();
    }

    fn material(app: &mut App, alpha_mode: AlphaMode) -> MeshMaterial3d<SceneMaterial> {
        MeshMaterial3d(app.world_mut().resource_mut::<Assets<SceneMaterial>>().add(
            SceneMaterial::new_unbounded(StandardMaterial {
                alpha_mode,
                ..default()
            }),
        ))
    }

    /// a built avatar under `container`: an opaque body, an alpha-tested and a blended wearable
    fn build_avatar(app: &mut App, container: Entity) -> (Entity, [Entity; 3]) {
        let opaque = material(app, AlphaMode::Opaque);
        let mask = material(app, AlphaMode::Mask(0.5));
        let blend = material(app, AlphaMode::Blend);
        let world = app.world_mut();
        let avatar = world.spawn(ChildOf(container)).id();
        let meshes =
            [opaque, mask, blend].map(|material| world.spawn((ChildOf(avatar), material)).id());
        (avatar, meshes)
    }

    fn player(app: &mut App) -> (Entity, Entity, [Entity; 3]) {
        let container = app.world_mut().spawn_empty().id();
        let (avatar, meshes) = build_avatar(app, container);
        (container, avatar, meshes)
    }

    fn glow(app: &App, entity: Entity) -> Option<HoverGlow> {
        app.world().get::<HoverGlow>(entity).copied()
    }

    fn highlight(app: &App, entity: Entity) -> Option<Highlight> {
        app.world().get::<Highlight>(entity).copied()
    }

    fn highlights(app: &mut App) -> usize {
        app.world_mut()
            .query::<&Highlight>()
            .iter(app.world())
            .count()
    }

    /// as the avatar build marks a mesh
    fn mark(app: &mut App, mesh: Entity, glows: bool) {
        let mut commands = app.world_mut().commands();
        set_hover_glows(&mut commands.entity(mesh), glows);
        app.world_mut().flush();
    }

    #[test]
    fn hovering_another_avatar_fades_the_first_out_and_the_second_in() {
        let mut app = app();
        let (a, _, a_meshes) = player(&mut app);
        let (b, _, b_meshes) = player(&mut app);

        app.world_mut().resource_mut::<Hovered>().0 = Some(a);
        tick(&mut app, 0.05);
        let a_glow = glow(&app, a).unwrap();
        assert!((a_glow.alpha - 0.5).abs() < 1e-6);
        assert_eq!(
            highlight(&app, a_meshes[0]),
            Some(glow_highlight(a_glow.alpha))
        );

        app.world_mut().resource_mut::<Hovered>().0 = Some(b);
        tick(&mut app, 0.001);
        let a_glow = glow(&app, a).unwrap();
        let b_glow = glow(&app, b).unwrap();
        assert_eq!(a_glow.target, 0.0);
        assert!((a_glow.alpha - 0.49).abs() < 1e-6, "{a_glow:?}");
        assert_eq!(b_glow.target, 1.0);
        assert!((b_glow.alpha - 0.01).abs() < 1e-6, "{b_glow:?}");
        assert_eq!(
            highlight(&app, a_meshes[0]),
            Some(glow_highlight(a_glow.alpha))
        );
        assert_eq!(
            highlight(&app, b_meshes[0]),
            Some(glow_highlight(b_glow.alpha))
        );

        // A fades out completely and lets go of its meshes; B stays lit
        tick(&mut app, 0.1);
        assert_eq!(glow(&app, a), None);
        assert!(a_meshes.iter().all(|mesh| highlight(&app, *mesh).is_none()));
        assert_eq!(glow(&app, b).unwrap().alpha, 1.0);
        assert_eq!(highlight(&app, b_meshes[0]), Some(glow_highlight(1.0)));
    }

    #[test]
    fn despawning_a_glowing_avatar_leaves_no_highlight() {
        let mut app = app();
        let (a, _, _) = player(&mut app);

        app.world_mut().resource_mut::<Hovered>().0 = Some(a);
        tick(&mut app, 0.05);
        assert_eq!(highlights(&mut app), 2);

        app.world_mut().resource_mut::<Hovered>().0 = None;
        tick(&mut app, 0.01);
        assert_eq!(highlights(&mut app), 2);
        app.world_mut().entity_mut(a).despawn();
        tick(&mut app, 0.01);
        assert_eq!(highlights(&mut app), 0);
    }

    #[test]
    fn a_rebuilt_avatar_glows_at_the_current_alpha_next_tick() {
        let mut app = app();
        let (b, avatar, _) = player(&mut app);

        app.world_mut().resource_mut::<Hovered>().0 = Some(b);
        tick(&mut app, 0.03);

        // as a rebuild does: the old avatar goes, a new one is built in its place
        app.world_mut().entity_mut(avatar).despawn();
        let (_, meshes) = build_avatar(&mut app, b);
        assert!(meshes.iter().all(|mesh| highlight(&app, *mesh).is_none()));

        tick(&mut app, 0.03);
        let alpha = glow(&app, b).unwrap().alpha;
        assert!((alpha - 0.6).abs() < 1e-6, "{alpha}");
        assert_eq!(highlight(&app, meshes[0]), Some(glow_highlight(alpha)));
        assert_eq!(highlight(&app, meshes[1]), Some(glow_highlight(alpha)));
    }

    #[test]
    fn a_wearable_that_is_not_outline_compatible_never_glows() {
        let mut app = app();
        let (a, _, [opaque, mask, _]) = player(&mut app);
        mark(&mut app, opaque, true);
        mark(&mut app, mask, false);

        app.world_mut().resource_mut::<Hovered>().0 = Some(a);
        for _ in 0..4 {
            tick(&mut app, 0.05);
            assert!(highlight(&app, opaque).is_some());
            assert_eq!(highlight(&app, mask), None);
        }
    }

    /// a mesh entity kept across a rebuild follows the wearable it now shows, both ways, and
    /// mid-glow
    #[test]
    fn a_reused_mesh_follows_its_current_wearable() {
        let mut app = app();
        let (a, _, [opaque, mask, _]) = player(&mut app);
        mark(&mut app, mask, false);

        app.world_mut().resource_mut::<Hovered>().0 = Some(a);
        tick(&mut app, 0.2);
        assert_eq!(highlight(&app, opaque), Some(glow_highlight(1.0)));
        assert_eq!(highlight(&app, mask), None);

        mark(&mut app, opaque, false);
        mark(&mut app, mask, true);
        tick(&mut app, 0.01);
        assert_eq!(highlight(&app, opaque), None);
        assert_eq!(highlight(&app, mask), Some(glow_highlight(1.0)));
    }

    #[test]
    fn a_blended_wearable_never_glows() {
        let mut app = app();
        let (a, _, [opaque, mask, blend]) = player(&mut app);

        app.world_mut().resource_mut::<Hovered>().0 = Some(a);
        for _ in 0..4 {
            tick(&mut app, 0.05);
            assert!(highlight(&app, opaque).is_some());
            assert!(highlight(&app, mask).is_some());
            assert_eq!(highlight(&app, blend), None);
        }
    }
}
