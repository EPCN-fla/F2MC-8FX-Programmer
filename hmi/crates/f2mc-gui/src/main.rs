//! F2MC Studio —— F2MC-8FX / STM32 编程器上位机（egui）

// Windows 下作为 GUI 子系统构建，不弹出控制台黑窗口
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod settings;
mod widgets;
mod worker;

use std::sync::mpsc;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> eframe::Result<()> {
    // tracing → GUI 日志桥（级别过滤在 GUI 侧；TRACE 帧日志不转发）
    let (tx_log, rx_log) = mpsc::channel();
    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::DEBUG)
        .with(widgets::GuiLayer::new(tx_log))
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([860.0, 560.0])
            .with_title("F2MC Studio")
            // 自带 RGBA 图标，避免 eframe 解码内嵌 PNG（libpng iCCP 警告）
            .with_icon(std::sync::Arc::new(app_icon())),
        ..Default::default()
    };

    eframe::run_native(
        "f2mc-studio",
        options,
        Box::new(move |cc| {
            load_cjk_fonts(&cc.egui_ctx);
            Ok(Box::new(app::F2mcStudioApp::new(cc, rx_log)))
        }),
    )
}

/// 程序化生成 32×32 RGBA 图标（工业蓝底 + 白色 F）
fn app_icon() -> egui::IconData {
    const W: usize = 32;
    let mut rgba = vec![0u8; W * W * 4];
    for y in 0..W {
        for x in 0..W {
            let i = (y * W + x) * 4;
            let border = x < 2 || y < 2 || x >= W - 2 || y >= W - 2;
            let glyph = (9..=11).contains(&x) && (7..=25).contains(&y)
                || (9..=22).contains(&x) && (7..=9).contains(&y)
                || (9..=19).contains(&x) && (15..=17).contains(&y);
            let (r, g, b): (u8, u8, u8) = if glyph {
                (255, 255, 255)
            } else if border {
                (0x14, 0x3A, 0x5E)
            } else {
                (0x1F, 0x5F, 0xA8)
            };
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 255;
        }
    }
    egui::IconData { rgba, width: W as u32, height: W as u32 }
}

/// 加载 CJK 字体（egui 默认字体无中文字形），按候选路径顺序尝试
fn load_cjk_fonts(ctx: &egui::Context) {
    let candidates = [
        // WSL → Windows 字体
        "/mnt/c/Windows/Fonts/msyh.ttc",
        "/mnt/c/Windows/Fonts/simhei.ttf",
        "/mnt/c/Windows/Fonts/simsun.ttc",
        // Linux Noto
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        // Windows 原生
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
    ];
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("cjk".into());
        }
        ctx.set_fonts(fonts);
        tracing::info!("loaded CJK font: {path}");
        return;
    }
    tracing::warn!("no CJK font found; Chinese text may not render");
}
