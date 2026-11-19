use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, PoisonError, RwLock, RwLockReadGuard,
};

use anyhow::anyhow;
use bevy::prelude::*;
use common::structs::AppConfig;

use super::{NamedVariant, Setting, SettingCategory, SettingInfo, Settings};

pub const TITLE: &str = "Microphone";
const DESCRIPTION: &str = "The microphone voice chat captures from.\n\nSystem Default follows the device's own choice. While a chosen microphone is unplugged, voice chat uses the system default.\n\nThe list fills in once microphone access is granted.";

/// An audio input the host offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneInput {
    pub id: String,
    pub label: String,
}

/// An input reported during this session, kept after it is unplugged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownMicrophone {
    device: MicrophoneInput,
    pub present: bool,
    /// How many reports found it gone: after one listing it, or as the first report.
    absences: u32,
}

/// Every audio input the host reported this session, in first-report order. An input keeps its
/// place once seen, so a choice index read from an older list still names the same input. Clones
/// share one list, which the setting's choices read; [`MicrophoneDevices::replace`] does not mark
/// the resource changed.
#[derive(Resource, Default, Debug, Clone)]
pub struct MicrophoneDevices {
    known: Arc<RwLock<Vec<KnownMicrophone>>>,
    /// The host has reported its inputs at least once.
    reported: Arc<AtomicBool>,
}

impl MicrophoneDevices {
    pub fn read(&self) -> RwLockReadGuard<'_, Vec<KnownMicrophone>> {
        self.known.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the host has reported its inputs at least once.
    pub fn reported(&self) -> bool {
        self.reported.load(Ordering::SeqCst)
    }

    fn contains(&self, id: &str) -> bool {
        self.read()
            .iter()
            .any(|known| known.present && known.device.id == id)
    }

    /// Whether the host reported its inputs and `id` was not among them.
    pub fn missing(&self, id: &str) -> bool {
        self.reported() && !self.contains(id)
    }

    /// How `id` is named in the choices, if it is one.
    pub fn label(&self, id: &str) -> Option<String> {
        let reported = self.reported();
        self.read()
            .iter()
            .find(|entry| entry.device.id == id)
            .map(|entry| shown_label(entry, reported))
    }

    /// Whether the host reported its inputs and `id` was among them.
    pub fn offers(&self, id: &str) -> bool {
        self.reported() && self.contains(id)
    }

    /// How many reports found `id` gone; a change means it went missing in between.
    pub fn absences(&self, id: &str) -> u32 {
        self.read()
            .iter()
            .find(|known| known.device.id == id)
            .map_or(0, |known| known.absences)
    }

    /// Lists a saved choice the host has not reported yet, so the first report that lacks it
    /// counts as an absence.
    fn remember(&self, id: &str) {
        let mut known = self.known.write().unwrap_or_else(PoisonError::into_inner);
        if !known.iter().any(|entry| entry.device.id == id) {
            known.push(KnownMicrophone {
                device: MicrophoneInput {
                    id: id.to_owned(),
                    label: "Saved microphone".to_owned(),
                },
                present: false,
                absences: 0,
            });
        }
    }

    /// Takes the host's current inputs; returns false when nothing changed.
    pub fn replace(&self, reported: Vec<MicrophoneInput>) -> bool {
        let first = !self.reported.swap(true, Ordering::SeqCst);
        let mut known = self.known.write().unwrap_or_else(PoisonError::into_inner);
        let before = known.clone();
        for entry in known.iter_mut() {
            if !reported.iter().any(|device| device.id == entry.device.id) {
                if entry.present || first {
                    entry.absences += 1;
                }
                entry.present = false;
            }
        }
        for device in reported {
            match known.iter_mut().find(|entry| entry.device.id == device.id) {
                Some(entry) => {
                    entry.device.label = device.label;
                    entry.present = true;
                }
                None => known.push(KnownMicrophone {
                    device,
                    present: true,
                    absences: 0,
                }),
            }
        }
        first || *known != before
    }
}

/// Before the first report nothing is known to be unplugged.
fn shown_label(entry: &KnownMicrophone, reported: bool) -> String {
    if entry.present || !reported {
        entry.device.label.clone()
    } else {
        format!("{} (unplugged)", entry.device.label)
    }
}

fn choice(devices: &MicrophoneDevices, config: &AppConfig) -> f32 {
    config
        .microphone
        .as_ref()
        .and_then(|id| {
            devices
                .read()
                .iter()
                .position(|entry| &entry.device.id == id)
        })
        .map_or(0.0, |index| (index + 1) as f32)
}

fn info(devices: &MicrophoneDevices, config: &AppConfig) -> SettingInfo {
    let variant = |name: String| NamedVariant {
        name,
        description: DESCRIPTION.to_owned(),
    };
    let value = choice(devices, config);
    let reported = devices.reported();
    let known = devices.read();
    SettingInfo {
        name: TITLE.to_owned(),
        category: SettingCategory::Audio.to_string(),
        description: DESCRIPTION.to_owned(),
        min_value: 0.0,
        max_value: (known.len() + 1) as f32,
        named_variants: std::iter::once(variant("System Default".to_owned()))
            .chain(
                known
                    .iter()
                    .map(|entry| variant(shown_label(entry, reported))),
            )
            .collect(),
        value,
        default: 0.0,
        step_size: 1.0,
    }
}

