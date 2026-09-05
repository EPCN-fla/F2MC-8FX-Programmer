//! 设置持久化（ron，启动恢复上次配置）

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// GUI 设置（ron 持久化，启动恢复上次配置）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 深色主题开关
    pub dark_theme: bool,
    /// 上次选中的型号名（896.csv 全表）
    pub chip_name: String,
    /// 上次使用的烧录文件路径
    pub last_hex: String,
    /// 烧录后写 0xFFFC 安全位
    pub write_secure: bool,
    /// 烧录后复位运行：None = 未选择
    pub reset_after: Option<bool>,
    /// 上次连接的通道描述
    pub channel_desc: String,
    /// STM32 上次使用的烧录文件路径
    pub stm32_file: String,
    /// STM32 BIN 烧入基地址输入
    pub stm32_bin_base: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark_theme: false, // 默认浅色（工业浅色主题）
            chip_name: "MB95F636H".into(),
            last_hex: String::new(),
            write_secure: false,
            reset_after: None,
            channel_desc: String::new(),
            stm32_file: String::new(),
            stm32_bin_base: "0x08000000".into(),
        }
    }
}

impl Settings {
    /// 配置文件路径（$XDG_CONFIG_HOME / %APPDATA% / ~/.config 下的 f2mc-studio/settings.ron）
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("f2mc-studio").join("settings.ron")
    }

    /// 加载设置（文件缺失或解析失败回退默认）
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => ron::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// 保存设置（失败静默忽略）
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) {
            let _ = std::fs::write(&path, text);
        }
    }
}
