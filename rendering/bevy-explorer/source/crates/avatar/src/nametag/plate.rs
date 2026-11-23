//! The plate a scene puts over an avatar's name with `AvatarNametag`: one line of the scene's
//! words in the scene's colours.
//!
//! Only the scene the local player stands in plates avatars: the local player, the other
//! players standing in it, and the avatars it made. The plates are worked out again every
//! frame from what that scene holds now, so one goes with its component, its entity, its
//! scene, or the player who walks out.

use bevy::{
    platform::collections::{HashMap, HashSet},
    prelude::*,
};
use common::structs::PrimaryUser;
use comms::global_crdt::ForeignPlayer;
use dcl_component::{
    proto_components::{common::Color3, sdk::components::PbAvatarNametag},
    SceneEntityId,
};
use scene_runner::{ContainingScene, SceneEntity};

use super::{bubble::capped, NametagVoice, BACKGROUND};
use crate::{dynamic_nametag::NametagHeadOffset, AvatarSelection, AvatarShape};

/// The most characters of a scene's label a plate shows.
pub const MAX_PLATE_CHARS: usize = 128;
const LABEL_COLOR: Color = Color::srgb_u8(0xFC, 0xFC, 0xFC);

/// On an avatar: the plate over its name.
#[derive(Component, Clone, Debug, PartialEq)]
#[require(NametagVoice)]
pub struct NametagPlate {
    /// One line; empty for a plate with nothing written on it.
    pub label: String,
    pub label_color: Color,
    pub background: Color,
    pub border: Color,
}

/// A scene's colour as a display colour. The scene's numbers are bounded here.
fn display(color: Color3) -> Color {
    let channel = |value: f32| {
        if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    Color::srgb(channel(color.r), channel(color.g), channel(color.b))
}

impl From<&PbAvatarNametag> for NametagPlate {
    fn from(scene: &PbAvatarNametag) -> Self {
        let background = scene.background_color.map_or(BACKGROUND, display);
        Self {
            label: capped(&scene.label, MAX_PLATE_CHARS),
            label_color: scene.label_color.map_or(LABEL_COLOR, display),
            background,
            // no border of its own: the plate reads as one fill
            border: scene.border_color.map_or(background, display),
        }
    }
}

/// On a scene's entity: the plate the scene asks for, for the avatar the entity stands for.
#[derive(Component, Clone, Debug)]
pub struct AvatarNametag(pub NametagPlate);

impl From<PbAvatarNametag> for AvatarNametag {
    fn from(scene: PbAvatarNametag) -> Self {
        Self(NametagPlate::from(&scene))
    }
}

/// A scene entity that asks for a plate, and whether it is an avatar the scene made, with a
/// body that is built.
pub(super) type Plated<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static SceneEntity,
        &'static AvatarNametag,
        Has<AvatarShape>,
        Has<AvatarSelection>,
        Has<NametagHeadOffset>,
    ),
>;

/// Whether a scene's entity with this id stands for a player. Such an entity never plates an
/// avatar of the scene's own, whatever else it carries.
fn is_player_id(id: SceneEntityId) -> bool {
    id.id == SceneEntityId::PLAYER.id || SceneEntityId::FOREIGN_PLAYER_RANGE.contains(&id.id)
}

/// Give every avatar the plate the scene `current` asks for it, and take the plate off every
/// other. `players` are the players standing in that scene, by the id the scene knows them by.
pub(super) fn apply_plates(
    commands: &mut Commands,
    current: Option<Entity>,
    players: &HashMap<SceneEntityId, Entity>,
    plated: &Plated,
    holders: &Query<(Entity, &NametagPlate)>,
    owners: &mut HashSet<Entity>,
) {
    owners.clear();
    for (entity, scene_entity, wanted, shape, selection, built) in plated {
        if Some(scene_entity.root) != current {
            continue;
        }
        let owner = if is_player_id(scene_entity.id) {
            players.get(&scene_entity.id).copied()
        } else {
            (shape && selection && built).then_some(entity)
        };
        let Some(owner) = owner else {
            continue;
        };
        owners.insert(owner);
        if holders
            .get(owner)
            .map_or(true, |(_, plate)| *plate != wanted.0)
        {
            commands.entity(owner).try_insert(wanted.0.clone());
        }
    }
    for (holder, _) in holders {
        if !owners.contains(&holder) {
            commands.entity(holder).try_remove::<NametagPlate>();
        }
    }
}

pub(super) fn resolve_plates(
    mut commands: Commands,
    containing_scene: ContainingScene,
    local: Query<Entity, With<PrimaryUser>>,
    others: Query<(Entity, &ForeignPlayer)>,
    plated: Plated,
    holders: Query<(Entity, &NametagPlate)>,
    mut players: Local<HashMap<SceneEntityId, Entity>>,
    mut owners: Local<HashSet<Entity>>,
) {
    if plated.is_empty() && holders.is_empty() {
        return;
    }
    players.clear();
    // the parcel's scene alone: a portable experience covers every parcel and plates no one
    let current = local.single().ok().and_then(|local| {
        let scene = containing_scene.get_parcel(local)?;
        players.insert(SceneEntityId::PLAYER, local);
        Some(scene)
    });
    if current.is_some() {
        for (player, foreign) in &others {
            if containing_scene.get_parcel(player) == current {
                players.insert(foreign.scene_id, player);
            }
        }
    }
    apply_plates(
        &mut commands,
        current,
        &players,
        &plated,
        &holders,
        &mut owners,
    );
}
