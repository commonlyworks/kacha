#![windows_subsystem = "windows"]

mod about;
mod apps;
mod bar;
mod dock;
mod fonts;
mod panel;
mod power;
mod settings;
mod settings_ui;
mod status;
mod taskbar;
mod volume;
mod wifi;

use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, TRUE};
use windows::Win32::System::Threading::CreateMutexW;

fn main() {
    // `kacha.exe --restore` : paksa kembalikan taskbar (kalau app crash / di-kill).
    if std::env::args().any(|a| a == "--restore") {
        taskbar::restore_force();
        return;
    }

    // Hanya boleh satu instance.
    unsafe {
        let _mutex = CreateMutexW(None, TRUE, w!("Local\\KachaSingleton"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
    }

    fonts::load();
    settings::load();

    // Kalau panic, taskbar tetap dikembalikan.
    std::panic::set_hook(Box::new(|_| {
        taskbar::restore();
    }));

    taskbar::hide();
    let _ = bar::run();
    taskbar::restore();
}