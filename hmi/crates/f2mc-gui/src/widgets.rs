//! 自定义控件与视觉主题（工业风格）

use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use egui::{Align2, Color32, FontId, RichText, Stroke, Vec2};

// ------------------------------------------------------------- 日志桥（tracing → egui）

/// 日志级别（数值越小越严重，过滤时 `level <= min_level` 显示）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 错误
    Error = 0,
    /// 告警
    Warn = 1,
    /// 信息
    Info = 2,
    /// 调试
    Debug = 3,
}

impl Level {
    /// 级别短标签（日志行前缀）
    pub fn label(&self) -> &'static str {
        match self {
            Level::Error => "ERR",
            Level::Warn => "WRN",
            Level::Info => "INF",
            Level::Debug => "DBG",
        }
    }
    /// 全部级别（过滤器 UI 用）
    pub fn all() -> [Level; 4] {
        [Level::Error, Level::Warn, Level::Info, Level::Debug]
    }
}

/// 一行 GUI 日志
#[derive(Debug, Clone)]
pub struct LogLine {
    /// HH:MM:SS.mmm 时间戳
    pub time: String,
    /// 级别
    pub level: Level,
    /// 正文
    pub text: String,
}

/// 当前本地时间戳（HH:MM:SS.mmm）
pub fn now_stamp() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

/// tracing Layer：把日志事件转发到 GUI 日志缓冲
pub struct GuiLayer {
    tx: Mutex<Sender<LogLine>>,
}

impl GuiLayer {
    pub fn new(tx: Sender<LogLine>) -> Self {
        Self { tx: Mutex::new(tx) }
    }
}

struct MsgVisitor(String);
impl tracing::field::Visit for MsgVisitor {
    fn record_debug(&mut self, _f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        if self.0.is_empty() {
            self.0 = format!("{v:?}");
        }
    }
    fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
        if f.name() == "message" {
            self.0 = v.to_string();
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for GuiLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            tracing::Level::DEBUG => Level::Debug,
            // TRACE（DAP 帧日志）不转发到 GUI：量大且刷新频繁
            tracing::Level::TRACE => return,
        };
        let mut v = MsgVisitor(String::new());
        event.record(&mut v);
        let target = event.metadata().target();
        let text = if target == "dap" {
            format!("[dap] {}", v.0)
        } else {
            v.0
        };
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(LogLine { time: now_stamp(), level, text });
        }
    }
}

// ------------------------------------------------------------- 主题

/// 工业风格配色
#[derive(Clone)]
pub struct Theme {
    pub accent: Color32,     // 主强调色（按钮/进度）
    pub ok: Color32,         // 状态绿
    pub warn: Color32,       // 状态琥珀
    pub err: Color32,        // 状态红
    pub border: Color32,     // 单色描边
    pub panel: Color32,      // 面板底
    pub text_dim: Color32,   // 次级文字
}

impl Theme {
    pub fn light() -> Self {
        Self {
            accent: Color32::from_rgb(0x1F, 0x5F, 0xA8), // 工业蓝
            ok: Color32::from_rgb(0x1E, 0x8E, 0x3E),
            warn: Color32::from_rgb(0xC8, 0x8A, 0x00),
            err: Color32::from_rgb(0xC0, 0x28, 0x28),
            border: Color32::from_rgb(0xA8, 0xAD, 0xB4),
            panel: Color32::from_rgb(0xE9, 0xEB, 0xEE),
            text_dim: Color32::from_rgb(0x5A, 0x5F, 0x66),
        }
    }
    pub fn dark() -> Self {
        Self {
            accent: Color32::from_rgb(0x4E, 0x9A, 0xE0),
            ok: Color32::from_rgb(0x3E, 0xB0, 0x60),
            warn: Color32::from_rgb(0xE0, 0xA8, 0x20),
            err: Color32::from_rgb(0xE0, 0x50, 0x50),
            border: Color32::from_rgb(0x44, 0x49, 0x50),
            panel: Color32::from_rgb(0x22, 0x25, 0x2A),
            text_dim: Color32::from_rgb(0x8A, 0x90, 0x97),
        }
    }
}

