use std::sync::{Arc, PoisonError, RwLock};

use anyhow::anyhow;
use bevy::prelude::*;
use common::structs::AppConfig;

use super::{NamedVariant, Setting, SettingCategory, SettingInfo, Settings};

const TITLE: &str = "Microphone";
const DESCRIPTION: &str = "The microphone voice chat captures from.\n\nSystem Default follows the device's own choice. A chosen microphone that is unplugged falls back to it.";

/// An audio input the host offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneDevice {
    pub id: String,
    pub label: String,
}

/// The audio inputs the host offers, in its order.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneDevices(pub Vec<MicrophoneDevice>);

/// The device ids behind the setting's choices after the first, which is the system default.
#[derive(Resource, Clone, Default)]
struct Choices(Arc<RwLock<Vec<String>>>);

fn choice(ids: &[String], config: &AppConfig) -> f32 {
    config
        .audio
        .microphone
        .as_ref()
        .and_then(|id| ids.iter().position(|known| known == id))
        .map_or(0.0, |index| (index + 1) as f32)
}

fn info(devices: &[MicrophoneDevice], ids: &[String], config: &AppConfig) -> SettingInfo {
    let variant = |name: &str| NamedVariant {
        name: name.to_owned(),
        description: DESCRIPTION.to_owned(),
    };
    SettingInfo {
        name: TITLE.to_owned(),
        category: SettingCategory::Audio.to_string(),
        description: DESCRIPTION.to_owned(),
        min_value: 0.0,
        max_value: (devices.len() + 1) as f32,
        named_variants: std::iter::once(variant("System Default"))
            .chain(devices.iter().map(|device| variant(&device.label)))
            .collect(),
        value: choice(ids, config),
        default: 0.0,
        step_size: 1.0,
    }
}

/// Adds the setting; its choices follow [`MicrophoneDevices`].
// Only the web host reports devices.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(super) fn add(app: &mut App, settings: &mut Settings, config: &AppConfig) {
    let choices = Choices::default();
    let apply_ids = choices.0.clone();
    let load_ids = choices.0.clone();
    settings.settings.push(Setting {
        info: info(&[], &[], config),
        apply: Some(Box::new(
            move |config: &mut AppConfig, value: f32| -> Result<f32, anyhow::Error> {
                let ids = apply_ids.read().unwrap_or_else(PoisonError::into_inner);
                let index = value as usize;
                config.audio.microphone = match index {
                    0 => None,
                    index => Some(
                        ids.get(index - 1)
                            .ok_or(anyhow!("invalid variant index"))?
                            .clone(),
                    ),
                };
                Ok(index as f32)
            },
        )),
        load: Box::new(move |config: &AppConfig| {
            choice(
                &load_ids.read().unwrap_or_else(PoisonError::into_inner),
                config,
            )
        }),
    });
    app.insert_resource(choices);
    app.init_resource::<MicrophoneDevices>();
    app.add_systems(
        Update,
        sync_choices.run_if(resource_changed::<MicrophoneDevices>),
    );
}

fn sync_choices(
    devices: Res<MicrophoneDevices>,
    choices: Res<Choices>,
    config: Res<AppConfig>,
    mut settings: ResMut<Settings>,
) {
    let mut ids = choices.0.write().unwrap_or_else(PoisonError::into_inner);
    *ids = devices.0.iter().map(|device| device.id.clone()).collect();
    if let Some(setting) = settings.settings.iter_mut().find(|s| s.info.name == TITLE) {
        setting.info = info(&devices.0, &ids, &config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str) -> MicrophoneDevice {
        MicrophoneDevice {
            id: id.to_owned(),
            label: format!("{id} label"),
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.insert_resource(AppConfig::default());
        let mut settings = Settings {
            settings: Vec::default(),
        };
        add(&mut app, &mut settings, &AppConfig::default());
        app.insert_resource(settings);
        app
    }

    fn set(app: &mut App, value: f32) -> Result<(), anyhow::Error> {
        app.world_mut()
            .resource_scope(|world, mut settings: Mut<Settings>| {
                settings.set_value(&mut world.resource_mut::<AppConfig>(), TITLE, value)
            })
    }

    fn shown(app: &App) -> SettingInfo {
        app.world().resource::<Settings>().get().remove(0)
    }

    #[test]
    fn the_choices_follow_the_reported_devices_and_save_the_chosen_id() {
        let mut app = app();
        app.update();
        assert_eq!(shown(&app).named_variants.len(), 1);
        assert!(set(&mut app, 1.0).is_err());

        app.insert_resource(MicrophoneDevices(vec![device("a"), device("b")]));
        app.update();
        let names: Vec<_> = shown(&app)
            .named_variants
            .into_iter()
            .map(|v| v.name)
            .collect();
        assert_eq!(names, ["System Default", "a label", "b label"]);

        set(&mut app, 2.0).unwrap();
        assert_eq!(
            app.world()
                .resource::<AppConfig>()
                .audio
                .microphone
                .as_deref(),
            Some("b")
        );
        assert_eq!(shown(&app).value, 2.0);

        set(&mut app, 0.0).unwrap();
        assert_eq!(app.world().resource::<AppConfig>().audio.microphone, None);
    }

    #[test]
    fn an_unplugged_choice_shows_the_system_default_and_keeps_its_id() {
        let mut app = app();
        app.insert_resource(MicrophoneDevices(vec![device("a"), device("b")]));
        app.update();
        set(&mut app, 2.0).unwrap();

        app.insert_resource(MicrophoneDevices(vec![device("a")]));
        app.update();
        assert_eq!(shown(&app).value, 0.0);
        assert_eq!(
            app.world()
                .resource::<AppConfig>()
                .audio
                .microphone
                .as_deref(),
            Some("b")
        );

        app.insert_resource(MicrophoneDevices(vec![device("b"), device("a")]));
        app.update();
        assert_eq!(shown(&app).value, 1.0);
    }
}
