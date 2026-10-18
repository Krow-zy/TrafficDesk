//! Global hotkeys via `RegisterHotKey`, pumped by a message loop on its own
//! thread. No extra dependency, and no admin rights required.

use crate::app::{Cmd, Mode};
use eframe::egui::Context;
use std::sync::mpsc::Sender;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

// Virtual key codes (VK_1, VK_2, VK_3, VK_H).
const VK_1: u32 = 0x31;
const VK_2: u32 = 0x32;
const VK_3: u32 = 0x33;
const VK_H: u32 = 0x48;

const ID_MINI: i32 = 0xA001;
const ID_PANEL: i32 = 0xA002;
const ID_EXPANDED: i32 = 0xA003;
const ID_TOGGLE: i32 = 0xA004;

pub fn spawn(tx: Sender<Cmd>, ctx: Context) {
    let _ = std::thread::Builder::new()
        .name("trafficdesk-hotkeys".into())
        .spawn(move || unsafe {
            let mods = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
            let bindings = [
                (ID_MINI, VK_1, Mode::Mini),
                (ID_PANEL, VK_2, Mode::Panel),
                (ID_EXPANDED, VK_3, Mode::Expanded),
            ];
            let mut registered: Vec<(i32, Mode)> = Vec::new();
            for (id, vk, mode) in bindings {
                if RegisterHotKey(std::ptr::null_mut(), id, mods, vk) != 0 {
                    registered.push((id, mode));
                } else {
                    log::warn!("hotkey Ctrl+Alt+{} already taken", (vk as u8) as char);
                }
            }
            let toggle_ok = RegisterHotKey(std::ptr::null_mut(), ID_TOGGLE, mods, VK_H) != 0;
            if !toggle_ok {
                log::warn!("hotkey Ctrl+Alt+H already taken");
            }
            log::info!(
                "registered {} global hotkeys",
                registered.len() + usize::from(toggle_ok)
            );

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.message != WM_HOTKEY {
                    continue;
                }
                let id = msg.wParam as i32;
                if id == ID_TOGGLE {
                    // Handled on this thread: a hidden window must stay reachable.
                    match crate::ffi::set_own_window_visible(
                        !crate::ffi::own_window_visible().unwrap_or(true),
                    ) {
                        Some(v) => log::info!("window {}", if v { "shown" } else { "hidden" }),
                        None => log::warn!("could not find own window to toggle"),
                    }
                    ctx.request_repaint();
                    continue;
                }
                if let Some(cmd) = registered
                    .iter()
                    .find(|(i, _)| *i == id)
                    .map(|(_, m)| Cmd::SetMode(*m))
                {
                    let _ = tx.send(cmd);
                    ctx.request_repaint();
                }
            }
        });
}
