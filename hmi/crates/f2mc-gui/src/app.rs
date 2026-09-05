//! 主状态机 + 布局（工业风格）

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, RichText};

use f2mc_core::chipdef::{self, chips};
use f2mc_core::hexfile::{self, HexImage};
use stm32_flash::{self as stm32, ImageFormat, ProbeInfo};
use f2mc_core::transport::DapChannelInfo;

use crate::settings::Settings;
use crate::widgets::{self, Level, LogLine, Theme};
use crate::worker::{JobCfg, UiCommand, UiEvent, Worker};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    F2mc,
    Stm32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnState {
    Disconnected,
    Connected,
    Busy,
}

/// 页面：主页（模式选择）/ 工作页
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Work,
}

/// 应用主状态机：通道/连接/文件/选项/进度/日志状态 + 三栏布局绘制
pub struct F2mcStudioApp {
    // 页面
    page: Page,
    // 通道
    tx_cmd: Sender<UiCommand>,
    rx_evt: Receiver<UiEvent>,
    rx_log: Receiver<LogLine>,
    cancel: crate::worker::CancelToken,

    // 连接区
    family: Family,
    channels: Vec<DapChannelInfo>,
    channel_sel: Option<usize>, // None = 未选择
    chip_idx: usize,
    chip_filter: String,
    conn: ConnState,
    conn_desc: String,
    last_error: Option<String>,

    // STM32 区
    stm32_probes: Vec<ProbeInfo>,
    stm32_probe_sel: Option<usize>,
    /// 首次切到 STM32 面板时自动枚举一次探针
    stm32_probes_requested: bool,
    /// 自动识别芯片型号（勾选时手动选择灰化不可选）
    stm32_auto: bool,
    stm32_filter: String,
    stm32_target: Option<String>,
    /// 连接后识别到的型号（顶栏显示；断开清空）
    stm32_detected: String,
    stm32_file: String,
    /// BIN 烧入基地址（十六进制输入）
    stm32_bin_base: String,
    /// 烧录后自动读回校验
    stm32_verify_after: bool,
    /// 烧录后软件复位运行
    stm32_reset_after: bool,
    /// SWD 速率档（STM32_SPEEDS 索引）
    stm32_speed_idx: usize,

    // 文件区
    hex_path: String,
    image: Option<HexImage>,
    hex_error: Option<String>,
    parsed_key: String, // "path@chip_idx"，避免每帧重复解析

    // 选项
    write_secure: bool,
    /// 烧录后动作：None = 未选择（禁止烧录）
    reset_after: Option<bool>,

    // 进度区
    stage: String,
    done: usize,
    total: usize,
    started: Option<Instant>,
    cancel_requested: bool,
    /// 操作按钮行实测宽度（上一帧，用于烧录按钮与行右缘对齐）
    ops_row_w: f32,

    // 日志区
    logs: VecDeque<LogLine>,
    min_level: Level,

    // 外观
    dark: bool,
    theme: Theme,
    theme_dirty: bool,
}

impl F2mcStudioApp {
    /// 构造应用：加载设置、启动 worker 线程、应用主题、触发首次通道枚举
    pub fn new(cc: &eframe::CreationContext<'_>, rx_log: Receiver<LogLine>) -> Self {
        let settings = Settings::load();

        let (tx_cmd, rx_cmd) = mpsc::channel();
        let (tx_evt, rx_evt) = mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        Worker::spawn(rx_cmd, tx_evt, cc.egui_ctx.clone(), cancel.clone());

        let theme = widgets::apply_theme(&cc.egui_ctx, settings.dark_theme);

        // 型号名 → 索引（旧配置回退默认型号）
        let chip_idx = chips()
            .iter()
            .position(|c| c.name == settings.chip_name)
            .unwrap_or_else(|| {
                chips()
                    .iter()
                    .position(|c| c.name == chipdef::default_chip().name)
                    .unwrap_or(0)
            });

        let mut app = Self {
            page: Page::Home,
            tx_cmd,
            rx_evt,
            rx_log,
            cancel,
            family: Family::F2mc,
            channels: Vec::new(),
            channel_sel: None,
            chip_idx,
            chip_filter: String::new(),
            conn: ConnState::Disconnected,
            conn_desc: String::new(),
            last_error: None,
            stm32_probes: Vec::new(),
            stm32_probe_sel: None,
            stm32_probes_requested: false,
            stm32_auto: true,
            stm32_filter: String::new(),
            stm32_target: None,
            stm32_detected: String::new(),
            stm32_file: settings.stm32_file.clone(),
            stm32_bin_base: settings.stm32_bin_base.clone(),
            stm32_verify_after: true,
            stm32_reset_after: true,
            stm32_speed_idx: 2, // 默认 4 MHz
            hex_path: settings.last_hex.clone(),
            image: None,
            hex_error: None,
            parsed_key: String::new(),
            write_secure: settings.write_secure,
            reset_after: settings.reset_after,
            stage: String::new(),
            done: 0,
            total: 0,
            started: None,
            cancel_requested: false,
            ops_row_w: 0.0,
            logs: VecDeque::new(),
            min_level: Level::Info,
            dark: settings.dark_theme,
            theme,
            theme_dirty: false,
        };
        app.log(Level::Info, "F2MC Studio 已启动");
        let _ = app.tx_cmd.send(UiCommand::RefreshChannels);
        app
    }

