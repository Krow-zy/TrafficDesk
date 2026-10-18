//! eframe application: window chrome, the three UI modes, and the glue that
//! fans tray/hotkey commands into state.

use crate::collect::{self, Shared};
use crate::config::{Config, Paths};
use crate::demo;
use crate::model::{ConnEvent, DestAgg, Snapshot, fmt_ago, fmt_count, fmt_rate};
use crate::store::Store;
use crate::{ffi, hotkeys, tray, ui};
use eframe::egui::{self, Rect, RichText, Vec2};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Mini,
    Panel,
    Expanded,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Mini => "mini",
            Mode::Panel => "panel",
            Mode::Expanded => "expanded",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "mini" => Mode::Mini,
            "expanded" => Mode::Expanded,
            _ => Mode::Panel,
        }
    }

    /// (inner size, minimum size, resizable)
    fn geometry(self) -> (Vec2, Vec2, bool) {
        match self {
            Mode::Mini => (Vec2::new(560.0, 28.0), Vec2::new(420.0, 28.0), false),
            Mode::Panel => (Vec2::new(360.0, 430.0), Vec2::new(320.0, 240.0), false),
            Mode::Expanded => (Vec2::new(780.0, 540.0), Vec2::new(560.0, 340.0), true),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    SetMode(Mode),
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Tab {
    #[default]
    Detail,
    Processes,
    Settings,
}

pub fn run(paths: Paths, cfg: Config) -> eframe::Result {
    let mode = Mode::from_str(&cfg.mode);
    let (size, min, resizable) = mode.geometry();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("TrafficDesk")
        .with_decorations(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_resizable(resizable)
        .with_inner_size(size)
        .with_min_inner_size(min);
    // Same artwork as assets/icon.ico, decoded from the PNG that ships beside
    // it, so the window, the tray and Explorer all show one mark.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "TrafficDesk",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, paths, cfg)))),
    )
}

struct App {
    paths: Paths,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    collector: Option<std::thread::JoinHandle<()>>,
    rx: Receiver<Cmd>,
    // UI state
    mode: Mode,
    /// True when started with `TRAFFICDESK_DEMO`: sample data instead of capture.
    demo: bool,
    visible: bool,
    tab: Tab,
    search: String,
    selected: Option<String>,
    draft: Config,
    toast: Option<(String, Instant)>,
    timeline: Vec<ConnEvent>,
    timeline_for: Option<(String, u64)>,
    /// Last geometry pushed to the window, so we only send commands on change.
    applied_mode: Mode,
    /// Handle of our own window, cached once found, plus when we last told
    /// Windows to keep it out of the taskbar.
    hwnd: Option<isize>,
    last_tool_window_assert: Option<std::time::Instant>,
    /// Frames still owed a taskbar-style re-assert after a geometry change.
    assert_burst: u8,
    _tray: Option<tray_icon::TrayIcon>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, paths: Paths, cfg: Config) -> Self {
        ui::set_style(&cc.egui_ctx);
        let shared = Arc::new(Mutex::new(Shared {
            snap: Snapshot::default(),
            cfg: cfg.clone(),
            note: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        // Demo mode swaps the collector for a sample-data generator: no capture,
        // no database, but the same snapshot and the same UI code path.
        let demo = demo::enabled();
        let collector = Some(if demo {
            demo::spawn(shared.clone(), stop.clone())
        } else {
            collect::spawn(shared.clone(), paths.clone(), stop.clone())
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let tray_icon = tray::build(tx.clone(), cc.egui_ctx.clone());
        hotkeys::spawn(tx.clone(), cc.egui_ctx.clone());
        let mode = Mode::from_str(&cfg.mode);
        log::info!("UI ready in {} mode", mode.as_str());
        Self {
            paths,
            shared,
            stop,
            collector,
            rx,
            mode,
            demo,
            visible: true,
            tab: Tab::default(),
            search: String::new(),
            // The detail pane needs a row; the sample data always has one.
            selected: demo.then(|| "github.com".to_string()),
            draft: cfg,
            toast: None,
            timeline: Vec::new(),
            timeline_for: None,
            applied_mode: mode,
            hwnd: None,
            last_tool_window_assert: None,
            assert_burst: 0,
            _tray: tray_icon,
        }
    }

    fn handle(&mut self, cmd: Cmd, ctx: &egui::Context) {
        match cmd {
            Cmd::SetMode(mode) => {
                self.mode = mode;
                self.persist_mode(mode);
                if !self.visible {
                    self.show_window();
                }
            }
            Cmd::Quit => {
                self.stop.store(true, Ordering::Relaxed);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn show_window(&mut self) {
        if let Some(v) = ffi::set_own_window_visible(true) {
            self.visible = v;
            log::info!("window {}", if v { "shown" } else { "hidden" });
        }
    }

    fn persist_mode(&mut self, mode: Mode) {
        self.draft.mode = mode.as_str().to_string();
        if let Ok(mut s) = self.shared.lock() {
            s.cfg.mode = mode.as_str().to_string();
        }
        self.write_config();
    }

    fn write_config(&mut self) {
        match std::fs::write(&self.paths.config, self.draft.to_toml()) {
            Ok(()) => self.set_toast("config saved"),
            Err(e) => {
                log::warn!("cannot write config: {e}");
                self.set_toast(format!("config write failed: {e}"));
            }
        }
    }

    fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn assert_tool_window(&mut self) {
        if self.hwnd.is_none() {
            self.hwnd = ffi::find_own_window("TrafficDesk");
        }
        if let Some(h) = self.hwnd {
            ffi::assert_tool_window(h);
        }
    }

    fn apply_geometry(&mut self, ctx: &egui::Context) {
        if self.applied_mode == self.mode {
            return;
        }
        self.applied_mode = self.mode;
        let (size, min, resizable) = self.mode.geometry();
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(resizable));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        self.assert_burst = 3;
    }

    fn snapshot(&self) -> Snapshot {
        self.shared
            .lock()
            .map(|s| s.snap.clone())
            .unwrap_or_default()
    }

    fn collector_dead(&self) -> bool {
        self.collector
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(true)
    }

    // ---------------------------------------------------------------- views ---

    fn mini(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, snap: &Snapshot) {
        ui.horizontal_centered(|ui| {
            let (rect, resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width() - 56.0, 24.0),
                egui::Sense::drag(),
            );
            if resp.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            let p = ui.painter();
            // Rates first: the mini bar exists to answer "how much, right now".
            let f = ui::font(11.5);
            let badge = Rect::from_min_size(
                rect.left_center() - Vec2::new(0.0, 9.0),
                Vec2::new(22.0, 18.0),
            );
            p.rect_filled(badge, ui::corner(3), ui::BAR_BG);
            p.rect_stroke(
                badge,
                ui::corner(3),
                egui::Stroke::new(1.0_f32, ui::BORDER),
                egui::StrokeKind::Inside,
            );
            p.text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                "TD",
                ui::font(10.5),
                ui::DIM,
            );
            let x = badge.right() + 8.0;
            p.text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "DN",
                f.clone(),
                ui::DOWN,
            );
            let dn = ui::fit(p, &fmt_rate(snap.down_bps), &f, 62.0);
            p.text(
                egui::pos2(x + 20.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                dn,
                f.clone(),
                ui::TEXT,
            );
            p.text(
                egui::pos2(x + 88.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "UP",
                f.clone(),
                ui::UP,
            );
            let up = ui::fit(p, &fmt_rate(snap.up_bps), &f, 62.0);
            p.text(
                egui::pos2(x + 108.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                up,
                f.clone(),
                ui::TEXT,
            );

            // Busiest live destination, or the newest thing that happened.
            let top = snap
                .dests
                .iter()
                .find(|d| d.live > 0)
                .or_else(|| snap.dests.first());
            let mut text_x = x + 176.0;
            let text_rect = Rect::from_min_max(
                egui::pos2(text_x, rect.top()),
                egui::pos2(rect.right() - 4.0, rect.bottom()),
            );
            if let Some(d) = top {
                let dot = egui::pos2(text_x, rect.center().y);
                if let Some(c) = ui::name_color(d.named) {
                    p.circle_filled(dot, 2.5, c);
                } else {
                    p.circle_stroke(dot, 2.5, egui::Stroke::new(1.0_f32, ui::DIM));
                }
                text_x += 8.0;
                let detail = if d.live > 0 {
                    format!("{}  live {}", d.dest, d.live)
                } else {
                    d.dest.clone()
                };
                let detail = ui::fit(p, &detail, &f, text_rect.width() - 8.0);
                p.text(
                    egui::pos2(text_x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    detail,
                    f,
                    ui::cat_color(d.category),
                );
            } else {
                p.text(
                    egui::pos2(text_x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "no outbound connections",
                    f,
                    ui::DIM,
                );
            }

            if ui.button(RichText::new("panel").size(10.5)).clicked() {
                self.handle(Cmd::SetMode(Mode::Panel), ctx);
            }
            if ui.button(RichText::new("x").size(10.5)).clicked() {
                self.handle(Cmd::Quit, ctx);
            }
        });
    }

    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, snap: &Snapshot, title: &str) {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), egui::Sense::drag());
        if resp.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            let f = ui::font(11.5);
            p.text(
                rect.left_center(),
                egui::Align2::LEFT_CENTER,
                title,
                f.clone(),
                ui::TEXT,
            );
            let mut x = rect.left() + title.len() as f32 * 7.0 + 14.0;
            p.text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "DN",
                f.clone(),
                ui::DOWN,
            );
            x += 20.0;
            let dn = ui::fit(p, &fmt_rate(snap.down_bps), &f, 60.0);
            p.text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &dn,
                f.clone(),
                ui::TEXT,
            );
            x += p.layout_no_wrap(dn, f.clone(), ui::TEXT).size().x + 12.0;
            p.text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "UP",
                f.clone(),
                ui::UP,
            );
            x += 20.0;
            let up = ui::fit(p, &fmt_rate(snap.up_bps), &f, 60.0);
            p.text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                up,
                f,
                ui::TEXT,
            );
        }
        let mut x = rect.right() - 22.0;
        let close = egui::Rect::from_min_max(
            egui::pos2(x, rect.top()),
            egui::pos2(rect.right(), rect.bottom()),
        );
        if ui
            .put(close, egui::Button::new(RichText::new("x").size(10.5)))
            .clicked()
        {
            self.handle(Cmd::Quit, ctx);
        }
        x -= 26.0;
        let expand = egui::Rect::from_min_max(
            egui::pos2(x, rect.top()),
            egui::pos2(x + 24.0, rect.bottom()),
        );
        let (label, target) = if self.mode == Mode::Expanded {
            ("^", Mode::Panel)
        } else {
            ("v", Mode::Expanded)
        };
        if ui
            .put(expand, egui::Button::new(RichText::new(label).size(10.5)))
            .clicked()
        {
            self.handle(Cmd::SetMode(target), ctx);
        }
        // Hairline so the drag strip and the content below are visibly separate.
        let p = ui.painter();
        p.line_segment(
            [
                egui::pos2(rect.left(), rect.bottom() - 0.5),
                egui::pos2(rect.right(), rect.bottom() - 0.5),
            ],
            egui::Stroke::new(1.0_f32, ui::BORDER),
        );
    }

    fn panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, snap: &Snapshot) {
        self.header(ui, ctx, snap, "TrafficDesk");
        ui.add_space(2.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("filter domain / ip / process")
                .desired_width(f32::INFINITY)
                .font(ui::font(11.5)),
        );
        ui.add_space(3.0);
        ui::sparkline(ui, &snap.spark, 36.0);
        ui.add_space(2.0);

        let query = self.search.trim().to_ascii_lowercase();
        let rows: Vec<&DestAgg> = snap
            .dests
            .iter()
            .filter(|d| {
                query.is_empty()
                    || d.dest.to_ascii_lowercase().contains(&query)
                    || d.ip.contains(&query)
                    || d.procs
                        .iter()
                        .any(|(p, _)| p.to_ascii_lowercase().contains(&query))
            })
            .take(400)
            .collect();

        ui::col_header(ui, &header_cols(), "destination");
        let mut clicked: Option<String> = None;
        let avail = ui.available_height() - 30.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(avail.max(60.0))
            .show(ui, |ui| {
                for (i, d) in rows.iter().enumerate() {
                    let sel = self.selected.as_deref() == Some(d.dest.as_str());
                    let cols = row_cols(d);
                    let resp = ui::paint_row(
                        ui,
                        ui::ROW_H,
                        i,
                        sel,
                        ui::cat_color(d.category),
                        &d.dest,
                        if d.live == 0 { ui::DIM } else { ui::TEXT },
                        &cols,
                        d.named,
                    );
                    let hint = d.procs.first().map(|(p, _)| p.as_str()).unwrap_or("");
                    let mut tip = format!(
                        "{}\n{}\nlive {}  events {}",
                        d.dest,
                        d.ip,
                        d.live,
                        fmt_count(d.events)
                    );
                    tip.push_str(&format!("\nname: {}", d.named.label()));
                    tip.push_str(&format!(
                        "\nlast process: {hint}\nlast seen {}",
                        fmt_ago(d.last_seen)
                    ));
                    let resp = resp.on_hover_text(tip);
                    if resp.clicked() {
                        clicked = Some(d.dest.clone());
                    }
                }
            });

        if let Some(dest) = clicked {
            self.selected = Some(dest);
            self.tab = Tab::Detail;
            self.handle(Cmd::SetMode(Mode::Expanded), ctx);
        }
        self.footer(ui, snap);
    }

    fn expanded(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, snap: &Snapshot) {
        self.header(ui, ctx, snap, "TrafficDesk / expanded");
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            for (tab, label) in [
                (Tab::Detail, "detail"),
                (Tab::Processes, "processes"),
                (Tab::Settings, "settings"),
            ] {
                if ui
                    .selectable_label(self.tab == tab, RichText::new(label).monospace().size(11.5))
                    .clicked()
                {
                    self.tab = tab;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} sockets/tick", snap.connections_seen))
                        .color(ui::DIM)
                        .monospace()
                        .size(10.5),
                );
            });
        });
        ui.separator();
        match self.tab {
            Tab::Detail => self.detail(ui, snap),
            Tab::Processes => self.processes(ui, snap),
            Tab::Settings => self.settings(ui, snap),
        }
        self.footer(ui, snap);
    }

    fn detail(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let Some(dest) = self.selected.clone() else {
            ui.add_space(10.0);
            ui.label(
                RichText::new("Pick a row in the panel to inspect it.")
                    .color(ui::DIM)
                    .monospace(),
            );
            return;
        };
        let Some(d) = snap.dests.iter().find(|d| d.dest == dest) else {
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("{dest} is not active right now (history only)."))
                    .color(ui::DIM)
                    .monospace(),
            );
            self.reload_timeline(&dest, 0);
            self.timeline_list(ui);
            return;
        };

        ui.add_space(4.0);
        ui.label(
            RichText::new(&d.dest)
                .color(ui::TEXT)
                .monospace()
                .size(14.0)
                .strong(),
        );
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui::chip(
                ui,
                d.named.label(),
                ui::name_color(d.named).unwrap_or(ui::DIM),
            );
            ui::chip(
                ui,
                match d.category {
                    crate::model::Category::System => "system",
                    crate::model::Category::Browser => "browser",
                    crate::model::Category::Other => "other",
                },
                ui::cat_color(d.category),
            );
            ui::chip(
                ui,
                if d.live > 0 { "live" } else { "idle" },
                if d.live > 0 { ui::UP } else { ui::DIM },
            );
        });
        ui.add_space(4.0);
        ui::section(ui, "peer");
        ui::kv(ui, "remote ip", &d.ip, ui::TEXT);
        ui::kv(
            ui,
            "name source",
            match d.named {
                crate::model::NameSource::Cache => "windows dns cache (resolved by the os)",
                crate::model::NameSource::Remembered => "remembered from an earlier lookup",
                crate::model::NameSource::None => "no cached name - showing the raw ip",
            },
            match d.named {
                crate::model::NameSource::Cache => ui::UP,
                crate::model::NameSource::Remembered => ui::REMEMBERED,
                crate::model::NameSource::None => ui::WARN,
            },
        );
        if d.named == crate::model::NameSource::Remembered && d.named_at > 0 {
            ui::kv(
                ui,
                "named",
                &format!("{} ago", fmt_ago(d.named_at)),
                ui::DIM,
            );
        }
        ui::kv(ui, "live conns", &d.live.to_string(), ui::TEXT);
        ui::kv(ui, "events", &fmt_count(d.events), ui::TEXT);
        ui::kv(ui, "first seen", &fmt_ago(d.first_seen), ui::DIM);
        ui::kv(ui, "last seen", &fmt_ago(d.last_seen), ui::DIM);
        ui::kv(
            ui,
            "top process",
            &format!("{} (pid {})", d.top_process, d.top_pid),
            ui::TEXT,
        );
        if !d.top_path.is_empty() {
            ui::kv(ui, "path", &d.top_path, ui::DIM);
        }
        ui::section(ui, "processes talking here");
        for (i, (name, live)) in d.procs.iter().enumerate() {
            let cols = vec![(format!("{live}"), 30.0)];
            ui::paint_row(
                ui,
                18.0,
                i,
                false,
                ui::cat_color(d.category),
                name,
                ui::TEXT,
                &cols,
                crate::model::NameSource::None,
            );
        }

        ui::section(ui, "connection events");
        self.reload_timeline(&dest, snap.tick / 5);
        self.timeline_list(ui);
    }

    fn reload_timeline(&mut self, dest: &str, bucket: u64) {
        if let Some((cached, b)) = &self.timeline_for
            && cached == dest
            && *b == bucket
        {
            return;
        }
        self.timeline_for = Some((dest.to_string(), bucket));
        if self.demo {
            self.timeline = demo::timeline(dest);
            return;
        }
        self.timeline = Store::timeline(dest, &self.paths.db, 60).unwrap_or_else(|e| {
            log::warn!("timeline query failed: {e}");
            Vec::new()
        });
    }

    fn timeline_list(&mut self, ui: &mut egui::Ui) {
        if self.timeline.is_empty() {
            ui.label(
                RichText::new("no recorded events yet for this destination")
                    .color(ui::DIM)
                    .monospace()
                    .size(11.0),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(170.0)
            .show(ui, |ui| {
                for (i, e) in self.timeline.iter().enumerate() {
                    let cols = vec![(fmt_ago(e.ts), 38.0), (e.state.clone(), 68.0)];
                    ui::paint_row(
                        ui,
                        18.0,
                        i,
                        false,
                        ui::cat_color(crate::model::Category::classify(&e.process, &e.path, e.pid)),
                        &format!("{}:{}  {}", e.ip, e.port, e.process),
                        ui::TEXT,
                        &cols,
                        if e.is_domain {
                            crate::model::NameSource::Cache
                        } else {
                            crate::model::NameSource::None
                        },
                    );
                }
            });
    }

    fn processes(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.add_space(3.0);
        let cols = process_cols();
        ui::col_header(ui, &cols, "process");
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, p) in snap.procs.iter().enumerate() {
                    let label = if p.path.is_empty() {
                        p.process.clone()
                    } else {
                        format!("{}  (pid {})", p.process, p.pid)
                    };
                    let row = vec![
                        (format!("{}", p.live), 30.0),
                        (fmt_count(p.events), 48.0),
                        (format!("{}", p.dests), 30.0),
                        (fmt_ago(p.last_seen), 38.0),
                    ];
                    let resp = ui::paint_row(
                        ui,
                        18.0,
                        i,
                        false,
                        ui::cat_color(p.category),
                        &label,
                        if p.live == 0 { ui::DIM } else { ui::TEXT },
                        &row,
                        crate::model::NameSource::None,
                    );
                    if !p.path.is_empty() {
                        resp.on_hover_text(&p.path);
                    }
                }
            });
    }

    fn settings(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let mut changed = false;
        ui::section(ui, "collection");
        ui.horizontal(|ui| {
            ui.add_sized(
                [104.0, 16.0],
                egui::Label::new(
                    RichText::new("poll interval")
                        .color(ui::DIM)
                        .monospace()
                        .size(11.0),
                ),
            );
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.draft.poll_interval_ms, 500..=5000)
                        .suffix(" ms")
                        .step_by(100.0),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.add_sized(
                [104.0, 16.0],
                egui::Label::new(
                    RichText::new("retention")
                        .color(ui::DIM)
                        .monospace()
                        .size(11.0),
                ),
            );
            changed |= ui
                .add(egui::Slider::new(&mut self.draft.retention_days, 1..=365).suffix(" days"))
                .changed();
        });
        ui::section(ui, "ignore processes (substring, one per line)");
        let mut ignore = self.draft.ignore_processes.join("\n");
        if ui
            .add(
                egui::TextEdit::multiline(&mut ignore)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .font(ui::font(11.5)),
            )
            .changed()
        {
            self.draft.ignore_processes = ignore
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            changed = true;
        }
        ui.horizontal(|ui| {
            if ui.button("save config.toml").clicked() {
                self.draft.clamp();
                self.write_config();
            }
            if ui.button("reload").clicked() {
                self.draft = crate::config::load_or_create(&self.paths.config);
                self.set_toast("config reloaded");
            }
            if changed {
                ui.label(
                    RichText::new("(unsaved)")
                        .color(ui::WARN)
                        .monospace()
                        .size(10.5),
                );
            }
        });

        ui::section(ui, "naming");
        ui::kv(
            ui,
            "source",
            "windows dns client cache only - no lookups, no queries",
            ui::DIM,
        );
        ui::kv(
            ui,
            "cached names",
            &format!("{} addresses right now", snap.dns_entries),
            if snap.dns_error.is_some() {
                ui::WARN
            } else {
                ui::DIM
            },
        );
        ui::kv(
            ui,
            "remembered",
            &format!(
                "{} names kept after their cache entry expired",
                snap.names_remembered
            ),
            ui::DIM,
        );

        ui::section(ui, "storage");
        ui::kv(
            ui,
            "data dir",
            &self.paths.dir.display().to_string(),
            ui::DIM,
        );
        ui::kv(ui, "database", &self.db_size(), ui::DIM);
        ui::kv(
            ui,
            "config",
            &self.paths.config.display().to_string(),
            ui::DIM,
        );
        ui::kv(
            ui,
            "log",
            &format!("{} (2 MB x3)", self.paths.log.display()),
            ui::DIM,
        );

        ui::section(ui, "diagnostics");
        ui::kv(
            ui,
            "tick",
            &format!("{}  every {} ms", snap.tick, snap.interval_ms),
            ui::DIM,
        );
        ui::kv(
            ui,
            "sockets seen",
            &snap.connections_seen.to_string(),
            ui::DIM,
        );
        ui::kv(
            ui,
            "hotkeys",
            "Ctrl+Alt+1|2|3 modes, Ctrl+Alt+H show/hide",
            ui::DIM,
        );
        if let Some(note) = self.shared.lock().ok().and_then(|s| s.note.clone()) {
            ui::kv(ui, "collector note", &note, ui::DIM);
        }
        if let Some(err) = snap.errors.last() {
            ui::kv(ui, "last error", err, ui::ERR);
        }
    }

    fn db_size(&self) -> String {
        match std::fs::metadata(&self.paths.db) {
            Ok(m) => format!("{} ({} KiB)", self.paths.db.display(), m.len() / 1024),
            Err(_) => format!("{} (not created yet)", self.paths.db.display()),
        }
    }

    /// Always-on summary line: what the dots mean, and how much is going on.
    fn footer(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        self.status_strip(ui, snap);
        ui.add_space(1.0);
        ui.horizontal(|ui| {
            ui::legend(ui, crate::model::NameSource::Cache, "cache");
            ui::legend(ui, crate::model::NameSource::Remembered, "remembered");
            ui::legend(ui, crate::model::NameSource::None, "ip");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let named = snap.dests.iter().filter(|d| d.is_domain).count();
                let live = snap.dests.iter().filter(|d| d.live > 0).count();
                ui.label(
                    RichText::new(format!(
                        "{live} live  {named} named/{}  tick {}",
                        snap.dests.len(),
                        snap.tick
                    ))
                    .color(ui::DIM)
                    .monospace()
                    .size(10.0),
                );
            });
        });
    }

    fn status_strip(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let dead = self.collector_dead();
        let toast = self
            .toast
            .as_ref()
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(4))
            .map(|(m, _)| m.clone());
        if !dead && toast.is_none() && snap.errors.is_empty() {
            return;
        }
        ui.separator();
        if dead {
            ui.label(
                RichText::new("collector stopped - restart the app, see trafficdesk.log")
                    .color(ui::ERR)
                    .monospace()
                    .size(11.0),
            );
        }
        if let Some(msg) = toast {
            ui.label(RichText::new(msg).color(ui::TEXT).monospace().size(11.0));
        }
        for err in snap.errors.iter().take(2) {
            ui.label(
                RichText::new(format!("warn: {err}"))
                    .color(ui::WARN)
                    .monospace()
                    .size(10.5),
            );
        }
    }
}