/// Adds the setting; its choices follow [`MicrophoneDevices`].
// Only the web host reports devices.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(super) fn add(app: &mut App, settings: &mut Settings, config: &AppConfig) {
    // The livekit plugin, which fills the list, creates it too: either plugin can be built
    // without the other, and whichever builds first creates the one shared list.
    let devices = app
        .world_mut()
        .get_resource_or_init::<MicrophoneDevices>()
        .clone();
    if let Some(id) = &config.microphone {
        devices.remember(id);
    }
    let apply_devices = devices.clone();
    let info = info(&devices, config);
    settings.settings.push(Setting {
        info,
        apply: Some(Box::new(
            move |config: &mut AppConfig, value: f32| -> Result<f32, anyhow::Error> {
                let index = value as usize;
                config.microphone = match index {
                    0 => None,
                    index => Some(
                        apply_devices
                            .read()
                            .get(index - 1)
                            .ok_or(anyhow!("invalid variant index"))?
                            .device
                            .id
                            .clone(),
                    ),
                };
                Ok(index as f32)
            },
        )),
        load: Box::new(move |config: &AppConfig| choice(&devices, config)),
        runtime_choices: true,
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
        setting.info = info(&devices, &config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str) -> MicrophoneInput {
        MicrophoneInput {
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
            app.world().resource::<AppConfig>().microphone.as_deref(),
            Some("b")
        );
        assert_eq!(shown(&app).value, 2.0);

        set(&mut app, 0.0).unwrap();
        assert_eq!(app.world().resource::<AppConfig>().microphone, None);
    }

    #[test]
    fn a_choice_past_the_list_fails_and_the_setting_still_changes_after() {
        let mut app = app();
        report(&mut app, &["a"]);
        assert!(set(&mut app, 2.0).is_err());
        assert_eq!(app.world().resource::<AppConfig>().microphone, None);

        set(&mut app, 1.0).unwrap();
        assert_eq!(
            app.world().resource::<AppConfig>().microphone.as_deref(),
            Some("a")
        );
    }

    #[test]
    fn an_unplugged_choice_stays_chosen_and_keeps_its_place() {
        let mut app = app();
        report(&mut app, &["a", "b"]);
        set(&mut app, 2.0).unwrap();

        report(&mut app, &["a"]);
        let shown_now = shown(&app);
        assert_eq!(shown_now.value, 2.0);
        assert_eq!(shown_now.named_variants[2].name, "b label (unplugged)");
        assert!(!app.world().resource::<MicrophoneDevices>().contains("b"));

        report(&mut app, &["b", "a"]);
        assert_eq!(shown(&app).value, 2.0);
        assert_eq!(shown(&app).named_variants[2].name, "b label");
        assert!(app.world().resource::<MicrophoneDevices>().contains("b"));
    }

    #[test]
    fn an_index_from_an_older_list_names_the_input_it_named_then() {
        let mut app = app();
        report(&mut app, &["a", "b"]);
        // a new input plugged in ahead of the others joins the end
        report(&mut app, &["c", "a"]);
        set(&mut app, 1.0).unwrap();
        assert_eq!(
            app.world().resource::<AppConfig>().microphone.as_deref(),
            Some("a")
        );
        set(&mut app, 2.0).unwrap();
        assert_eq!(
            app.world().resource::<AppConfig>().microphone.as_deref(),
            Some("b")
        );
    }

    #[test]
    fn a_saved_choice_not_yet_reported_shows_as_chosen_by_its_saved_name() {
        let config = AppConfig {
            microphone: Some("headset".to_owned()),
            ..Default::default()
        };
        let mut app = App::new();
        app.insert_resource(config.clone());
        let mut settings = Settings {
            settings: Vec::default(),
        };
        add(&mut app, &mut settings, &config);
        app.insert_resource(settings);
        app.update();
        let shown_now = shown(&app);
        assert_eq!(shown_now.value, 1.0);
        assert_eq!(shown_now.named_variants[1].name, "Saved microphone");
        assert!(!app
            .world()
            .resource::<MicrophoneDevices>()
            .missing("headset"));

        report(&mut app, &["a", "headset"]);
        let shown_now = shown(&app);
        assert_eq!(shown_now.value, 1.0);
        assert_eq!(shown_now.named_variants[1].name, "headset label");
    }

    #[test]
    fn a_first_report_without_the_saved_choice_counts_as_its_absence() {
        let devices = MicrophoneDevices::default();
        devices.remember("headset");
        assert_eq!(devices.absences("headset"), 0);
        assert!(!devices.missing("headset"));
        devices.replace(Vec::new());
        assert_eq!(devices.absences("headset"), 1);
        assert!(devices.missing("headset"));
        devices.replace(vec![device("headset")]);
        assert_eq!(devices.absences("headset"), 1);
        assert!(devices.offers("headset"));
    }
}