    fn log(&mut self, level: Level, text: impl Into<String>) {
        self.logs.push_back(LogLine {
            time: widgets::now_stamp(),
            level,
            text: text.into(),
        });
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.rx_evt.try_recv() {
            match ev {
                UiEvent::Channels(list) => {
                    let n_v2 = list.iter().filter(|c| matches!(c.kind, f2mc_core::transport::ChannelKind::V2Bulk)).count();
                    let n_v1 = list.len() - n_v2;
                    self.log(Level::Info, format!("枚举到 {} 个通道（v2: {n_v2}, v1: {n_v1}）", list.len()));
                    // 自动预选 v2（列表已 v2 在前）
                    if !list.is_empty() && self.channel_sel.is_none() {
                        self.channel_sel = Some(0);
                    }
                    self.channels = list;
                }
                UiEvent::Connected(desc) => {
                    // STM32 模式从描述中提取识别到的型号（格式 "STM32 | {name}（{mode}）"）
                    if self.family == Family::Stm32 {
                        if let Some(n) = desc
                            .strip_prefix("STM32 | ")
                            .and_then(|s| s.split('（').next())
                        {
                            self.stm32_detected = n.trim().to_string();
                        }
                    }
                    self.conn = ConnState::Connected;
                    self.conn_desc = desc;
                    self.last_error = None;
                }
                UiEvent::Disconnected => {
                    self.conn = ConnState::Disconnected;
                    self.conn_desc.clear();
                    self.stm32_detected.clear();
                }
                UiEvent::Log(m) => self.log(Level::Info, m),
                UiEvent::Warn(m) => self.log(Level::Warn, m),
                UiEvent::Stage(s) => {
                    self.stage = s.to_string();
                }
                UiEvent::Progress { done, total } => {
                    self.done = done;
                    self.total = total;
                }
                UiEvent::Done(m) => {
                    self.conn = ConnState::Connected;
                    self.started = None;
                    self.cancel_requested = false;
                    self.log(Level::Info, format!("[+] {m}"));
                }
                UiEvent::Stm32Probes(list) => {
                    if !list.is_empty() && self.stm32_probe_sel.is_none() {
                        self.stm32_probe_sel = Some(0);
                    }
                    self.stm32_probes = list;
                }
                UiEvent::Failed(m) => {
                    self.conn = if self.conn_desc.is_empty() {
                        ConnState::Disconnected
                    } else {
                        ConnState::Connected
                    };
                    self.started = None;
                    self.cancel_requested = false;
                    self.last_error = Some(m.clone());
                    self.log(Level::Error, format!("[-] {m}"));
                }
            }
        }
        while let Ok(l) = self.rx_log.try_recv() {
            self.log(l.level, l.text);
        }
    }

    fn begin_job(&mut self, cmd: UiCommand) {
        self.conn = ConnState::Busy;
        self.done = 0;
        self.total = 0;
        self.stage.clear();
        self.started = Some(Instant::now());
        self.cancel_requested = false;
        self.cancel.store(false, Ordering::Relaxed);
        let _ = self.tx_cmd.send(cmd);
    }

    fn job(&self) -> Option<JobCfg> {
        if self.image.is_none() {
            return None;
        }
        Some(JobCfg {
            chip_idx: self.chip_idx,
            hex_path: PathBuf::from(&self.hex_path),
            write_secure: self.write_secure,
            reset_after: self.reset_after.unwrap_or(true),
        })
    }

    /// 路径或型号变化时解析 hex（UI 线程，解析很快）
    fn maybe_parse_hex(&mut self) {
        let key = format!("{}@{}", self.hex_path, self.chip_idx);
        if key == self.parsed_key {
            return;
        }
        self.parsed_key = key;
        self.image = None;
        self.hex_error = None;
        if self.hex_path.is_empty() {
            return;
        }
        match std::fs::read_to_string(&self.hex_path) {
            Ok(text) => match hexfile::parse(&text, &chips()[self.chip_idx]) {
                Ok(img) => {
                    for w in &img.warnings {
                        self.log(Level::Warn, format!("hex: {w}"));
                    }
                    self.image = Some(img);
                }
                Err(e) => self.hex_error = Some(format!("{e}")),
            },
            Err(e) => self.hex_error = Some(format!("读取文件失败：{e}")),
        }
    }

