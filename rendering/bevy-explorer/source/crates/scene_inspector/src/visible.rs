//! `/visible_entities`: the scene entities whose meshes fall in the primary camera's view, with the
//! screen rect their bounds project to, largest first. Frustum and bounds only: an entity behind
//! another is still listed.

use bevy::{
    math::Affine3A,
    prelude::*,
    render::primitives::{Aabb, Frustum},
    window::PrimaryWindow,
};
use bevy_console::ConsoleCommand;
use common::structs::{PrimaryCamera, PrimaryUser};
use console::{
    reply::{round, round_fine, Screen},
    DoAddConsoleCommand, PendingConsoleResponses,
};
use dcl_component::{transform_and_parent::DclTranslation, SceneEntityId};
use scene_runner::SceneEntity;
use serde_json::json;

use crate::{active_scene::SceneResolver, labels, snapshot::PendingSnapshotRequests};

const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 200;

pub struct VisibleEntitiesPlugin;

impl Plugin for VisibleEntitiesPlugin {
    fn build(&self, app: &mut App) {
        app.add_console_command::<VisibleEntitiesCommand, _>(visible_entities_cmd);
    }
}

/// List, as JSON, the parcel scene's entities that draw a primitive mesh or a glTF and whose
/// bounds meet the camera's view, sorted by `coverage` (the share of the screen their projected
/// bounds cover), largest first: `crdtEntityId`, `label` (as `/scene_entities --labels`),
/// `screenRect` (logical pixels from the top left, clamped to the view), `center` (normalized),
/// `coverage`, `distance` (from the player), `distanceToCamera`, `renderers`. With `screen`,
/// `cameraPosition`, `candidates` (entities that draw), `inFrustum`, `returned`, `truncated`.
/// Occlusion is not evaluated: an entity behind a wall is listed, and a bounds box is larger than
/// the mesh it wraps; a screenshot shows what is visible.
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/visible_entities")]
struct VisibleEntitiesCommand {
    /// most entities to list, largest coverage first (1 to 200, default 25)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
    limit: Option<u32>,
    /// least share of the screen (0 to 1) an entity must cover to be listed
    #[arg(long, default_value_t = 0.0)]
    min_coverage: f32,
}

/// What one entity's meshes project to.
struct Placed {
    entity: SceneEntityId,
    rect: Option<Rect>,
    centre: Vec3,
    renderers: usize,
}

