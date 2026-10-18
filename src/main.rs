#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod collect;
mod config;
mod demo;
mod ffi;
mod hotkeys;
mod logutil;
mod model;
mod store;
mod tray;
mod ui;

use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};

fn main() {
    let paths = config::Paths::resolve();
    if let Err(e) = logutil::init(&paths.dir) {
        eprintln!("file logging unavailable: {e}");
    }

    // A GUI binary has no console, so a panic would otherwise leave no trace.
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
        eprintln!("panic: {info}");
    }));

    if !ffi::single_instance_guard() {
        log::info!("another instance is already running; exiting");
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                ffi::wide("TrafficDesk is already running - look for it in the tray area.")
                    .as_ptr(),
                ffi::wide("TrafficDesk").as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        return;
    }

    log::info!(
        "TrafficDesk {} starting (data dir {})",
        env!("CARGO_PKG_VERSION"),
        paths.dir.display()
    );
    if demo::enabled() {
        log::info!(
            "TRAFFICDESK_DEMO is set: sample data only, nothing is captured and no history is written"
        );
    }
    let cfg = config::load_or_create(&paths.config);
    log::info!(
        "interval {} ms, retention {} days, start mode {}",
        cfg.poll_interval_ms,
        cfg.retention_days,
        cfg.mode
    );

    match app::run(paths, cfg) {
        Ok(()) => log::info!("clean shutdown"),
        Err(e) => log::error!("eframe terminated with error: {e:?}"),
    }
}