    fn set_hex_path(&mut self, p: PathBuf) {
        self.hex_path = p.to_string_lossy().into_owned();
        self.parsed_key.clear();
    }

    fn save_settings(&self) {
        Settings {
            dark_theme: self.dark,
            chip_name: chips()[self.chip_idx].name.clone(),
            last_hex: self.hex_path.clone(),
            write_secure: self.write_secure,
            reset_after: self.reset_after,
            channel_desc: self
                .channel_sel
                .and_then(|i| self.channels.get(i))
                .map(|c| c.to_string())
                .unwrap_or_default(),
            stm32_file: self.stm32_file.clone(),
            stm32_bin_base: self.stm32_bin_base.clone(),
        }
        .save();
    }

    // ---------------------------------------------------------- 各区绘制

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                // 返回主页（执行中禁用；已连接则先断开）
                if ui
                    .add_enabled(self.conn != ConnState::Busy, egui::Button::new("[ 主页 ]"))
                    .clicked()
                {
                    if self.conn != ConnState::Disconnected {
                        let _ = self.tx_cmd.send(UiCommand::Disconnect);
                    }
                    self.page = Page::Home;
                }
                ui.separator();
                ui.label(
                    RichText::new("F2MC STUDIO")
                        .monospace()
                        .size(15.0)
                        .strong()
                        .color(self.theme.accent),
                );
                ui.separator();
                // 当前型号（STM32 模式显示识别/选择的型号）
                let model = match self.family {
                    Family::F2mc => chips()[self.chip_idx].name.clone(),
                    Family::Stm32 => {
                        if !self.stm32_detected.is_empty() {
                            self.stm32_detected.clone()
                        } else if let Some(t) = &self.stm32_target {
                            t.clone()
                        } else {
                            "STM32".into()
                        }
                    }
                };
                ui.label(
                    RichText::new(model)
                        .monospace()
                        .size(12.0)
                        .strong()
                        .color(self.theme.accent),
                );
                ui.separator();
                // 状态 LED
                let (color, text) = match self.conn {
                    ConnState::Disconnected => (self.theme.text_dim, "未连接"),
                    ConnState::Connected => (self.theme.ok, "已连接"),
                    ConnState::Busy => (self.theme.warn, "执行中"),
                };
                widgets::led(ui, color, text);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn = if self.dark { "[ 浅色 ]" } else { "[ 深色 ]" };
                    if ui.button(btn).clicked() {
                        self.dark = !self.dark;
                        self.theme_dirty = true;
                        self.save_settings();
                    }
                });
            });
            ui.add_space(2.0);
        });
    }

    /// 主页：两个大按钮选择目标家族（参考 serial-helper 布局）
    fn home_page(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            let avail = ui.available_size();
            const TITLE_H: f32 = 44.0;
            const GAP: f32 = 50.0;
            const BTN_H: f32 = 150.0;
            const CONTENT_W: f32 = 2.0 * 220.0 + 20.0 + 20.0;
            let total_h = TITLE_H + GAP + BTN_H;
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(((avail.y - total_h) / 2.0).max(10.0));
                    ui.horizontal(|ui| {
                        ui.add_space(((avail.x - CONTENT_W) / 2.0).max(10.0));
                        ui.allocate_ui(egui::vec2(CONTENT_W, total_h), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new("F2MC STUDIO")
                                        .size(32.0)
                                        .strong()
                                        .color(self.theme.accent),
                                );
                                ui.add_space(GAP);
                                ui.horizontal(|ui| {
                                    for (label, family) in
                                        [("F2MC-8FX", Family::F2mc), ("STM32", Family::Stm32)]
                                    {
                                        if ui
                                            .add_sized(
                                                [220.0, BTN_H],
                                                egui::Button::new(
                                                    RichText::new(label).size(28.0),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            self.family = family;
                                            self.page = Page::Work;
                                            // 进入模式页即刷新对应编程器列表
                                            match family {
                                                Family::F2mc => {
                                                    let _ = self
                                                        .tx_cmd
                                                        .send(UiCommand::RefreshChannels);
                                                }
                                                Family::Stm32 => {
                                                    let _ = self
                                                        .tx_cmd
                                                        .send(UiCommand::Stm32RefreshProbes);
                                                    self.stm32_probes_requested = true;
                                                }
                                            }
                                        }
                                        ui.add_space(20.0);
                                    }
                                });
                            });
                        });
                    });
                });
        });
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        egui::Panel::left("left")
            .exact_size(330.0)
            .resizable(false)
            .show_separator_line(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                ui.add_space(6.0);
                let busy = self.conn == ConnState::Busy;
                let connected = self.conn != ConnState::Disconnected;

                // ---------------- 连接区 ----------------
                widgets::group(ui, &theme, "连接 CONNECTION", |ui| {
                    if self.family == Family::Stm32 {
                        // 首次进入自动枚举一次探针
                        if !self.stm32_probes_requested {
                            self.stm32_probes_requested = true;
                            let _ = self.tx_cmd.send(UiCommand::Stm32RefreshProbes);
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("编程器通道");
                            if ui.add_enabled(!busy, egui::Button::new("刷新")).clicked() {
                                let _ = self.tx_cmd.send(UiCommand::Stm32RefreshProbes);
                            }
                        });
                        let probe_desc = |p: &ProbeInfo| {
                            format!("{} (SN: {})", p.identifier, p.serial.as_deref().unwrap_or("-"))
                        };
                        let sel_text = self
                            .stm32_probe_sel
                            .and_then(|i| self.stm32_probes.get(i))
                            .map(&probe_desc)
                            .unwrap_or_else(|| "未发现探针".to_string());
                        egui::ComboBox::from_id_salt("stm32_probe")
                            .selected_text(RichText::new(sel_text).size(11.0))
                            .width(280.0)
                            .show_ui(ui, |ui| {
                                for (i, p) in self.stm32_probes.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut self.stm32_probe_sel,
                                        Some(i),
                                        RichText::new(probe_desc(p)).size(11.0),
                                    );
                                }
                            });

                        ui.add_space(4.0);
                        ui.checkbox(&mut self.stm32_auto, "自动识别芯片型号");
                        // 手动选择：自动识别勾选时灰化且不可选
                        ui.add_enabled_ui(!self.stm32_auto, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("型号");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.stm32_filter)
                                        .hint_text("输入关键词检索")
                                        .desired_width(180.0),
                                );
                                if ui.small_button("×").clicked() {
                                    self.stm32_filter.clear();
                                }
                            });
                            let results = stm32::search_targets(&self.stm32_filter);
                            egui::Frame::NONE
                                .stroke(egui::Stroke::new(1.0f32, theme.border))
                                .inner_margin(egui::Margin::same(2))
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical()
                                        .max_height(120.0)
                                        .auto_shrink([false, true])
                                        .show(ui, |ui| {
                                            ui.set_min_width(ui.available_width());
                                            for name in results.iter().take(200) {
                                                let selected =
                                                    self.stm32_target.as_deref() == Some(name);
                                                if ui
                                                    .selectable_label(
                                                        selected,
                                                        RichText::new(name).size(11.0),
                                                    )
                                                    .clicked()
                                                {
                                                    self.stm32_target = Some(name.clone());
                                                }
                                            }
                                        });
                                });
                        });
                        if self.stm32_auto {
                            ui.label(
                                RichText::new("连接时自动读取目标芯片 ID")
                                    .size(10.0)
                                    .color(theme.text_dim),
                            );
                        } else {
                            match &self.stm32_target {
                                Some(t) => ui.label(
                                    RichText::new(format!("当前：{t}"))
                                        .monospace()
                                        .size(11.0)
                                        .strong()
                                        .color(theme.accent),
                                ),
                                None => ui.label(
                                    RichText::new("请先选择型号")
                                        .size(10.0)
                                        .color(theme.warn),
                                ),
                            };
                        }

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("SWD 速率");
                            egui::ComboBox::from_id_salt("stm32_speed")
                                .selected_text(format!("{} MHz", STM32_SPEEDS[self.stm32_speed_idx] / 1000))
                                .show_ui(ui, |ui| {
                                    for (i, &khz) in STM32_SPEEDS.iter().enumerate() {
                                        ui.selectable_value(
                                            &mut self.stm32_speed_idx,
                                            i,
                                            format!("{} MHz", khz / 1000),
                                        );
                                    }
                                });
                        });

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let can_connect = !busy
                                && !connected
                                && (self.stm32_auto || self.stm32_target.is_some());
                            if ui
                                .add_enabled(can_connect, egui::Button::new("连  接"))
                                .clicked()
                            {
                                let serial = self
                                    .stm32_probe_sel
                                    .and_then(|i| self.stm32_probes.get(i))
                                    .and_then(|p| p.serial.clone());
                                let _ = self.tx_cmd.send(UiCommand::Stm32Connect {
                                    serial,
                                    auto: self.stm32_auto,
                                    target_name: self.stm32_target.clone().unwrap_or_default(),
                                    speed_khz: STM32_SPEEDS[self.stm32_speed_idx],
                                });
                            }
                            if ui
                                .add_enabled(!busy && connected, egui::Button::new("断  开"))
                                .clicked()
                            {
                                let _ = self.tx_cmd.send(UiCommand::Disconnect);
                            }
                        });
                        return;
                    }

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("编程器通道");
                        if ui
                            .add_enabled(!busy, egui::Button::new("刷新"))
                            .clicked()
                        {
                            let _ = self.tx_cmd.send(UiCommand::RefreshChannels);
                        }
                    });
                    let selected_text = match self.channel_sel {
                        Some(i) => self
                            .channels
                            .get(i)
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "未检测到编程器".to_string()),
                        None => "未检测到编程器".to_string(),
                    };
                    egui::ComboBox::from_id_salt("channel")
                        .selected_text(RichText::new(selected_text).size(11.0))
                        .width(280.0)
                        .show_ui(ui, |ui| {
                            for (i, c) in self.channels.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.channel_sel,
                                    Some(i),
                                    RichText::new(c.to_string()).size(11.0),
                                );
                            }
                        });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("型号");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.chip_filter)
                                .hint_text("输入关键词检索")
                                .desired_width(180.0),
                        );
                        if ui.small_button("×").clicked() {
                            self.chip_filter.clear();
                        }
                    });
                    // 常驻过滤列表（216 款，避免 ComboBox 弹窗点击即关闭的问题）
                    let results = chipdef::search(&self.chip_filter);
                    egui::Frame::NONE
                        .stroke(egui::Stroke::new(1.0f32, theme.border))
                        .inner_margin(egui::Margin::same(2))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(120.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    for c in results {
                                        let idx = chips()
                                            .iter()
                                            .position(|x| x.name == c.name)
                                            .unwrap();
                                        let selected = idx == self.chip_idx;
                                        let label = if c.fram {
                                            format!("{}  ({} KB, FRAM)", c.name, c.flash_bytes / 1024)
                                        } else {
                                            format!("{}  ({} KB)", c.name, c.flash_bytes / 1024)
                                        };
                                        if ui.selectable_label(selected, RichText::new(label).size(11.0)).clicked() {
                                            self.chip_idx = idx;
                                            self.parsed_key.clear(); // 重解析 hex
                                        }
                                    }
                                });
                        });
                    let cur = &chips()[self.chip_idx];
                    ui.label(
                        RichText::new(format!("当前：{} ({} KB)", cur.name, cur.flash_bytes / 1024))
                            .monospace()
                            .size(11.0)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.label(
                        RichText::new(format!("映射：{}", cur.ranges_desc()))
                            .monospace()
                            .size(10.0)
                            .color(theme.text_dim),
                    );
                    if cur.fram {
                        ui.label(
                            RichText::new("[!] FRAM/掩膜器件，New8FX 串行烧录不适用")
                                .size(10.0)
                                .color(theme.warn),
                        );
                    }

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !busy && !connected && self.channel_sel.is_some(),
                                egui::Button::new("连  接"),
                            )
                            .clicked()
                        {
                            if let Some(channel) =
                                self.channel_sel.and_then(|i| self.channels.get(i).cloned())
                            {
                                let _ = self.tx_cmd.send(UiCommand::Connect { channel });
                            }
                        }
                        if ui
                            .add_enabled(!busy && connected, egui::Button::new("断  开"))
                            .clicked()
                        {
                            let _ = self.tx_cmd.send(UiCommand::Disconnect);
                        }
                    });
                });

                ui.add_space(8.0);

                // ---------------- 文件区 ----------------
                if self.family == Family::F2mc {
                widgets::group(ui, &theme, "文件 FILE", |ui| {
                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.hex_path)
                                .desired_width(210.0)
                                .hint_text("拖放文件或点击浏览"),
                        );
                        if resp.changed() {
                            self.parsed_key.clear();
                        }
                        if ui.button("浏览…").clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("烧录文件", &["mhx", "hex", "ihx", "ehx", "s19"])
                                .add_filter("Intel HEX", &["hex", "ihx", "ehx"])
                                .add_filter("Motorola S-record", &["mhx", "s19", "mot"])
                                .pick_file()
                            {
                                self.set_hex_path(p);
                            }
                        }
                    });

                    if let Some(err) = &self.hex_error {
                        ui.label(RichText::new(format!("[-] {err}")).size(11.0).color(theme.err));
                    } else if let Some(img) = &self.image {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!(
                                "{} | 共 {} 字节，{} 个区间",
                                img.format.label(),
                                img.total_bytes(),
                                img.segments.len()
                            ))
                            .monospace()
                            .size(11.0),
                        );
                        for (addr, data) in img.segments.iter().take(4) {
                            ui.label(
                                RichText::new(format!(
                                    "  0x{:04X}..0x{:04X}  ({} B)",
                                    addr,
                                    addr + data.len() as u32 - 1,
                                    data.len()
                                ))
                                .monospace()
                                .size(10.0)
                                .color(theme.text_dim),
                            );
                        }
                        if img.segments.len() > 4 {
                            ui.label(
                                RichText::new(format!("  … 等 {} 个区间", img.segments.len()))
                                    .size(10.0)
                                    .color(theme.text_dim),
                            );
                        }
                        for w in &img.warnings {
                            ui.label(RichText::new(format!("[!] {w}")).size(10.0).color(theme.warn));
                        }
                    }
                });

                ui.add_space(8.0);

                // ---------------- 操作区 ----------------
                widgets::group(ui, &theme, "操作 OPERATIONS", |ui| {
                    let can_op = connected && !busy;
                    let has_img = self.image.is_some();
                    let can_burn = can_op && has_img && self.reset_after.is_some();

                    // 宽度与下方操作按钮行右缘对齐（用上一帧实测行宽，一帧收敛）
                    let burn_w = if self.ops_row_w > 0.0 {
                        self.ops_row_w
                    } else {
                        140.0
                    };
                    let burn = ui.add_enabled(
                        can_burn,
                        egui::Button::new(RichText::new("烧  录").strong().color(Color32::WHITE))
                            .min_size(egui::Vec2::new(burn_w, 28.0))
                            .fill(theme.accent),
                    );
                    if burn.clicked() {
                        if let Some(job) = self.job() {
                            self.begin_job(UiCommand::Program(job));
                        }
                    }
                    if has_img && self.reset_after.is_none() {
                        ui.label(
                            RichText::new("请先选择下方“烧录完成后”动作")
                                .size(10.0)
                                .color(theme.warn),
                        );
                    }

                    ui.add_space(4.0);
                    let ops_row = ui.horizontal(|ui| {
                        if ui
                            .add_enabled(can_op, egui::Button::new("擦除"))
                            .clicked()
                        {
                            self.begin_job(UiCommand::EraseOnly);
                        }
                        if ui
                            .add_enabled(can_op && has_img, egui::Button::new("校验"))
                            .clicked()
                        {
                            if let Some(job) = self.job() {
                                self.begin_job(UiCommand::VerifyOnly(job));
                            }
                        }
                        if ui
                            .add_enabled(can_op, egui::Button::new("读取"))
                            .clicked()
                        {
                            if let Some(p) = rfd::FileDialog::new()
                                .set_file_name("readout.hex")
                                .add_filter("Intel HEX", &["hex"])
                                .add_filter("Motorola S-record", &["mhx"])
                                .add_filter("二进制 BIN", &["bin"])
                                .save_file()
                            {
                                self.begin_job(UiCommand::ReadOut { out: p });
                            }
                        }
                        if ui
                            .add_enabled(can_op, egui::Button::new("复位"))
                            .clicked()
                        {
                            self.begin_job(UiCommand::ResetRun);
                        }
                        if ui
                            .add_enabled(can_op, egui::Button::new("断电"))
                            .on_hover_text("断开目标供电（状态机复位 IDLE）")
                            .clicked()
                        {
                            self.begin_job(UiCommand::SetPower(false));
                        }
                        if ui
                            .add_enabled(can_op, egui::Button::new("上电"))
                            .on_hover_text("目标上电（含上升确认，约 3 s）")
                            .clicked()
                        {
                            self.begin_job(UiCommand::SetPower(true));
                        }
                    });
                    // 记录行宽供烧录按钮对齐（下一帧生效）
                    self.ops_row_w = ops_row.response.rect.width();

                    ui.add_space(6.0);
                    // 烧录完成后动作：显式单选，无默认（未选择时禁止烧录）
                    ui.label("烧录完成后：");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.reset_after, Some(true), "下载后复位运行");
                        ui.radio_value(&mut self.reset_after, Some(false), "保持编程模式");
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.write_secure, "写入安全位 (0xFFFC)");
                    });
                    if self.write_secure {
                        ui.label(
                            RichText::new("[!] 启用后需整片擦除才能再次烧录！")
                                .size(11.0)
                                .strong()
                                .color(theme.err),
                        );
                    }
                });
                } else {
                    self.ui_stm32_file_ops(ui, &theme, busy, connected);
                }
                ui.add_space(8.0);
            });
                    });
    }

    /// STM32 家族的文件与操作区（烧录 / 擦除 / 校验 / 复位）
    fn ui_stm32_file_ops(&mut self, ui: &mut egui::Ui, theme: &Theme, busy: bool, connected: bool) {
        let can_op = connected && !busy;
        let file_now = self.stm32_file.clone();
        let bin_base_now = self.stm32_bin_base.clone();
        // fmt：扩展名判格式；BIN 时用偏移输入框的值
        let fmt = stm32_image_format(&file_now, &bin_base_now);
        let bin_base_valid = parse_addr(&bin_base_now).is_some();
        let is_bin = matches!(fmt, Some((ImageFormat::Bin { .. }, _)));
        let can_burn = can_op && fmt.is_some() && (!is_bin || bin_base_valid);

        widgets::group(ui, theme, "文件 FILE", |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.stm32_file)
                        .desired_width(210.0)
                        .hint_text("hex / bin / elf"),
                );
                if ui.button("浏览…").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("烧录文件", &["hex", "ihx", "bin", "elf", "axf"])
                        .add_filter("Intel HEX", &["hex", "ihx"])
                        .add_filter("二进制 BIN", &["bin"])
                        .add_filter("ELF", &["elf", "axf"])
                        .pick_file()
                    {
                        self.stm32_file = p.to_string_lossy().into_owned();
                    }
                }
            });
            match &fmt {
                Some((_, label)) => {
                    ui.label(RichText::new(format!("格式：{label}")).monospace().size(11.0));
                }
                None if !file_now.is_empty() => {
                    ui.label(
                        RichText::new("[-] 无法识别的格式（支持 hex/bin/elf）")
                            .size(11.0)
                            .color(theme.err),
                    );
                }
                None => {
                    ui.label(RichText::new("未选择文件").size(10.0).color(theme.text_dim));
                }
            }
            // BIN 无内嵌地址：提供烧入地址输入
            if is_bin {
                ui.horizontal(|ui| {
                    ui.label("烧入地址");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.stm32_bin_base)
                            .desired_width(110.0)
                            .hint_text("0x08000000"),
                    );
                });
                if !bin_base_valid {
                    ui.label(
                        RichText::new("[-] 地址格式错误（十六进制 0x... 或十进制）")
                            .size(10.0)
                            .color(theme.err),
                    );
                }
            }
        });

        ui.add_space(8.0);

        widgets::group(ui, theme, "操作 OPERATIONS", |ui| {
            let burn = ui.add_enabled(
                can_burn,
                egui::Button::new(RichText::new("烧  录").strong().color(Color32::WHITE))
                    .min_size(egui::Vec2::new(140.0, 28.0))
                    .fill(theme.accent),
            );
            if burn.clicked() {
                if let Some((f, _)) = fmt.clone() {
                    self.begin_job(UiCommand::Stm32Program {
                        path: PathBuf::from(&self.stm32_file),
                        fmt: f,
                        verify_after: self.stm32_verify_after,
                        reset_after: self.stm32_reset_after,
                    });
                }
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(can_op, egui::Button::new("擦除")).clicked() {
                    self.begin_job(UiCommand::Stm32Erase);
                }
                if ui
                    .add_enabled(can_burn, egui::Button::new("校验"))
                    .clicked()
                {
                    if let Some((f, _)) = fmt.clone() {
                        self.begin_job(UiCommand::Stm32Verify {
                            path: PathBuf::from(&self.stm32_file),
                            fmt: f,
                        });
                    }
                }
                if ui.add_enabled(can_op, egui::Button::new("复位")).clicked() {
                    self.begin_job(UiCommand::Stm32ResetRun);
                }
            });

            ui.add_space(6.0);
            // 执行中灰化，禁止改选项
            ui.add_enabled_ui(!busy, |ui| {
                ui.checkbox(&mut self.stm32_verify_after, "烧录后自动校验");
                ui.checkbox(&mut self.stm32_reset_after, "烧录后复位运行（软复位）");
            });
        });
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(6.0);

            // ---------------- 进度区 ----------------
            widgets::group(ui, &theme, "进度 PROGRESS", |ui| {
                ui.horizontal(|ui| {
                    ui.label("阶段");
                    widgets::mono_num(
                        ui,
                        if self.stage.is_empty() { "等待" } else { &self.stage },
                        13.0,
                        theme.accent,
                    );
                    if let Some(t0) = self.started {
                        ui.label(
                            RichText::new(format!("已用 {:.1} s", t0.elapsed().as_secs_f32()))
                                .monospace()
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.conn == ConnState::Busy {
                            let label = if self.cancel_requested { "取消中…" } else { "取  消" };
                            if ui
                                .add_enabled(!self.cancel_requested, egui::Button::new(label))
                                .clicked()
                            {
                                self.cancel_requested = true;
                                self.cancel.store(true, Ordering::Relaxed);
                            }
                        }
                    });
                });
                ui.add_space(2.0);
                let frac = if self.total > 0 {
                    self.done as f32 / self.total as f32
                } else {
                    0.0
                };
                let text = if self.total > 0 {
                    format!("{:.1}%   {} / {} B", frac * 100.0, self.done, self.total)
                } else if self.conn == ConnState::Busy {
                    "执行中…".to_string()
                } else {
                    String::new()
                };
                let bar_w = ui.available_width();
                let bar = egui::ProgressBar::new(frac)
                    .desired_height(16.0)
                    .desired_width(bar_w)
                    .text(
                        RichText::new(text)
                            .monospace()
                            .size(11.0)
                            .strong()
                            .color(Color32::WHITE),
                    );
                let bar = if self.conn == ConnState::Busy && self.total == 0 {
                    bar.animate(true)
                } else {
                    bar
                };
                ui.add(bar.fill(theme.accent));
            });

            ui.add_space(8.0);

            // ---------------- 日志区 ----------------
            widgets::group(ui, &theme, "日志 LOG", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("级别");
                    for lv in Level::all() {
                        ui.selectable_value(&mut self.min_level, lv, lv.label());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("清空").clicked() {
                            self.logs.clear();
                        }
                        if ui.button("导出日志").clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .set_file_name("f2mc-trace.log")
                                .save_file()
                            {
                                let text: String = self
                                    .logs
                                    .iter()
                                    .map(|l| format!("{} [{}] {}\n", l.time, l.level.label(), l.text))
                                    .collect();
                                match std::fs::write(&p, text) {
                                    Ok(()) => self.log(Level::Info, format!("日志已导出：{}", p.display())),
                                    Err(e) => self.log(Level::Error, format!("导出失败：{e}")),
                                }
                            }
                        }
                    });
                });
                ui.add_space(4.0);
                let height = (ui.available_height() - 4.0).max(80.0);
                egui::Frame::NONE
                    .stroke(egui::Stroke::new(1.0f32, theme.border))
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.set_min_height(height);
                        if self.logs.is_empty() {
                            widgets::centered_hint(ui, &theme, "暂无日志");
                        } else {
                            widgets::log_view(ui, &theme, &self.logs, self.min_level);
                        }
                    });
            });
        });
    }
}