/// Columns for a destination row. Shared with the header so they cannot drift.
fn row_cols(d: &DestAgg) -> Vec<(String, f32)> {
    vec![
        (fmt_count(d.events), 52.0),
        (format!("{}", d.live), 30.0),
        (fmt_ago(d.last_seen), 38.0),
    ]
}

fn header_cols() -> Vec<(String, f32)> {
    vec![
        ("ev".to_string(), 52.0),
        ("live".to_string(), 30.0),
        ("ago".to_string(), 38.0),
    ]
}

fn process_cols() -> Vec<(String, f32)> {
    vec![
        ("live".to_string(), 30.0),
        ("ev".to_string(), 48.0),
        ("dest".to_string(), 30.0),
        ("ago".to_string(), 38.0),
    ]
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(cmd) = self.rx.try_recv() {
            self.handle(cmd, ctx);
        }
        // The tray and hotkey threads hide and show the window themselves, so
        // mirror the real state instead of tracking it independently.
        self.visible = ffi::own_window_visible().unwrap_or(true);
        // winit re-applies its own window styles whenever the window is
        // resized, so re-assert ours with a short burst after every geometry
        // change and then once a second; two cheap user32 calls each time.
        if self.assert_burst > 0 {
            self.assert_burst -= 1;
            self.assert_tool_window();
        } else if self
            .last_tool_window_assert
            .map(|t| t.elapsed().as_secs() >= 1)
            .unwrap_or(true)
        {
            self.last_tool_window_assert = Some(std::time::Instant::now());
            self.assert_tool_window();
        }
        self.apply_geometry(ctx);
        let snap = self.snapshot();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(ui::BG)
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ctx, |ui| match self.mode {
                Mode::Mini => self.mini(ui, ctx, &snap),
                Mode::Panel => self.panel(ui, ctx, &snap),
                Mode::Expanded => self.expanded(ui, ctx, &snap),
            });
        // Nothing is visible while the panel is minimized, so only ask for
        // frames when it is on screen; the tray and hotkey threads wake us with
        // an explicit repaint request when they bring it back.
        if self.visible {
            ctx.request_repaint_after(Duration::from_millis(snap.interval_ms.clamp(250, 5000)));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop.store(true, Ordering::Relaxed);
        log::info!("TrafficDesk exiting");
    }
}
