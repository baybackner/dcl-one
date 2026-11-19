use bevy::{
    ecs::system::{
        lifetimeless::{SQuery, Write},
        SystemParamItem,
    },
    platform::collections::HashSet,
    prelude::*,
    window::{PresentMode, PrimaryWindow},
};
use common::structs::AppConfig;

use super::{AppSetting, EnumAppSetting};

#[derive(Debug, PartialEq, Eq)]
pub enum VsyncSetting {
    Off,
    On,
}

impl VsyncSetting {
    pub fn present_mode(&self) -> PresentMode {
        match self {
            VsyncSetting::Off => PresentMode::AutoNoVsync,
            VsyncSetting::On => PresentMode::AutoVsync,
        }
    }
}

impl EnumAppSetting for VsyncSetting {
    fn variants() -> Vec<Self> {
        vec![Self::Off, Self::On]
    }

    fn name(&self) -> String {
        match self {
            VsyncSetting::Off => "Off",
            VsyncSetting::On => "On",
        }
        .to_owned()
    }
}

impl AppSetting for VsyncSetting {
    type Param = SQuery<Write<Window>, With<PrimaryWindow>>;

    fn title() -> String {
        "VSync".to_owned()
    }

    fn description(&self) -> String {
        "Sync frames to the monitor refresh.\n\nOn: the display's refresh rate sets the frame rate and Target Frame Rate has no effect. Off: Target Frame Rate caps the frame rate.".to_owned()
    }

    fn category() -> super::SettingCategory {
        super::SettingCategory::Performance
    }

    fn save(&self, config: &mut AppConfig) {
        config.graphics.vsync = matches!(self, VsyncSetting::On);
    }

    fn load(config: &AppConfig) -> Self {
        if config.graphics.vsync {
            Self::On
        } else {
            Self::Off
        }
    }

    fn apply(&self, mut window: SystemParamItem<Self::Param>, _: Commands, _: &HashSet<Entity>) {
        if let Ok(mut window) = window.single_mut() {
            window.present_mode = self.present_mode();
        }
    }
}