impl eframe::App for F2mcStudioApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.theme_dirty {
            self.theme = widgets::apply_theme(&ctx, self.dark);
            self.theme_dirty = false;
        }

        self.drain_events();
        self.maybe_parse_hex();

        // 拖放文件
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(f) = dropped.into_iter().next() {
            if let Some(p) = f.path {
                self.set_hex_path(p);
            }
        }

        if self.page == Page::Home {
            self.home_page(ui);
            return;
        }

        self.top_bar(ui);
        self.left_panel(ui);
        self.central_panel(ui);

        // busy 时保持刷新（计时显示）
        if self.conn == ConnState::Busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn on_exit(&mut self) {
        // GUI 关闭前发送 DISCONNECT（0x11）：通知固件熄 LED、复位状态机
        if self.conn != ConnState::Disconnected {
            let _ = self.tx_cmd.send(UiCommand::Disconnect);
            std::thread::sleep(std::time::Duration::from_millis(200)); // 给 worker 留出发送时间
        }
        self.save_settings();
    }
}

/// SWD 速率档位（kHz）：1/2/4/8 MHz，默认 4 MHz（OpenOCD 实测 4 MHz 烧录 ~7 s）
const STM32_SPEEDS: [u32; 4] = [1000, 2000, 4000, 8000];

/// 解析地址输入：支持 "0x..." 十六进制或十进制
fn parse_addr(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

/// STM32 镜像格式判定：按扩展名（优先级 hex > bin > elf）；BIN 解析烧入地址
///
/// @param path 文件路径
/// @param bin_base BIN 烧入基地址输入（仅 BIN 使用）
/// @returns (格式, 显示名)；无法识别返回 None
fn stm32_image_format(path: &str, bin_base: &str) -> Option<(ImageFormat, &'static str)> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "hex" | "ihx" => Some((ImageFormat::Hex, "Intel HEX")),
        "bin" => Some((
            ImageFormat::Bin { base_address: parse_addr(bin_base) },
            "BIN",
        )),
        "elf" | "axf" => Some((ImageFormat::Elf, "ELF")),
        _ => None,
    }
}
