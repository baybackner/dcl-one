//! The teleport's debug section and its `/teleport_state` console command.

use bevy::prelude::*;
use bevy_console::ConsoleCommand;
use common::structs::PrimaryUser;
use console::{AddDebugSection, DebugSection, DoAddConsoleCommand};
use ipfs::RealmInitialLocation;
use scene_runner::OutOfWorld;

use super::LastNamedLanding;

pub(crate) struct TeleportDiagnosticsPlugin;

impl Plugin for TeleportDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.add_console_command::<TeleportState, _>(teleport_state)
            .add_debug_section(DebugSection {
                title: "Teleport",
                query: "/teleport_state",
                fields: &[
                    ("Spawn point the landing under way looks for", "pending"),
                    ("Spawn point the last landing looked for", "last.name"),
                    ("The landing scene had it", "last.found"),
                ],
                actions: &[],
                inputs: &[],
            });
    }
}

/// The spawn point a landing names, for the debug panel's Teleport section.
#[derive(clap::Parser, ConsoleCommand)]
#[command(name = "/teleport_state")]
struct TeleportState;

fn teleport_state(
    mut input: ConsoleCommand<TeleportState>,
    player: Query<(Option<&OutOfWorld>, Option<&LastNamedLanding>), With<PrimaryUser>>,
    realm_target: Res<RealmInitialLocation>,
) {
    if input.take().is_none() {
        return;
    }
    let (out_of_world, last) = player.single().unwrap_or_default();
    // of the landing under way, else of the realm change that has yet to start one
    let pending = out_of_world
        .and_then(|out_of_world| out_of_world.spawn_point.clone())
        .or_else(|| match &*realm_target {
            RealmInitialLocation::Base { spawn_point }
            | RealmInitialLocation::Parcel { spawn_point, .. } => spawn_point.clone(),
            RealmInitialLocation::None => None,
        });
    input.reply_ok(
        serde_json::json!({
            "pending": pending,
            "last": last.map(|last| {
                serde_json::json!({ "name": last.name, "found": last.found })
            }),
            "unavailable": {
                "pending": "No landing under way names a spawn point.",
                "last": "No landing has looked for a named spawn point.",
            },
        })
        .to_string(),
    );
}
