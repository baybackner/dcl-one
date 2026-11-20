use bevy::{
    prelude::*,
    window::{PrimaryWindow, RawHandleWrapper},
};
use winit::raw_window_handle::RawDisplayHandle;

/// Present while the clipboard uses the primary window's Wayland display. The app drops its
/// resources on exit while winit still holds that display open, and the drop ends the use.
#[derive(Resource)]
struct WaylandClipboard;

impl Drop for WaylandClipboard {
    fn drop(&mut self) {
        copypwasmta::clear_wayland_display();
    }
}

/// Gives the clipboard the primary window's Wayland display when winit put the window on Wayland,
/// so chat, text inputs and scene copies use that session's clipboard. A window on X11 registers
/// nothing, and the clipboard stays on X11.
pub(crate) fn register_wayland_clipboard(
    mut commands: Commands,
    window: Query<&RawHandleWrapper, (With<PrimaryWindow>, Added<RawHandleWrapper>)>,
) {
    let Ok(handle) = window.single() else {
        return;
    };
    if let RawDisplayHandle::Wayland(handle) = handle.get_display_handle() {
        // SAFETY: the pointer is the display of winit's Wayland connection, open until after the
        // app drops `WaylandClipboard`, which ends the clipboard's use of it.
        if unsafe { copypwasmta::set_wayland_display(handle.display.as_ptr()) } {
            commands.insert_resource(WaylandClipboard);
        }
    }
}
