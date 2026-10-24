//! Native desktop controls; the capture worker never runs on the UI thread.
use crate::{engine::Engine, model::*};
use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};
use std::time::{Duration, Instant};

const INK: Color32 = Color32::from_rgb(31, 43, 60);
const MUTED: Color32 = Color32::from_rgb(113, 125, 143);
const ACCENT: Color32 = Color32::from_rgb(47, 102, 236);
const LINE: Color32 = Color32::from_rgb(226, 231, 238);
const CANVAS: Color32 = Color32::from_rgb(245, 247, 250);
/// Right column height that is not the live preview image (card padding, title,
/// region line, metric cards and the source hint). Used to shrink the preview so
/// the column fits the window instead of scrolling the whole page.
const PREVIEW_CHROME: f32 = 320.0;

pub struct DesktopApp {
    config: Config,
    engine: Engine,
    monitors: Vec<Monitor>,
    adapters: Vec<crate::network_adapters::Adapter>,
    adapter_error: Option<String>,
    stats: Stats,
    error: Option<String>,
    dismissed_worker_error: Option<String>,
    notice: Option<(String, Instant)>,
    preview: Option<egui::TextureHandle>,
    preview_sequence: Option<u64>,
    snapshot: bool,
    compact: bool,
    dirty_at: Option<Instant>,
    pending_pick: bool,
    exiting: bool,
    hidden: bool,
    preview_enabled: bool,
    pub tray_available: bool,
    #[cfg(windows)]
    tray: Option<crate::tray::Tray>,
    #[cfg(windows)]
    picker: Option<std::sync::mpsc::Receiver<Result<Option<Region>, String>>>,
}