/// 应用工业风格 visuals（在默认 light/dark 基础上收棱角、统一描边、去阴影）
pub fn apply_theme(ctx: &egui::Context, dark: bool) -> Theme {
    let t = if dark { Theme::dark() } else { Theme::light() };

    // egui 0.35 的 visuals 按明/暗两个槽存储，set_visuals 只写当前槽：
    // 系统主题为暗时，启动写入的是暗槽，切换后另一槽仍是 egui 默认 → 偏色。
    // 修复：强制主题偏好 + 两个槽都写入各自的定制 visuals
    ctx.set_theme(if dark {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
    for (slot, slot_dark) in [(egui::Theme::Light, false), (egui::Theme::Dark, true)] {
        let palette = if slot_dark { Theme::dark() } else { Theme::light() };
        let mut visuals = if slot_dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        customize_visuals(&mut visuals, &palette, slot_dark);
        ctx.set_visuals_of(slot, visuals);
    }
    t
}

/// 用调色板定制一份 visuals（圆角/描边/面板色/选中色等）
fn customize_visuals(visuals: &mut egui::Visuals, t: &Theme, dark: bool) {
    let corner = egui::CornerRadius::same(2); // 工业风：小圆角
    visuals.widgets.noninteractive.corner_radius = corner;
    visuals.widgets.inactive.corner_radius = corner;
    visuals.widgets.hovered.corner_radius = corner;
    visuals.widgets.active.corner_radius = corner;
    visuals.widgets.open.corner_radius = corner;
    visuals.window_corner_radius = corner;

    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0f32, t.border);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0f32, t.border);
    visuals.panel_fill = t.panel;
    visuals.window_fill = t.panel;
    visuals.extreme_bg_color = if dark {
        Color32::from_rgb(0x14, 0x16, 0x19)
    } else {
        Color32::from_rgb(0xD2, 0xD5, 0xD9)
    };
    visuals.selection.bg_fill = t.accent.linear_multiply(0.8);
    visuals.hyperlink_color = t.accent;
    // 工业风：面板/弹窗无阴影（避免深色阴影条）
    visuals.window_shadow = egui::epaint::Shadow::NONE;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;
}

// ------------------------------------------------------------- 控件

/// 状态指示灯（LED）
pub fn led(ui: &mut egui::Ui, color: Color32, label: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
    ui.painter()
        .circle_stroke(rect.center(), 4.5, Stroke::new(1.0f32, Color32::BLACK.linear_multiply(0.4f32)));
    if !label.is_empty() {
        ui.label(RichText::new(label).size(11.0));
    }
}

/// 工业分组框：单色描边 + 小标题
pub fn group(ui: &mut egui::Ui, theme: &Theme, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .stroke(Stroke::new(1.0f32, theme.border))
        .corner_radius(egui::CornerRadius::same(2))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.label(
                RichText::new(title)
                    .size(11.0)
                    .strong()
                    .color(theme.accent),
            );
            ui.add_space(4.0);
            add(ui);
        });
}

/// 日志视图（等宽单行 + show_rows 虚拟化，TRC 大量帧日志也不卡）
pub fn log_view(ui: &mut egui::Ui, theme: &Theme, logs: &VecDeque<LogLine>, min_level: Level) {
    let filtered: Vec<&LogLine> = logs.iter().filter(|l| l.level <= min_level).collect();
    const ROW_H: f32 = 15.0;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        // 滚动条常显：防止内容高度临界时滚动条出现/消失逐帧翻转（日志抽搐/反复滚动）
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show_rows(ui, ROW_H, filtered.len(), |ui, rows| {
            for i in rows {
                let line = filtered[i];
                let color = match line.level {
                    Level::Error => theme.err,
                    Level::Warn => theme.warn,
                    Level::Info => ui.visuals().text_color(),
                    Level::Debug => theme.text_dim,
                };
                ui.label(
                    RichText::new(format!("{} {} {}", line.time, line.level.label(), line.text))
                        .monospace()
                        .size(11.0)
                        .color(color),
                );
            }
        });
}

/// 大数字等宽显示（进度字节数等）
pub fn mono_num(ui: &mut egui::Ui, s: &str, size: f32, color: Color32) {
    ui.label(RichText::new(s).font(FontId::monospace(size)).color(color));
}

/// 居中提示文字（占位用）
pub fn centered_hint(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.painter().text(
        ui.max_rect().center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(13.0),
        theme.text_dim,
    );
}