#[allow(clippy::too_many_arguments)]
fn visible_entities_cmd(
    mut input: ConsoleCommand<VisibleEntitiesCommand>,
    resolver: SceneResolver,
    camera: Query<(&Camera, &GlobalTransform, &Frustum, &Projection), With<PrimaryCamera>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    player: Query<&GlobalTransform, With<PrimaryUser>>,
    meshes: Query<(&Aabb, &GlobalTransform, &InheritedVisibility)>,
    children: Query<&Children>,
    scene_entities: Query<(), With<SceneEntity>>,
    mut pending: ResMut<PendingSnapshotRequests>,
    mut console_responses: ResMut<PendingConsoleResponses>,
) {
    let Some(Ok(cmd)) = input.take() else {
        return;
    };
    if !(0.0..=1.0).contains(&cmd.min_coverage) {
        input.reply_failed(format!(
            "--min-coverage runs from 0 to 1; got {}",
            cmd.min_coverage
        ));
        return;
    }
    let (_, ctx) = match resolver.resolve() {
        Ok(resolved) => resolved,
        Err(e) => {
            input.reply_failed(e);
            return;
        }
    };
    let Ok((camera, camera_transform, frustum, projection)) = camera.single() else {
        input.reply_failed("no primary camera");
        return;
    };
    let Some(screen) = Screen::primary(&windows) else {
        input.reply_failed("no primary window");
        return;
    };
    let Some(viewport) = camera.logical_viewport_rect() else {
        input.reply_failed("the camera has no viewport yet");
        return;
    };
    let near = match projection {
        Projection::Perspective(p) => p.near,
        Projection::Orthographic(p) => p.near,
        _ => 0.1,
    };
    let player = player.single().ok().map(|t| t.translation());

    let mut placed = Vec::new();
    for entity in ctx.live_scene_entities() {
        if entity.id <= SceneEntityId::CAMERA.id {
            continue;
        }
        let Some(target) = ctx.bevy_entity(entity) else {
            continue;
        };
        let boxes = own_meshes(target, &meshes, &children, &scene_entities);
        let Some((min, max)) = union(&boxes) else {
            continue;
        };
        let world = Aabb::from_min_max(min, max);
        if !frustum.intersects_obb(&world, &Affine3A::IDENTITY, true, false) {
            continue;
        }
        let rect = project_box(camera, camera_transform, near, min, max)
            .map(|rect| rect.intersect(viewport))
            .filter(|rect| !rect.is_empty());
        placed.push(Placed {
            entity,
            rect,
            centre: (min + max) * 0.5,
            renderers: boxes.len(),
        });
    }

    let camera_position = camera_transform.translation();
    let limit = cmd
        .limit
        .map_or(DEFAULT_LIMIT, |l| l as usize)
        .min(MAX_LIMIT);
    let min_coverage = cmd.min_coverage;
    let scene = ctx.hash.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    match resolver.request_snapshot(&mut pending, move |crdt| {
        let candidates = ctx_candidates(crdt);
        let viewport_area = viewport.width() * viewport.height();
        let mut listed: Vec<_> = placed
            .into_iter()
            .filter(|p| labels::renders(crdt, p.entity))
            .map(|p| {
                let coverage = p
                    .rect
                    .map_or(0.0, |r| r.width() * r.height() / viewport_area.max(1.0));
                (p, coverage)
            })
            .collect();
        let in_frustum = listed.len();
        listed.retain(|(_, coverage)| *coverage >= min_coverage);
        listed.sort_by(|a, b| b.1.total_cmp(&a.1));
        let truncated = listed.len() > limit;
        listed.truncate(limit);
        let entities: Vec<_> = listed
            .iter()
            .map(|(p, coverage)| {
                json!({
                    "crdtEntityId": p.entity.as_proto_u32().unwrap_or(p.entity.id as u32),
                    "label": labels::label(crdt, p.entity),
                    "screenRect": p.rect.map(|r| json!({
                        "x": round(r.min.x),
                        "y": round(r.min.y),
                        "width": round(r.width()),
                        "height": round(r.height()),
                    })),
                    "center": p.rect.map(|r| {
                        let c = screen.to_normalized(r.center());
                        json!({ "x": round_fine(c.x), "y": round_fine(c.y) })
                    }),
                    "coverage": round_fine(*coverage),
                    "distance": player.map(|player| round(player.distance(p.centre))),
                    "distanceToCamera": round(camera_position.distance(p.centre)),
                    "renderers": p.renderers,
                })
            })
            .collect();
        let camera = DclTranslation::from_bevy_translation(camera_position).0;
        let _ = tx.send(Ok(json!({
            "scene": scene,
            "screen": screen.to_json(),
            "cameraPosition": { "x": round(camera[0]), "y": round(camera[1]), "z": round(camera[2]) },
            "candidates": candidates,
            "inFrustum": in_frustum,
            "returned": entities.len(),
            "truncated": truncated,
            "occlusion": "not evaluated: frustum and bounds only, so an entity behind another is still listed",
            "entities": entities,
        })
        .to_string()));
    }) {
        Ok(()) => console_responses.push_oneshot(rx, |r| r, input.take_responder()),
        Err(e) => input.reply_failed(e),
    }
}

/// The entities of the scene that draw a primitive mesh or a glTF.
fn ctx_candidates(crdt: &dcl_types::interface::CrdtStore) -> usize {
    let mut entities = std::collections::HashSet::new();
    for component in [
        dcl_component::SceneComponentId::MESH_RENDERER,
        dcl_component::SceneComponentId::GLTF_CONTAINER,
    ] {
        if let Some(state) = crdt.lww.get(&component) {
            entities.extend(
                state
                    .last_write
                    .iter()
                    .filter(|(_, entry)| entry.is_some)
                    .map(|(entity, _)| *entity),
            );
        }
    }
    entities.len()
}