impl DesktopApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::initialize(&cc.egui_ctx, false, false)
    }

    /// Uses the real widget tree for headless visual verification. The display is
    /// explicitly marked as an example and there is no simulated telemetry.
    pub fn for_snapshot(ctx: &egui::Context, compact: bool) -> Self {
        Self::initialize(ctx, true, compact)
    }

    fn initialize(ctx: &egui::Context, snapshot: bool, compact: bool) -> Self {
        install_style(ctx);
        let (mut config, warning) = crate::settings::load();
        let mut error = warning;
        #[cfg(any(windows, target_os = "macos"))]
        let mut monitors = if snapshot {
            Vec::new()
        } else {
            #[cfg(windows)]
            let result = crate::capture::monitors();
            #[cfg(target_os = "macos")]
            let result = crate::capture_macos::monitors();
            match result {
                Ok(monitors) => monitors,
                Err(e) => {
                    error = Some(e);
                    Vec::new()
                }
            }
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        let mut monitors: Vec<Monitor> = Vec::new();
        if cfg!(target_os = "macos") && !snapshot {
            config.backend = Backend::Auto;
            config.unlimited = false;
            config.precision = false;
            config.close_to_tray = false;
        }
        if snapshot {
            config = Config::default();
            error = None;
            monitors = vec![Monitor {
                id: "preview-display".into(),
                name: "主显示器 · 界面预览".into(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
                refresh_hz: 240,
                primary: true,
            }];
        }
        if let Some(monitor) = monitors
            .iter()
            .find(|m| m.id == config.monitor)
            .or_else(|| monitors.iter().find(|m| m.primary))
            .or(monitors.first())
        {
            config.monitor = monitor.id.clone();
            // A center preset follows resolution changes even when the old rectangle still fits.
            // Explicit custom coordinates remain unchanged and are validated before sending.
            if let Some(size) = config.preset {
                match Region::centered(monitor, size) {
                    Ok(region) => config.region = region,
                    Err(e) => error = Some(e),
                }
            }
        }
        let engine = Engine::new();
        #[cfg(windows)]
        let tray = if snapshot {
            None
        } else {
            match crate::tray::Tray::new(ctx, engine.stop_token()) {
                Ok(tray) => Some(tray),
                Err(e) => {
                    error = Some(format!("托盘不可用：{e}。关闭窗口将退出程序。"));
                    None
                }
            }
        };
        #[cfg(windows)]
        let tray_available = tray.is_some();
        #[cfg(not(windows))]
        let tray_available = false;
        let (adapters, adapter_error) = if cfg!(windows) && !snapshot {
            match crate::network_adapters::enumerate() {
                Ok(adapters) => (adapters, None),
                Err(error) => (Vec::new(), Some(error)),
            }
        } else {
            (Vec::new(), None)
        };
        #[cfg(windows)]
        if !snapshot {
            error = error.or_else(crate::network::startup_error);
        }
        Self {
            config,
            engine,
            monitors,
            adapters,
            adapter_error,
            stats: Stats::default(),
            error,
            dismissed_worker_error: None,
            notice: None,
            preview: None,
            preview_sequence: None,
            snapshot,
            compact,
            dirty_at: None,
            pending_pick: false,
            exiting: false,
            hidden: false,
            preview_enabled: false,
            tray_available,
            #[cfg(windows)]
            tray,
            #[cfg(windows)]
            picker: None,
        }
    }

    pub fn draw(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        if crate::single_instance::take_activation(ctx) {
            self.restore_window(ctx);
        }
        #[cfg(windows)]
        loop {
            let action = self.tray.as_ref().and_then(|tray| tray.poll());
            match action {
                Some(crate::tray::TrayAction::Show) => self.restore_window(ctx),
                Some(crate::tray::TrayAction::Stop) => self.engine.request_stop(),
                Some(crate::tray::TrayAction::Exit) => self.request_exit(),
                None => break,
            }
        }
        self.handle_window(ctx);
        self.poll_picker(ctx);
        let minimized = ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        self.sync_preview(!self.hidden && !minimized && !self.pending_pick);
        self.stats = self.engine.snapshot();
        if let Some(frame) = self.engine.take_preview()
            && self.preview_sequence != Some(frame.sequence)
            && frame.width > 0
            && frame.height > 0
            && frame.rgba.len() == frame.width * frame.height * 4
        {
            let image =
                egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
            if let Some(texture) = self.preview.as_mut() {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.preview =
                    Some(ctx.load_texture("capture-preview", image, egui::TextureOptions::LINEAR));
            }
            self.preview_sequence = Some(frame.sequence);
        }
        if self
            .dirty_at
            .is_some_and(|time| time.elapsed() > Duration::from_millis(700))
        {
            self.save();
        }
        if self.hidden || minimized || self.pending_pick {
            if self.exiting || self.dirty_at.is_some() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            return;
        }
        let margin = if self.compact || ctx.content_rect().width() < 1000.0 {
            22
        } else {
            28
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(CANVAS).inner_margin(margin))
            .show(ctx, |ui| {
                self.header(ui);
                ui.add_space(22.0);
                let footer_height = if self.error_message().is_some() {
                    112.0
                } else {
                    76.0
                };
                let body_height = (ui.available_height() - footer_height).max(260.0);
                egui::ScrollArea::vertical()
                    .id_salt("desktop-body")
                    .max_height(body_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let wide = ui.available_width() >= 800.0;
                        if wide {
                            // Both columns fill the visible height: an oversized settings
                            // list scrolls inside its own card, while the preview column
                            // absorbs the difference so the page itself never scrolls.
                            ui.horizontal_top(|ui| {
                                ui.allocate_ui_with_layout(
                                    Vec2::new(328.0, body_height),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| self.settings_card(ui, Some(body_height)),
                                );
                                ui.add_space(18.0);
                                let right = ui.available_width();
                                ui.allocate_ui_with_layout(
                                    Vec2::new(right, body_height),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| self.preview_column(ui, Some(body_height)),
                                );
                            });
                        } else {
                            self.settings_card(ui, None);
                            ui.add_space(18.0);
                            self.preview_column(ui, None);
                        }
                    });
                ui.add_space(16.0);
                self.footer(ui);
            });
        self.sync_preview(!self.hidden && !minimized && !self.pending_pick);
        if self.engine.is_running() || self.dirty_at.is_some() || self.pending_pick || self.exiting
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(42.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 12.0, ACCENT);
            let center = rect.center();
            for (offset, height) in [(-9.0, 12.0), (-3.0, 22.0), (3.0, 16.0), (9.0, 8.0)] {
                ui.painter().line_segment(
                    [
                        center + egui::vec2(offset, -height / 2.0),
                        center + egui::vec2(offset, height / 2.0),
                    ],
                    Stroke::new(2.6, Color32::WHITE),
                );
            }
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("FrameLink 帧连")
                            .size(23.0)
                            .strong()
                            .color(INK),
                    );
                    egui::Frame::new()
                        .fill(Color32::from_rgb(235, 239, 247))
                        .corner_radius(20.0)
                        .inner_margin(egui::Margin::symmetric(9, 4))
                        .show(ui, |ui| {
                            ui.hyperlink_to(
                                RichText::new("NDI®").size(10.5).color(MUTED),
                                "https://ndi.video/",
                            )
                            .on_hover_text("NDI® is a registered trademark of Vizrt NDI AB");
                        });
                });
                ui.label(
                    RichText::new("低延迟屏幕传输，连接远端分析")
                        .size(12.5)
                        .color(MUTED),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(env!("CARGO_PKG_VERSION"))
                        .size(12.0)
                        .color(MUTED),
                );
                ui.add_space(8.0);
                let (label, bg, fg, dot) = match self.stats.phase {
                    Phase::Sending => (
                        "正在发送",
                        Color32::from_rgb(231, 247, 239),
                        Color32::from_rgb(35, 142, 97),
                        true,
                    ),
                    Phase::Starting => ("正在启动", Color32::from_rgb(234, 240, 255), ACCENT, true),
                    Phase::Recovering => (
                        "重新连接",
                        Color32::from_rgb(255, 244, 219),
                        Color32::from_rgb(163, 116, 21),
                        true,
                    ),
                    Phase::Stopping => ("正在停止", Color32::from_rgb(235, 239, 245), MUTED, true),
                    Phase::Error => (
                        "需要处理",
                        Color32::from_rgb(255, 235, 234),
                        Color32::from_rgb(191, 64, 62),
                        true,
                    ),
                    Phase::Idle => (
                        if self.snapshot {
                            "界面预览"
                        } else {
                            "准备就绪"
                        },
                        Color32::from_rgb(234, 238, 244),
                        MUTED,
                        true,
                    ),
                };
                pill(ui, label, bg, fg, dot);
            });
        });
    }

    /// `height_budget` caps the card height so a long settings list scrolls inside
    /// the card instead of scrolling the whole window.
    fn settings_card(&mut self, ui: &mut egui::Ui, height_budget: Option<f32>) {
        let before = toml::to_string(&self.config).unwrap_or_default();
        let running = self.engine.is_running();
        let macos_capture = self.is_macos_capture();
        let compact = self.compact || ui.ctx().content_rect().width() < 1100.0;
        let pad = if compact { 16.0 } else { 20.0 };
        card().inner_margin(pad).show(ui, |ui| {
            let mut area = egui::ScrollArea::vertical()
                .id_salt("settings-scroll")
                .auto_shrink([false, true]);
            if let Some(budget) = height_budget {
                area = area.max_height((budget - pad * 2.0).max(180.0));
            }
            area.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = if compact { 0.0 } else { 3.0 };
                ui.set_min_width((ui.available_width()).min(284.0));
                ui.horizontal(|ui| {
                    ui.label(RichText::new("采集设置").size(17.0).strong().color(INK));
                });
                ui.add_space(if compact { 12.0 } else { 17.0 });
                ui.add_enabled_ui(!running, |ui| {
                    field_label(ui, "显示器");
                    let current = self
                        .selected_monitor()
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| "未找到显示器".into());
                    let mut selected = self.config.monitor.clone();
                    egui::ComboBox::from_id_salt("monitor")
                        .selected_text(current)
                        .width(ui.available_width() - 8.0)
                        .height(160.0)
                        .show_ui(ui, |ui| {
                            for monitor in &self.monitors {
                                ui.selectable_value(
                                    &mut selected,
                                    monitor.id.clone(),
                                    &monitor.name,
                                );
                            }
                        });
                    if selected != self.config.monitor
                        && let Some(monitor) = self.monitors.iter().find(|m| m.id == selected)
                        && let Err(error) = self.config.select_monitor(monitor)
                    {
                        self.error = Some(error);
                    }
                    if let Some(monitor) = self.selected_monitor() {
                        ui.add_space(4.0);
                        let dimensions = format!("{} × {}", monitor.width, monitor.height);
                        ui.label(
                            RichText::new(if monitor.refresh_hz > 0 {
                                format!("{dimensions}  ·  {} Hz", monitor.refresh_hz)
                            } else {
                                dimensions
                            })
                            .size(11.5)
                            .color(MUTED),
                        );
                    }
                    ui.add_space(14.0);
                    field_label(ui, "NDI 源名称");
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 38.0).max(80.0);
                        ui.add_sized(
                            [width, 32.0],
                            egui::TextEdit::singleline(&mut self.config.name)
                                .hint_text("接收端显示的名称")
                                .margin(egui::vec2(9.0, 7.0)),
                        );
                        if icon_button(ui, Icon::Copy, "复制源名称").clicked() {
                            ui.ctx().copy_text(self.config.name.clone());
                            self.notice("源名称已复制");
                        }
                    });
                    ui.add_space(if compact { 12.0 } else { 17.0 });
                    divider(ui);
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        field_label(ui, "采集区域");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new("以屏幕中心为起点").size(10.5).color(MUTED));
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        for size in [320, 416, 512, 640] {
                            if choice(
                                ui,
                                &size.to_string(),
                                self.config.preset == Some(size),
                                48.0,
                            )
                            .clicked()
                            {
                                self.apply_preset(size);
                            }
                        }
                        if choice(ui, "自定义", self.config.preset.is_none(), 61.0).clicked() {
                            self.config.preset = None;
                        }
                    });
                    ui.add_space(10.0);
                    egui::Frame::new()
                        .fill(Color32::from_rgb(247, 249, 252))
                        .corner_radius(8.0)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                draw_icon(ui, Icon::Crop, 18.0, MUTED);
                                ui.add_space(5.0);
                                ui.label(
                                    RichText::new(format!(
                                        "{} × {}",
                                        self.config.region.width, self.config.region.height
                                    ))
                                    .size(19.0)
                                    .strong()
                                    .color(INK),
                                );
                                ui.label(RichText::new("px").size(11.0).color(MUTED));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("居中").size(11.5).color(ACCENT),
                                                )
                                                .frame(false),
                                            )
                                            .clicked()
                                        {
                                            self.center_region();
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(7.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled_ui(cfg!(windows) || self.snapshot, |ui| {
                                ui.add_sized(
                                    [ui.available_width(), 29.0],
                                    egui::Button::new(RichText::new("在屏幕上框选区域").size(12.0))
                                        .fill(Color32::WHITE)
                                        .stroke(Stroke::new(1.0, LINE))
                                        .corner_radius(6.0),
                                )
                            })
                            .inner
                            .on_disabled_hover_text("请使用尺寸预设，或在下方输入屏幕像素坐标。")
                            .clicked()
                        {
                            #[cfg(windows)]
                            {
                                self.begin_pick(ui.ctx());
                            }
                            #[cfg(not(windows))]
                            {
                                self.error = Some("请使用尺寸预设或手动输入坐标。".into());
                            }
                        }
                    });
                    egui::CollapsingHeader::new(
                        RichText::new("精确位置与尺寸").size(11.5).color(MUTED),
                    )
                    .id_salt("region-details")
                    .show(ui, |ui| {
                        egui::Grid::new("region-grid")
                            .num_columns(4)
                            .spacing([8.0, 7.0])
                            .show(ui, |ui| {
                                ui.label("X");
                                if ui
                                    .add(egui::DragValue::new(&mut self.config.region.x).speed(1.0))
                                    .changed()
                                {
                                    self.config.preset = None;
                                }
                                ui.label("Y");
                                if ui
                                    .add(egui::DragValue::new(&mut self.config.region.y).speed(1.0))
                                    .changed()
                                {
                                    self.config.preset = None;
                                }
                                ui.end_row();
                                ui.label("宽");
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut self.config.region.width)
                                            .speed(2.0)
                                            .range(2..=32768),
                                    )
                                    .changed()
                                {
                                    self.config.preset = None;
                                }
                                ui.label("高");
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut self.config.region.height)
                                            .speed(2.0)
                                            .range(2..=32768),
                                    )
                                    .changed()
                                {
                                    self.config.preset = None;
                                }
                                ui.end_row();
                            });
                    });
                    ui.add_space(11.0);
                    divider(ui);
                    ui.add_space(13.0);
                    field_label(ui, "发送帧率");
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            !self.config.unlimited,
                            egui::DragValue::new(&mut self.config.fps)
                                .range(1..=1000)
                                .speed(1.0)
                                .suffix(" fps"),
                        );
                        if !macos_capture {
                            ui.add_space(15.0);
                            ui.checkbox(
                                &mut self.config.unlimited,
                                RichText::new("跟随刷新率").size(12.0),
                            )
                            .on_hover_text(
                                "不使用上方帧率：屏幕每更新一次就发一帧，不补帧、不重复。",
                            );
                        }
                    });
                    ui.add_space(5.0);
                    ui.label(
                        RichText::new(if macos_capture {
                            "帧率上限，实际采集取决于屏幕更新"
                        } else if self.config.unlimited {
                            "跟随屏幕更新：画面变化多少就发多少"
                        } else {
                            "固定帧率：高于屏幕更新会产生重复帧"
                        })
                        .size(11.0)
                        .color(MUTED),
                    );
                });
                if !macos_capture {
                    ui.add_space(12.0);
                    divider(ui);
                    ui.add_space(10.0);
                    ui.add_enabled_ui(!running && !self.exiting, |ui| self.network_controls(ui));
                }
                ui.add_space(13.0);
                divider(ui);
                ui.add_space(5.0);
                egui::CollapsingHeader::new(RichText::new("高级设置").size(12.0).color(MUTED))
                    .id_salt("advanced-options")
                    .show(ui, |ui| {
                        ui.add_enabled_ui(!running, |ui| {
                            egui::Grid::new("advanced-grid")
                                .num_columns(2)
                                .spacing([12.0, 9.0])
                                .show(ui, |ui| {
                                    ui.label("采集方式");
                                    if macos_capture {
                                        ui.label("ScreenCaptureKit");
                                    } else {
                                        egui::ComboBox::from_id_salt("backend")
                                            .width(145.0)
                                            .selected_text(backend_name(self.config.backend))
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut self.config.backend,
                                                    Backend::Auto,
                                                    "自动选择（推荐）",
                                                );
                                                ui.selectable_value(
                                                    &mut self.config.backend,
                                                    Backend::Dxgi,
                                                    "DXGI 高速采集",
                                                );
                                                ui.selectable_value(
                                                    &mut self.config.backend,
                                                    Backend::Gdi,
                                                    "GDI 兼容采集",
                                                );
                                            });
                                    }
                                    ui.end_row();
                                    ui.label("视频格式");
                                    egui::ComboBox::from_id_salt("pixel")
                                        .width(145.0)
                                        .selected_text(pixel_name(self.config.pixel))
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut self.config.pixel,
                                                PixelFormat::Nv12,
                                                "NV12（推荐）",
                                            );
                                            ui.selectable_value(
                                                &mut self.config.pixel,
                                                PixelFormat::Bgrx,
                                                "BGRX",
                                            );
                                        });
                                    ui.end_row();
                                });
                            ui.add_space(8.0);
                            ui.checkbox(&mut self.config.cursor, "包含鼠标指针");
                            if !macos_capture {
                                ui.checkbox(&mut self.config.precision, "精确帧间调度")
                                    .on_hover_text(
                                        "提高调度精度，但会增加 CPU 占用。建议先使用默认设置。",
                                    );
                            }
                        });
                        if !macos_capture {
                            ui.checkbox(&mut self.config.close_to_tray, "关闭窗口时收起到托盘");
                        }
                    });
            });
        });
        if toml::to_string(&self.config).unwrap_or_default() != before {
            self.dirty_at = Some(Instant::now());
        }
    }

    fn network_controls(&mut self, ui: &mut egui::Ui) {
        let selected = self
            .adapters
            .iter()
            .find(|a| a.id == self.config.network_adapter);
        let name = if self.config.network_adapter.is_empty() {
            "自动选择".to_owned()
        } else {
            selected.map_or_else(|| "已选网卡不可用".into(), |a| a.name.clone())
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("发送网卡").size(11.5).color(INK));
            egui::ComboBox::from_id_salt("network-adapter")
                .width((ui.available_width() - 45.0).max(100.0))
                .truncate()
                .selected_text(name)
                .show_ui(ui, |ui| {
                ui.set_max_width(390.0);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                ui.selectable_value(&mut self.config.network_adapter, String::new(), "自动选择");
                for adapter in &self.adapters {
                    let available = adapter.connected && !adapter.ipv4.is_empty();
                    let text = if available {
                        format!("{} · {}", adapter.name, adapter.ipv4[0])
                    } else {
                        format!("{} · 未连接 / 无 IPv4", adapter.name)
                    };
                    ui.add_enabled_ui(available, |ui| {
                        ui.selectable_value(
                            &mut self.config.network_adapter,
                            adapter.id.clone(),
                            text,
                        );
                    });
                }
                })
                .response
                .on_hover_text("自动选择沿用现有 NDI 设置；指定网卡后需重启应用。仅影响 FrameLink，不修改系统网卡设置。");
            if ui.small_button("刷新").clicked() && !self.snapshot {
                match crate::network_adapters::enumerate() {
                    Ok(adapters) => {
                        self.adapters = adapters;
                        self.adapter_error = None;
                    }
                    Err(error) => self.adapter_error = Some(error),
                }
            }
        });
        let selected = self
            .adapters
            .iter()
            .find(|a| a.id == self.config.network_adapter);
        let detail = if self.config.network_adapter.is_empty() {
            String::new()
        } else if let Some(adapter) = selected {
            if adapter.connected && !adapter.ipv4.is_empty() {
                let ips = adapter
                    .ipv4
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" / ");
                let speed = if adapter.link_speed_bps == 0 {
                    "速率未知".to_owned()
                } else if adapter.link_speed_bps >= 1_000_000_000 {
                    format!(
                        "{:.1} Gbps",
                        adapter.link_speed_bps as f64 / 1_000_000_000.0
                    )
                } else {
                    format!("{} Mbps", adapter.link_speed_bps / 1_000_000)
                };
                format!("{ips} · {speed}")
            } else {
                "网卡未连接，或没有可用 IPv4 地址".into()
            }
        } else {
            "请连接网卡后刷新，或选择其他网卡".into()
        };
        if !detail.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(detail).size(11.0).color(MUTED))
                .on_hover_text("指定网卡仅作用于 FrameLink 的 NDI 主视频数据。连接速率不等于实际吞吐量；不会修改系统网卡设置。");
        }
        if let Some(error) = &self.adapter_error {
            ui.label(
                RichText::new(error)
                    .size(11.0)
                    .color(Color32::from_rgb(181, 58, 64)),
            );
        }
        #[cfg(windows)]
        if !self.snapshot
            && crate::network::restart_required(&self.config.network_adapter, &self.adapters)
        {
            ui.horizontal(|ui| {
                ui.label(RichText::new("重启后生效").size(11.0).color(MUTED));
                if ui.small_button("应用并重启").clicked() {
                    match crate::settings::save(&self.config) {
                        Ok(()) => {
                            self.dirty_at = None;
                            crate::network::request_restart();
                            self.request_exit();
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            });
        }
    }

    fn preview_column(&mut self, ui: &mut egui::Ui, height_budget: Option<f32>) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("采集预览").size(17.0).strong().color(INK));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let response = ui.checkbox(
                        &mut self.config.preview,
                        RichText::new("开启预览").size(12.0).color(MUTED),
                    );
                    if response.changed() {
                        self.dirty_at = Some(Instant::now());
                        if !self.config.preview {
                            self.preview = None;
                        }
                    }
                });
            });
            ui.add_space(14.0);
            let preferred = if self.compact || ui.ctx().content_rect().width() < 1100.0 {
                200.0
            } else {
                (ui.ctx().content_rect().height() * 0.33).clamp(230.0, 280.0)
            };
            // Shrink the image area rather than overflowing the column: only when the
            // window is too short for the minimum preview does the page scroll.
            let height = match height_budget {
                Some(budget) => preferred.min((budget - PREVIEW_CHROME).max(150.0)),
                None => preferred,
            };
            let (rect, _) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), height),
                egui::Sense::hover(),
            );
            ui.painter()
                .rect_filled(rect, 10.0, Color32::from_rgb(244, 247, 252));
            if self.config.preview
                && self.engine.is_running()
                && let Some(texture) = self.preview.as_ref()
            {
                let size = texture.size_vec2();
                let scale = ((rect.width() - 20.0) / size.x).min((rect.height() - 20.0) / size.y);
                let image_rect = egui::Rect::from_center_size(rect.center(), size * scale);
                ui.painter().image(
                    texture.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                self.draw_placeholder(ui, rect);
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                draw_icon(ui, Icon::Crop, 14.0, MUTED);
                ui.label(
                    RichText::new(format!(
                        "{} × {} px",
                        self.config.region.width, self.config.region.height
                    ))
                    .size(12.0)
                    .color(INK),
                );
                ui.add_space(5.0);
                ui.label(RichText::new("·").color(MUTED));
                ui.label(
                    RichText::new(if self.config.preset.is_some() {
                        "中心区域"
                    } else {
                        "自定义区域"
                    })
                    .size(12.0)
                    .color(MUTED),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(if self.config.preview {
                            "预览按需刷新"
                        } else {
                            "预览关闭时节省资源"
                        })
                        .size(11.0)
                        .color(MUTED),
                    );
                });
            });
        });
        ui.add_space(16.0);
        let active = self.engine.is_running() && self.stats.frames > 0;
        let width = ((ui.available_width() - 24.0) / 3.0).max(70.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            metric(
                ui,
                width,
                "实际发送",
                if active {
                    format!("{:.0}", self.stats.fps)
                } else {
                    "—".into()
                },
                "fps",
                "每秒提交的视频帧",
                None,
            );
            metric(
                ui,
                width,
                "接收设备",
                if active {
                    self.stats.receivers.max(0).to_string()
                } else {
                    "—".into()
                },
                "台",
                "NDI 已连接的接收端",
                None,
            );
            let cost = self.stats.capture_ms + self.stats.conversion_ms + self.stats.submit_ms;
            metric(
                ui,
                width,
                "发送端处理",
                if active {
                    format!("{:.2}", cost)
                } else {
                    "—".into()
                },
                "ms",
                "不包含网络与接收端耗时",
                Some("采集、颜色转换与 NDI 提交的合计耗时。此数值不是端到端延迟。"),
            );
        });
        ui.add_space(15.0);
        let source_hint = egui::Frame::new()
            .fill(Color32::from_rgb(235, 241, 254))
            .corner_radius(9.0)
            .inner_margin(13)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    draw_icon(ui, Icon::Link, 18.0, ACCENT);
                    ui.add_space(5.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(
                            RichText::new("让远端接收软件找到这个画面")
                                .size(12.0)
                                .strong()
                                .color(INK),
                        );
                        ui.add_space(3.0);
                        ui.label(
                            RichText::new(format!(
                                "在同一局域网内，选择源「{}」 · 点击复制",
                                self.config.name
                            ))
                            .size(11.0)
                            .color(MUTED),
                        );
                    });
                });
            })
            .response;
        if source_hint
            .interact(egui::Sense::click())
            .on_hover_text("复制 NDI 源名称")
            .clicked()
        {
            ui.ctx().copy_text(self.config.name.clone());
            self.notice("源名称已复制");
        }
    }

    fn draw_placeholder(&self, ui: &egui::Ui, rect: egui::Rect) {
        let p = ui.painter();
        let dots = Color32::from_rgb(217, 226, 239);
        let mut y = rect.top() + 15.0;
        while y < rect.bottom() {
            let mut x = rect.left() + 15.0;
            while x < rect.right() {
                p.circle_filled(egui::pos2(x, y), 0.85, dots);
                x += 20.0;
            }
            y += 20.0;
        }
        let screen_width = (rect.width() * 0.61)
            .min(300.0)
            .min((rect.height() - 105.0).max(65.0) / 0.53);
        let screen = egui::Rect::from_center_size(
            rect.center() - egui::vec2(0.0, 28.0),
            egui::vec2(screen_width, screen_width * 0.53),
        );
        p.rect_filled(screen, 8.0, Color32::from_rgb(234, 239, 248));
        p.rect_stroke(
            screen,
            8.0,
            Stroke::new(1.3, Color32::from_rgb(203, 214, 233)),
            egui::StrokeKind::Inside,
        );
        let monitor = self.selected_monitor();
        let mw = monitor.map_or(2560.0, |m| m.width as f32);
        let mh = monitor.map_or(1440.0, |m| m.height as f32);
        let crop_w = (screen.width() * self.config.region.width as f32 / mw)
            .clamp(42.0, screen.width() - 8.0);
        let crop_h = (screen.height() * self.config.region.height as f32 / mh)
            .clamp(42.0, screen.height() - 8.0);
        let mut center = screen.center();
        if let Some(monitor) = monitor {
            center.x = screen.left()
                + (self.config.region.x as f32 - monitor.x as f32
                    + self.config.region.width as f32 / 2.0)
                    / mw
                    * screen.width();
            center.y = screen.top()
                + (self.config.region.y as f32 - monitor.y as f32
                    + self.config.region.height as f32 / 2.0)
                    / mh
                    * screen.height();
        }
        let crop = egui::Rect::from_center_size(center, egui::vec2(crop_w, crop_h))
            .intersect(screen.shrink(4.0));
        p.rect_filled(crop, 3.0, Color32::from_rgb(215, 228, 255));
        p.rect_stroke(
            crop,
            3.0,
            Stroke::new(1.4, ACCENT),
            egui::StrokeKind::Inside,
        );
        for corner in [
            crop.left_top(),
            crop.right_top(),
            crop.left_bottom(),
            crop.right_bottom(),
        ] {
            p.circle_filled(corner, 2.5, ACCENT);
        }
        let stem_top = egui::pos2(screen.center().x, screen.bottom());
        p.line_segment(
            [stem_top, stem_top + egui::vec2(0.0, 11.0)],
            Stroke::new(2.0, Color32::from_rgb(203, 214, 233)),
        );
        p.line_segment(
            [
                stem_top + egui::vec2(-17.0, 12.0),
                stem_top + egui::vec2(17.0, 12.0),
            ],
            Stroke::new(2.0, Color32::from_rgb(203, 214, 233)),
        );
        p.text(
            egui::pos2(rect.center().x, screen.top() - 20.0),
            egui::Align2::CENTER_CENTER,
            "区域示意",
            FontId::proportional(10.5),
            MUTED,
        );
        let title = if self.config.preview {
            "开始发送后显示实时预览"
        } else {
            "专注传输，预览已关闭"
        };
        p.text(
            egui::pos2(rect.center().x, screen.bottom() + 48.0),
            egui::Align2::CENTER_CENTER,
            title,
            FontId::proportional(14.0),
            INK,
        );
        p.text(
            egui::pos2(rect.center().x, screen.bottom() + 70.0),
            egui::Align2::CENTER_CENTER,
            "仅发送选定区域 · 始终优先处理最新画面",
            FontId::proportional(11.0),
            MUTED,
        );
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = self.error_message() {
            egui::Frame::new()
                .fill(Color32::from_rgb(255, 237, 234))
                .corner_radius(7.0)
                .inner_margin(9)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(error)
                                .size(11.5)
                                .color(Color32::from_rgb(174, 62, 55)),
                        );
                        if ui.small_button("清除提示").clicked() {
                            self.error = None;
                            self.dismissed_worker_error = self.stats.error.clone();
                        }
                    });
                });
            ui.add_space(9.0);
        }
        ui.horizontal(|ui| {
            let running = self.engine.is_running();
            let busy = matches!(self.stats.phase, Phase::Starting | Phase::Stopping);
            let text = if self.exiting {
                "正在退出…"
            } else if self.stats.phase == Phase::Stopping {
                "正在停止…"
            } else if running {
                "停止发送"
            } else {
                "开始发送"
            };
            let color = if running {
                Color32::from_rgb(222, 79, 77)
            } else {
                ACCENT
            };
            let button = egui::Button::new(
                RichText::new(text)
                    .size(15.0)
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(color)
            .stroke(Stroke::NONE)
            .corner_radius(9.0);
            if ui
                .add_enabled_ui(!busy && !self.exiting, |ui| {
                    ui.add_sized([220.0, 46.0], button)
                })
                .inner
                .clicked()
            {
                if running {
                    self.engine.request_stop();
                } else {
                    self.start();
                }
            }
            if let Some((message, _)) = self
                .notice
                .as_ref()
                .filter(|(_, at)| at.elapsed() < Duration::from_secs(3))
            {
                ui.add_space(12.0);
                ui.label(RichText::new(message).size(11.0).color(MUTED));
            }
            if !self.is_macos_capture() && ui.available_width() > 140.0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            self.tray_available,
                            egui::Button::new(RichText::new("收起到托盘").size(12.0).color(MUTED))
                                .min_size(egui::vec2(112.0, 38.0))
                                .fill(Color32::WHITE)
                                .stroke(Stroke::new(1.0, LINE))
                                .corner_radius(8.0),
                        )
                        .on_hover_text(if self.tray_available {
                            "窗口隐藏后，发送会继续；双击托盘图标恢复，右键打开菜单。"
                        } else {
                            "托盘仅在 Windows 运行时可用。"
                        })
                        .clicked()
                    {
                        self.hide_to_tray(ui.ctx());
                    }
                });
            }
        });
    }

    fn selected_monitor(&self) -> Option<&Monitor> {
        self.monitors
            .iter()
            .find(|m| m.id == self.config.monitor)
            .or(self.monitors.first())
    }
    fn is_macos_capture(&self) -> bool {
        cfg!(target_os = "macos") && !self.snapshot
    }
    fn apply_preset(&mut self, size: u32) {
        if let Some(monitor) = self.selected_monitor() {
            match Region::centered(monitor, size) {
                Ok(region) => {
                    self.config.region = region;
                    self.config.preset = Some(size);
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
        } else {
            self.config.region.width = size;
            self.config.region.height = size;
            self.config.preset = Some(size);
        }
    }
    fn center_region(&mut self) {
        if let Some(monitor) = self.selected_monitor().cloned() {
            let (width, height) = (self.config.region.width, self.config.region.height);
            if width <= monitor.width && height <= monitor.height {
                self.config.region.x = monitor.x + ((monitor.width - width) / 2) as i32;
                self.config.region.y = monitor.y + ((monitor.height - height) / 2) as i32;
            } else {
                self.error = Some("区域大于显示器，请先缩小宽度和高度。".into());
            }
        }
    }
    fn start(&mut self) {
        self.error = None;
        self.dismissed_worker_error = None;
        if let Err(error) = self.config.validate() {
            self.error = Some(error);
            return;
        }
        if self.is_macos_capture() {
            let Some(monitor) = self.selected_monitor() else {
                self.error = Some("没有可用的显示器，请检查显示器连接后重新打开程序。".into());
                return;
            };
            if !self.config.region.fits(monitor) {
                self.error = Some("采集区域需位于所选显示器内。".into());
                return;
            }
        }
        #[cfg(target_os = "macos")]
        if !crate::capture_macos::has_permission() && !crate::capture_macos::request_permission() {
            self.error = Some(
                "请在系统设置 → 隐私与安全性 → 屏幕与系统音频录制中允许 FrameLink，授权后退出并重新打开程序。".into(),
            );
            return;
        }
        if self.config.backend == Backend::Dxgi
            && !self.monitors.is_empty()
            && !self
                .monitors
                .iter()
                .any(|monitor| self.config.region.fits(monitor))
        {
            self.error = Some("DXGI区域需在单台显示器内；跨屏区域可用自动或GDI采集。".into());
            return;
        }
        self.save();
        self.preview = None;
        self.preview_sequence = None;
        if let Err(error) = self.engine.start(self.config.clone()) {
            self.error = Some(error);
            #[cfg(windows)]
            if !self.config.network_adapter.is_empty()
                && let Ok(adapters) = crate::network_adapters::enumerate()
            {
                // Refresh the restart action after a disconnect or DHCP change is detected.
                self.adapters = adapters;
            }
        }
    }
    fn save(&mut self) {
        self.dirty_at = None;
        if !self.snapshot
            && let Err(error) = crate::settings::save(&self.config)
        {
            self.error = Some(error);
        }
    }
    fn notice(&mut self, text: &str) {
        self.notice = Some((text.into(), Instant::now()));
    }
    fn error_message(&self) -> Option<String> {
        self.error.clone().or_else(|| {
            self.stats
                .error
                .clone()
                .filter(|message| Some(message) != self.dismissed_worker_error.as_ref())
        })
    }

    #[cfg(windows)]
    fn begin_pick(&mut self, ctx: &egui::Context) {
        if self.pending_pick || self.engine.is_running() {
            return;
        }
        let Some(monitor) = self.selected_monitor().cloned() else {
            self.error = Some("没有可用的显示器，请检查显示器连接后重新打开程序。".into());
            return;
        };
        let (send, receive) = std::sync::mpsc::channel();
        self.pending_pick = true;
        self.picker = Some(receive);
        self.sync_preview(false);
        crate::tray::set_window_visible(ctx, false);
        let worker_ctx = ctx.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("region-picker".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| crate::capture::pick_region(&monitor))
                    .unwrap_or_else(|_| Err("区域选择意外中断，请重试。".into()));
                let _ = send.send(result);
                // A hidden native window may not redraw until it is restored by Win32.
                crate::tray::set_window_visible(&worker_ctx, true);
                worker_ctx.request_repaint();
            })
        {
            self.pending_pick = false;
            self.picker = None;
            self.error = Some(format!("无法启动区域选择：{error}"));
            crate::tray::set_window_visible(ctx, true);
        }
    }

    fn poll_picker(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            let Some(receiver) = self.picker.as_ref() else {
                return;
            };
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("区域选择已中断，请重试。".into())
                }
            };
            self.picker = None;
            self.pending_pick = false;
            match result {
                Ok(Some(region)) => {
                    self.config.region = region;
                    self.config.preset = None;
                    self.dirty_at = Some(Instant::now());
                    self.error = None;
                }
                Ok(None) => {}
                Err(error) => self.error = Some(error),
            }
            crate::tray::set_window_visible(ctx, true);
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        #[cfg(not(windows))]
        let _ = ctx;
    }

    pub fn request_exit(&mut self) {
        self.exiting = true;
        self.engine.request_stop();
        self.save();
    }
    fn sync_preview(&mut self, visible: bool) {
        let desired = self.config.preview && visible && !self.exiting;
        if desired != self.preview_enabled {
            self.engine.set_preview(desired);
            self.preview_enabled = desired;
        }
    }
    pub fn restore_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        self.sync_preview(true);
        crate::tray::set_window_visible(ctx, true);
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
    pub fn hide_to_tray(&mut self, ctx: &egui::Context) {
        if self.tray_available {
            self.hidden = true;
            self.sync_preview(false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }
    pub fn is_sending(&self) -> bool {
        self.engine.is_running()
    }
    fn handle_window(&mut self, ctx: &egui::Context) {
        if self.exiting && !self.engine.is_running() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if self.exiting && !self.engine.is_running() {
                return;
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.config.close_to_tray && self.tray_available && !self.exiting {
                self.hide_to_tray(ctx);
            } else {
                self.request_exit();
            }
        }
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.draw(ctx);
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save();
        self.engine.shutdown();
    }
}

fn install_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/msyh.ttf",
        "C:/Windows/Fonts/simhei.ttf",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    ];
    if let Some(bytes) = candidates
        .into_iter()
        .find_map(|path| std::fs::read(path).ok())
    {
        fonts.font_data.insert(
            "chinese-ui".into(),
            egui::FontData::from_owned(bytes).into(),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("chinese-ui".into());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("chinese-ui".into());
    }
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.panel_fill = CANVAS;
    style.visuals.window_fill = Color32::WHITE;
    style.visuals.selection.bg_fill = Color32::from_rgb(220, 232, 255);
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.inactive.bg_fill = Color32::WHITE;
    style.visuals.widgets.inactive.weak_bg_fill = Color32::WHITE;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(239, 244, 255);
    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(239, 244, 255);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(155, 183, 245));
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(225, 235, 255);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(9.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    style.spacing.scroll.floating = true;
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating_width = 3.0;
    style.spacing.scroll.floating_allocated_width = 8.0;
    style.spacing.scroll.dormant_handle_opacity = 0.3;
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(12.5));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(12.5));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(10.5));
    ctx.set_style(style);
}

fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(12.0)
        .inner_margin(20)
}
fn field_label(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).size(12.0).strong().color(INK));
}
fn divider(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        Stroke::new(1.0, Color32::from_rgb(236, 239, 244)),
    );
}
fn choice(ui: &mut egui::Ui, text: &str, selected: bool, width: f32) -> egui::Response {
    ui.add_sized(
        [width, 30.0],
        egui::Button::new(RichText::new(text).size(12.0).color(if selected {
            ACCENT
        } else {
            MUTED
        }))
        .fill(if selected {
            Color32::from_rgb(234, 240, 255)
        } else {
            Color32::WHITE
        })
        .stroke(Stroke::new(
            1.0,
            if selected {
                Color32::from_rgb(163, 190, 250)
            } else {
                LINE
            },
        ))
        .corner_radius(6.0),
    )
}
fn pill(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32, dot: bool) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(20.0)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if dot {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(6.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 2.5, fg);
                }
                ui.label(RichText::new(text).size(10.5).color(fg));
            });
        });
}
fn metric(
    ui: &mut egui::Ui,
    width: f32,
    label: &str,
    value: String,
    unit: &str,
    detail: &str,
    tooltip: Option<&str>,
) {
    card().inner_margin(15).show(ui, |ui| {
        ui.set_width((width - 32.0).max(40.0));
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let response = ui.label(RichText::new(label).size(11.5).color(MUTED));
            if let Some(tooltip) = tooltip {
                response.on_hover_text(tooltip);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(value).size(27.0).strong().color(INK));
                ui.label(RichText::new(unit).size(10.5).color(MUTED));
            });
            ui.add_space(5.0);
            ui.label(RichText::new(detail).size(9.5).color(MUTED));
        });
    });
}
fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Auto => "自动选择（推荐）",
        Backend::Dxgi => "DXGI 高速采集",
        Backend::Gdi => "GDI 兼容采集",
    }
}
fn pixel_name(pixel: PixelFormat) -> &'static str {
    match pixel {
        PixelFormat::Nv12 => "NV12（推荐）",
        PixelFormat::Bgrx => "BGRX",
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Copy,
    Crop,
    Link,
}
fn icon_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(31.0, 31.0), egui::Sense::click());
    ui.painter().rect_filled(
        rect,
        6.0,
        if response.hovered() {
            Color32::from_rgb(236, 242, 255)
        } else {
            Color32::from_rgb(246, 248, 251)
        },
    );
    paint_icon(ui.painter(), rect.center(), icon, 15.0, MUTED);
    response.on_hover_text(tooltip)
}
fn draw_icon(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    paint_icon(ui.painter(), rect.center(), icon, size, color);
}
fn paint_icon(p: &egui::Painter, center: egui::Pos2, icon: Icon, size: f32, color: Color32) {
    let stroke = Stroke::new(1.3, color);
    let s = size / 16.0;
    let at = |x: f32, y: f32| center + egui::vec2(x * s, y * s);
    match icon {
        Icon::Copy => {
            p.rect_stroke(
                egui::Rect::from_min_max(at(-3.0, -3.0), at(6.0, 6.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            p.line_segment([at(-6.0, 3.0), at(-6.0, -6.0)], stroke);
            p.line_segment([at(-6.0, -6.0), at(3.0, -6.0)], stroke);
        }
        Icon::Crop => {
            for (a, b, c) in [
                ((-6.0, -2.0), (-6.0, -6.0), (-2.0, -6.0)),
                ((2.0, -6.0), (6.0, -6.0), (6.0, -2.0)),
                ((-6.0, 2.0), (-6.0, 6.0), (-2.0, 6.0)),
                ((2.0, 6.0), (6.0, 6.0), (6.0, 2.0)),
            ] {
                p.line_segment([at(a.0, a.1), at(b.0, b.1)], stroke);
                p.line_segment([at(b.0, b.1), at(c.0, c.1)], stroke);
            }
        }
        Icon::Link => {
            p.rect_stroke(
                egui::Rect::from_min_max(at(-7.0, -4.0), at(-1.0, 4.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            p.rect_stroke(
                egui::Rect::from_min_max(at(1.0, -4.0), at(7.0, 4.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            p.line_segment([at(-3.0, 0.0), at(3.0, 0.0)], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_from_tray_clears_hidden_state_and_preserves_configuration() {
        let ctx = egui::Context::default();
        let mut app = DesktopApp::for_snapshot(&ctx, false);
        app.tray_available = true;
        app.config.fps = 300;
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            app.hide_to_tray(ctx);
            assert!(app.hidden);
            app.restore_window(ctx);
        });
        assert!(!app.hidden);
        assert_eq!(app.config.fps, 300);
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            commands
                .iter()
                .any(|cmd| matches!(cmd, egui::ViewportCommand::Visible(true)))
        );
        assert!(
            commands
                .iter()
                .any(|cmd| matches!(cmd, egui::ViewportCommand::Minimized(false)))
        );
        assert!(
            commands
                .iter()
                .any(|cmd| matches!(cmd, egui::ViewportCommand::Focus))
        );
    }
}
