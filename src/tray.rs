//! Tray icon and its menu. Created on the main thread, inside eframe's running
//! event loop, which is what tray-icon requires on Windows.

use crate::app::{Cmd, Mode};
use eframe::egui::Context;
use std::sync::mpsc::Sender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const ID_MINI: &str = "td.mini";
const ID_PANEL: &str = "td.panel";
const ID_EXPANDED: &str = "td.expanded";
const ID_TOGGLE: &str = "td.toggle";
const ID_QUIT: &str = "td.quit";

pub fn build(tx: Sender<Cmd>, ctx: Context) -> Option<TrayIcon> {
    let menu = Menu::new();
    let mini = MenuItem::with_id(ID_MINI, "MiniBar", true, None);
    let panel = MenuItem::with_id(ID_PANEL, "Panel", true, None);
    let expanded = MenuItem::with_id(ID_EXPANDED, "Expanded", true, None);
    let toggle = MenuItem::with_id(ID_TOGGLE, "Show / hide", true, None);
    let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);
    menu.append_items(&[
        &mini,
        &panel,
        &expanded,
        &PredefinedMenuItem::separator(),
        &toggle,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .ok()?;

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id.0 == ID_TOGGLE {
            // Handled here, on the tray thread: a hidden window must stay
            // reachable even when the UI thread is not running updates.
            match crate::ffi::set_own_window_visible(
                !crate::ffi::own_window_visible().unwrap_or(true),
            ) {
                Some(v) => log::info!("window {}", if v { "shown" } else { "hidden" }),
                None => log::warn!("could not find own window to toggle"),
            }
            ctx.request_repaint();
            return;
        }
        let cmd = match event.id.0.as_str() {
            ID_MINI => Some(Cmd::SetMode(Mode::Mini)),
            ID_PANEL => Some(Cmd::SetMode(Mode::Panel)),
            ID_EXPANDED => Some(Cmd::SetMode(Mode::Expanded)),
            ID_QUIT => Some(Cmd::Quit),
            _ => None,
        };
        if let Some(cmd) = cmd {
            let _ = tx.send(cmd);
        }
        ctx.request_repaint();
    }));

    let icon = icon()?;
    match TrayIconBuilder::new()
        .with_tooltip("TrafficDesk")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
    {
        Ok(tray) => {
            log::info!("tray icon ready");
            Some(tray)
        }
        Err(e) => {
            log::warn!("tray icon unavailable: {e}");
            None
        }
    }
}

/// The same mark the executable carries, decoded from the shipped PNG asset.
fn icon() -> Option<Icon> {
    let data = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-64.png")).ok()?;
    Icon::from_rgba(data.rgba, data.width, data.height).ok()
}