/// The visible meshes of `target` and of its descendants that are not themselves scene entities
/// (a child entity is listed on its own), as world boxes.
fn own_meshes(
    target: Entity,
    meshes: &Query<(&Aabb, &GlobalTransform, &InheritedVisibility)>,
    children: &Query<&Children>,
    scene_entities: &Query<(), With<SceneEntity>>,
) -> Vec<(Vec3, Vec3)> {
    let mut boxes = Vec::new();
    let mut stack = vec![target];
    while let Some(entity) = stack.pop() {
        if let Ok((aabb, transform, visible)) = meshes.get(entity) {
            if visible.get() {
                boxes.push(world_box(aabb, &transform.affine()));
            }
        }
        if let Ok(kids) = children.get(entity) {
            stack.extend(kids.iter().filter(|kid| !scene_entities.contains(*kid)));
        }
    }
    boxes
}

fn corners(min: Vec3, max: Vec3) -> [Vec3; 8] {
    std::array::from_fn(|i| {
        Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        )
    })
}

/// A local box placed in the world, as the world box around it.
fn world_box(aabb: &Aabb, affine: &Affine3A) -> (Vec3, Vec3) {
    let min = Vec3::from(aabb.min());
    let max = Vec3::from(aabb.max());
    corners(min, max)
        .into_iter()
        .map(|c| affine.transform_point3(c))
        .fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)))
}

fn union(boxes: &[(Vec3, Vec3)]) -> Option<(Vec3, Vec3)> {
    boxes
        .iter()
        .copied()
        .reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max)))
}

/// The box's corners in view space (bevy looks down -Z), cut at the near plane: the corners in
/// front of it and the points where the box's edges cross it. Empty when the box is wholly behind.
fn clip_to_near(view_corners: [Vec3; 8], near: f32) -> Vec<Vec3> {
    let plane = -near * 1.001;
    let in_front = |v: Vec3| v.z <= plane;
    let mut points: Vec<Vec3> = view_corners.into_iter().filter(|v| in_front(*v)).collect();
    for i in 0..8 {
        for bit in [1, 2, 4] {
            if i & bit != 0 {
                continue;
            }
            let (a, b) = (view_corners[i], view_corners[i | bit]);
            if in_front(a) != in_front(b) {
                let t = (plane - a.z) / (b.z - a.z);
                points.push(a + (b - a) * t);
            }
        }
    }
    points
}

/// The logical-pixel rect around the world box as the camera sees it; `None` when it is wholly
/// behind the camera.
fn project_box(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    near: f32,
    min: Vec3,
    max: Vec3,
) -> Option<Rect> {
    let view_from_world = camera_transform.affine().inverse();
    let view_corners = corners(min, max).map(|c| view_from_world.transform_point3(c));
    clip_to_near(view_corners, near)
        .into_iter()
        .filter_map(|v| {
            camera
                .world_to_viewport(camera_transform, camera_transform.transform_point(v))
                .ok()
        })
        .map(|p| Rect::from_corners(p, p))
        .reduce(|a, b| a.union(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_across_the_near_plane_is_cut_where_its_edges_cross_it() {
        // a unit box from 1 m behind the camera to 1 m in front
        let view = corners(Vec3::new(-0.5, -0.5, -1.0), Vec3::new(0.5, 0.5, 1.0));
        let points = clip_to_near(view, 0.1);
        // four corners in front, four crossings on the edges along z
        assert_eq!(points.len(), 8);
        assert!(points.iter().all(|p| p.z <= -0.1));
        // a box wholly behind has nothing to project
        let behind = corners(Vec3::new(-0.5, -0.5, 1.0), Vec3::new(0.5, 0.5, 2.0));
        assert!(clip_to_near(behind, 0.1).is_empty());
    }

    #[test]
    fn a_turned_box_is_wrapped_by_its_world_box() {
        let aabb = Aabb::from_min_max(Vec3::splat(-1.0), Vec3::splat(1.0));
        let turned = Affine3A::from_rotation_y(std::f32::consts::FRAC_PI_4);
        let (min, max) = world_box(&aabb, &turned);
        assert!((max.x - std::f32::consts::SQRT_2).abs() < 1e-4);
        assert!((min.z + std::f32::consts::SQRT_2).abs() < 1e-4);
        assert_eq!(
            union(&[(Vec3::ZERO, Vec3::ONE), (Vec3::splat(-1.0), Vec3::ZERO)]),
            Some((Vec3::splat(-1.0), Vec3::ONE))
        );
    }
}
