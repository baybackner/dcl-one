use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard};

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

/// The audio inputs the host offers, in its order. Clones share one list, which the setting's
/// choices read; [`MicrophoneDevices::replace`] does not mark the resource changed.
#[derive(Resource, Default, Debug, Clone)]
pub struct MicrophoneDevices(Arc<RwLock<Vec<MicrophoneDevice>>>);

impl MicrophoneDevices {
    pub fn read(&self) -> RwLockReadGuard<'_, Vec<MicrophoneDevice>> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.read().iter().any(|device| device.id == id)
    }

    /// Returns false when the list was already `devices`.
    pub fn replace(&self, devices: Vec<MicrophoneDevice>) -> bool {
        let mut current = self.0.write().unwrap_or_else(PoisonError::into_inner);
        if *current == devices {
            return false;
        }
        *current = devices;
        true
    }
}

fn choice(devices: &[MicrophoneDevice], config: &AppConfig) -> f32 {
    config
        .audio
        .microphone
        .as_ref()
        .and_then(|id| devices.iter().position(|device| &device.id == id))
        .map_or(0.0, |index| (index + 1) as f32)
}

fn info(devices: &[MicrophoneDevice], config: &AppConfig) -> SettingInfo {
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
        value: choice(devices, config),
        default: 0.0,
        step_size: 1.0,
    }
}

/// Adds the setting; its choices follow [`MicrophoneDevices`].
// Only the web host reports devices.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(super) fn add(app: &mut App, settings: &mut Settings, config: &AppConfig) {
    let devices = app
        .world_mut()
        .get_resource_or_init::<MicrophoneDevices>()
        .clone();
    let apply_devices = devices.clone();
    settings.settings.push(Setting {
        info: info(&devices.read(), config),
        apply: Some(Box::new(
            move |config: &mut AppConfig, value: f32| -> Result<f32, anyhow::Error> {
                let index = value as usize;
                config.audio.microphone = match index {
                    0 => None,
                    index => Some(
                        apply_devices
                            .read()
                            .get(index - 1)
                            .ok_or(anyhow!("invalid variant index"))?
                            .id
                            .clone(),
                    ),
                };
                Ok(index as f32)
            },
        )),
        load: Box::new(move |config: &AppConfig| choice(&devices.read(), config)),
    });
    app.add_systems(
        Update,
        sync_choices.run_if(resource_changed::<MicrophoneDevices>),
    );
}

fn sync_choices(
    devices: Res<MicrophoneDevices>,
    config: Res<AppConfig>,
    mut settings: ResMut<Settings>,
) {
    if let Some(setting) = settings.settings.iter_mut().find(|s| s.info.name == TITLE) {
        setting.info = info(&devices.read(), &config);
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

    fn report(app: &mut App, ids: &[&str]) {
        let mut devices = app.world_mut().resource_mut::<MicrophoneDevices>();
        devices.replace(ids.iter().map(|id| device(id)).collect());
        devices.set_changed();
        app.update();
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

        report(&mut app, &["a", "b"]);
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
    fn a_choice_past_the_list_fails_and_the_setting_still_changes_after() {
        let mut app = app();
        report(&mut app, &["a"]);
        assert!(set(&mut app, 2.0).is_err());
        assert_eq!(app.world().resource::<AppConfig>().audio.microphone, None);

        set(&mut app, 1.0).unwrap();
        assert_eq!(
            app.world()
                .resource::<AppConfig>()
                .audio
                .microphone
                .as_deref(),
            Some("a")
        );
    }

    #[test]
    fn an_unplugged_choice_shows_the_system_default_and_keeps_its_id() {
        let mut app = app();
        report(&mut app, &["a", "b"]);
        set(&mut app, 2.0).unwrap();

        report(&mut app, &["a"]);
        assert_eq!(shown(&app).value, 0.0);
        assert_eq!(
            app.world()
                .resource::<AppConfig>()
                .audio
                .microphone
                .as_deref(),
            Some("b")
        );

        report(&mut app, &["b", "a"]);
        assert_eq!(shown(&app).value, 1.0);
    }
}
