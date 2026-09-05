//! STM32 烧录内核：基于 probe-rs（精确锁定 0.32.0，API 以该版本 docs.rs 为准）。
//!
//! 经编程器的 CMSIS-DAP 接口（SWD）连接任意 STM32 目标，提供烧录 / 校验 /
//! 整片擦除 / 复位运行四种操作。芯片自动识别走 probe-rs 内置目标库
//! （STM32F1/F4/G0/H7 等全系），也可手动指定型号。
//!
//! 说明：
//! - 编程器无 SRST 硬件线，复位运行走 NVIC SYSRESETREQ 软件复位（`Core::reset`）
//! - probe-rs 0.32 无中途取消机制，取消令牌仅在阶段边界检查（写/校验以页为单位，单页很快）

use probe_rs::config::Registry;
use probe_rs::flashing::{
    self, BinLoader, BinOptions, DownloadOptions, ElfLoader, ElfOptions, FlashProgress,
    HexLoader, ProgressEvent, ProgressOperation,
};
use probe_rs::probe::list::Lister;
use probe_rs::{MemoryInterface, Permissions, Session};
use probe_rs::config::TargetSelector;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 取消令牌：GUI 在阶段边界置位（probe-rs 单页内不可中断）
pub type CancelToken = Arc<AtomicBool>;

// ---------------------------------------------------------------- 错误类型

/// STM32 烧录错误（与 f2mc-core 风格统一的包装）
#[derive(Debug, thiserror::Error)]
pub enum Stm32Error {
    /// 探针打开/通信错误（USB 层）
    #[error("探针错误：{0}")]
    Probe(#[from] probe_rs::probe::DebugProbeError),
    /// 会话/目标连接错误（SWD 握手、芯片识别等）
    #[error("连接目标失败：{0}")]
    Session(#[from] probe_rs::Error),
    /// 镜像下载错误（含文件解析与烧录）
    #[error("烧录失败：{0}")]
    Download(#[from] probe_rs::flashing::FileDownloadError),
    /// Flash 操作错误（校验/擦除）
    #[error("Flash 操作失败：{0}")]
    Flash(#[from] probe_rs::flashing::FlashError),
    /// 未发现任何 CMSIS-DAP 调试探针
    #[error("未发现调试探针（请检查编程器 USB 连接）")]
    NoProbe,
    /// 指定的探针序列号未匹配到设备
    #[error("未找到序列号为 {0} 的探针")]
    ProbeNotFound(String),
    /// 参数错误
    #[error("{0}")]
    BadParam(String),
    /// 用户在阶段边界取消
    #[error("已取消")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Stm32Error>;

// ---------------------------------------------------------------- 探针枚举

/// 调试探针信息（GUI 通道列表展示用）
#[derive(Debug, Clone)]
pub struct ProbeInfo {
    /// 探针标识字符串（如 "CMSIS-DAP"）
    pub identifier: String,
    /// USB 序列号（区分多探针）
    pub serial: Option<String>,
    /// USB VID/PID
    pub vid: u16,
    pub pid: u16,
}

/// 枚举所有调试探针
///
/// @returns 探针列表；无探针时返回空 Vec（非错误）
pub fn list_probes() -> Vec<ProbeInfo> {
    Lister::new()
        .list_all()
        .into_iter()
        .map(|i| ProbeInfo {
            identifier: i.identifier,
            serial: i.serial_number,
            vid: i.vendor_id,
            pid: i.product_id,
        })
        .collect()
}

// ---------------------------------------------------------------- 型号检索

/// 关键词检索 probe-rs 内置目标库（仅返回 STM32 系列，按名称排序）
///
/// @param keyword 检索词（空串返回全部 STM32 型号；大小写不敏感的子串匹配）
/// @returns 目标名称列表，如 ["STM32F103C8", ...]
///
/// @warning 不用 `Registry::search_chips`：它仅做前缀匹配（"F103" 匹不到 "STM32F103xx"），
///          此处直接遍历 families 做子串匹配。
pub fn search_targets(keyword: &str) -> Vec<String> {
    let registry = Registry::from_builtin_families();
    let kw = keyword.to_ascii_uppercase();
    let mut v: Vec<String> = registry
        .families()
        .iter()
        .flat_map(|f| f.variants.iter().map(|c| c.name.to_string()))
        .filter(|n| n.to_ascii_uppercase().starts_with("STM32"))
        .filter(|n| kw.is_empty() || n.to_ascii_uppercase().contains(&kw))
        .collect();
    v.sort();
    v.dedup();
    v
}

// ---------------------------------------------------------------- 目标与镜像

/// 目标芯片选择方式
#[derive(Debug, Clone)]
pub enum TargetChoice {
    /// 自动识别（读目标 ROM 表）
    Auto,
    /// 手动指定型号名（probe-rs 目标库名称，如 "STM32F103C8"）
    Named(String),
}

/// 镜像文件格式（GUI 按扩展名选择；优先级 hex > bin > elf）
#[derive(Debug, Clone)]
pub enum ImageFormat {
    /// Intel HEX（.hex/.ihx）
    Hex,
    /// 纯二进制（.bin）；base_address=None 时写入目标最低 Flash 起始地址
    Bin { base_address: Option<u64> },
    /// ELF 可执行文件（.elf/.axf）
    Elf,
}

// ---------------------------------------------------------------- 进度事件

/// 烧录进度事件（probe-rs ProgressEvent 的简化投影，供 UI 层消费）
#[derive(Debug, Clone)]
pub enum OpEvent {
    /// 阶段开始：操作名（erase/program/verify）与总量（未知为 None）
    StageStart { op: String, total: Option<u64> },
    /// 阶段内进度：累计完成字节数
    Progress { done: u64 },
    /// 阶段完成
    StageEnd,
    /// 诊断消息（flash 算法输出等）
    Message(String),
}

// ---------------------------------------------------------------- 会话

/// STM32 烧录会话：持有一个已 attach 的 probe-rs Session
pub struct Stm32Session {
    session: Session,
}

impl Stm32Session {
    /// 连接探针并 attach 目标
    ///
    /// @param serial 探针序列号过滤（None = 第一个探针）
    /// @param target 自动识别或手动型号
    /// @param speed_khz SWD 速率（固件实测 4 MHz 可用，OpenOCD 同速烧录 ~7 s）
    /// @returns 已就绪会话（SWD 已握手、目标已识别）
    ///
    /// 自动识别分两步：先走 probe-rs Auto（依赖 ROM 表，仅部分芯片有效）；
    /// 失败后回退到 DBGMCU IDCODE 识别（见 [`auto_identify`]）。
    pub fn connect(serial: Option<&str>, target: &TargetChoice, speed_khz: u32) -> Result<Self> {
        let probe = open_probe(serial, speed_khz)?;
        let selector = match target {
            TargetChoice::Auto => TargetSelector::Auto,
            TargetChoice::Named(n) => TargetSelector::Unspecified(n.clone()),
        };
        // allow_erase_all：整片擦除需要该权限
        match probe.attach(selector, Permissions::new().allow_erase_all()) {
            Ok(session) => Ok(Self { session }),
            Err(e) => {
                if matches!(target, TargetChoice::Auto) {
                    // Auto 依赖 ROM 表 PID，STM32 全系不在其中 → DBGMCU 识别
                    tracing::info!("probe-rs Auto 失败（{e}），回退 DBGMCU IDCODE 识别");
                    auto_identify(serial, speed_khz)
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// 当前识别到的目标型号名
    ///
    /// @returns 如 "STM32F103C8"
    pub fn target_name(&self) -> &str {
        &self.session.target().name
    }

    /// 烧录镜像（扇区擦除 + 分页写入，可选烧录后自动校验）
    ///
    /// @param path 镜像文件路径
    /// @param fmt 格式（hex/bin/elf）
    /// @param verify_after 烧录完成后自动读回校验
    /// @param cb 进度回调
    /// @param cancel 取消令牌（阶段边界检查）
    ///
    /// @warning probe-rs 页内不可中断；取消在下一阶段开始前生效
    pub fn program(
        &mut self,
        path: &Path,
        fmt: &ImageFormat,
        verify_after: bool,
        cb: &mut dyn FnMut(OpEvent),
        cancel: &CancelToken,
    ) -> Result<()> {
        check_cancel(cancel)?;
        // DownloadOptions 为 non_exhaustive，用 default 后逐字段赋值
        let mut options = DownloadOptions::default();
        options.progress = make_progress(cb);
        options.keep_unwritten_bytes = false;
        flashing::download_file_with_options(&mut self.session, path, make_loader(fmt), options)?;
        check_cancel(cancel)?;
        if verify_after {
            self.verify(path, fmt, cb, cancel)?;
        }
        Ok(())
    }

    /// 仅校验镜像与芯片内容一致
    ///
    /// @param path 镜像文件路径（格式同 program）
    /// @warning 校验的是镜像覆盖范围；未覆盖区域不比对
    pub fn verify(
        &mut self,
        path: &Path,
        fmt: &ImageFormat,
        cb: &mut dyn FnMut(OpEvent),
        cancel: &CancelToken,
    ) -> Result<()> {
        check_cancel(cancel)?;
        let loader = flashing::build_loader(&mut self.session, path, make_loader(fmt), None)?;
        {
            let mut progress = make_progress(&mut *cb);
            loader.verify(&mut self.session, &mut progress)?;
        }
        check_cancel(cancel)
    }

    /// 整片擦除（含选项字节外的全部非易失存储）
    ///
    /// @warning 结果使芯片回到出厂空白状态
    pub fn erase_all(&mut self, cb: &mut dyn FnMut(OpEvent), cancel: &CancelToken) -> Result<()> {
        check_cancel(cancel)?;
        cb(OpEvent::StageStart {
            op: "erase".into(),
            total: None,
        });
        {
            let mut progress = make_progress(&mut *cb);
            flashing::erase_all(&mut self.session, &mut progress, false)?;
        }
        cb(OpEvent::StageEnd);
        check_cancel(cancel)
    }

    /// 复位并运行（NVIC SYSRESETREQ 软件复位，无需硬件复位线）
    pub fn reset_run(&mut self) -> Result<()> {
        self.session.core(0)?.reset()?;
        Ok(())
    }
}

// ---------------------------------------------------------------- 内部辅助

/// 打开探针并设置 SWD 速率
fn open_probe(serial: Option<&str>, speed_khz: u32) -> Result<probe_rs::probe::Probe> {
    let lister = Lister::new();
    let infos = lister.list_all();
    let info = match serial {
        Some(sn) => infos
            .iter()
            .find(|i| i.serial_number.as_deref() == Some(sn))
            .ok_or_else(|| Stm32Error::ProbeNotFound(sn.into()))?,
        None => infos.first().ok_or(Stm32Error::NoProbe)?,
    };
    let mut probe = info.open()?;
    let _ = probe.set_speed(speed_khz); // 不支持的探针忽略
    Ok(probe)
}

/// DBGMCU_IDCODE 候选地址（家族差异：F1/F4 在 0xE0042000，G0/G4 在 0x40015800，H7 在 0x5C001000）
const DBGMCU_ADDRS: [u32; 3] = [0xE004_2000, 0x4001_5800, 0x5C00_1000];

/// probe-rs Auto 对 STM32 无效（registry 无 ROM 表 PID）时的回退识别：
/// 借 F103 目标建立探测会话（内存读不需要 flash 算法），读 DBGMCU_IDCODE 与容量寄存器，
/// 查 DEV_ID 表得到具体型号后重新正式 attach
fn auto_identify(serial: Option<&str>, speed_khz: u32) -> Result<Stm32Session> {
    let probe = open_probe(serial, speed_khz)?;
    let mut probe_session = probe
        .attach(
            TargetSelector::Unspecified("STM32F103C8".into()),
            Permissions::new(),
        )
        .map_err(Stm32Error::Session)?;

    let mut core = probe_session.core(0).map_err(Stm32Error::Session)?;
    // 依次尝试各候选地址，读成功且 DEV_ID 已知即采信
    let mut identified: Option<(u16, Option<u16>)> = None;
    let mut last_idcode = 0u32;
    for &addr in &DBGMCU_ADDRS {
        let Ok(idcode) = core.read_word_32(addr as u64) else {
            continue;
        };
        last_idcode = idcode;
        let dev_id = (idcode & 0xFFF) as u16;
        let flash_kb = flash_size_addr(dev_id)
            .and_then(|a| core.read_word_32(a as u64).ok())
            .map(|v| (v & 0xFFFF) as u16);
        if identify(dev_id, flash_kb).is_some() {
            identified = Some((dev_id, flash_kb));
            break;
        }
    }
    drop(core);
    drop(probe_session);

    let (dev_id, flash_kb) = identified.ok_or_else(|| {
        Stm32Error::BadParam(format!(
            "无法识别目标芯片（IDCODE=0x{last_idcode:08X}），请取消自动识别手动选择型号"
        ))
    })?;
    let name = identify(dev_id, flash_kb).expect("已确认可识别");
    tracing::info!("DBGMCU 识别：DEV_ID=0x{dev_id:03X} flash={flash_kb:?}KB → {name}");

    // 重新开探针正式 attach（probe.attach 消费探针，需重新打开）
    let probe = open_probe(serial, speed_khz)?;
    let session = probe
        .attach(
            TargetSelector::Unspecified(name),
            Permissions::new().allow_erase_all(),
        )
        .map_err(Stm32Error::Session)?;
    Ok(Stm32Session { session })
}

/// 各家族 Flash 容量寄存器地址（读回 u16 单位 KB）
fn flash_size_addr(dev_id: u16) -> Option<u32> {
    match dev_id {
        // STM32F1
        0x410 | 0x412 | 0x414 | 0x418 | 0x420 | 0x428 | 0x430 => Some(0x1FFF_F7E0),
        // STM32F0
        0x440 | 0x442 | 0x444 | 0x445 | 0x448 => Some(0x1FFF_F7CC),
        // STM32F4
        0x413 | 0x419 | 0x421 | 0x423 | 0x433 => Some(0x1FFF_7A22),
        // STM32F7
        0x449 | 0x451 => Some(0x1FF0_F442),
        // STM32L4 / G4 / G0
        0x435 | 0x461 | 0x462 | 0x468 | 0x469 | 0x479 | 0x460 | 0x466 | 0x467 => {
            Some(0x1FFF_75E0)
        }
        // STM32H7
        0x450 => Some(0x1FF1_E880),
        _ => None,
    }
}

/// DBGMCU DEV_ID → probe-rs 目标名（选同密度代表型号；F1 按容量寄存器细分）
///
/// @param dev_id IDCODE 低 12 位
/// @param flash_kb 容量寄存器读值（可能为 None）
/// @returns 目标名；未知 DEV_ID 返回 None
fn identify(dev_id: u16, flash_kb: Option<u16>) -> Option<String> {
    let name: &str = match dev_id {
        // ---- STM32F1 ----
        0x410 => match flash_kb.unwrap_or(64) {
            0..=64 => "STM32F103C8",
            _ => "STM32F103CB", // 与 RB 同 DEV_ID/容量，仅封装不同，烧录算法通用
        },
        0x414 => match flash_kb.unwrap_or(256) {
            0..=256 => "STM32F103RC",
            257..=384 => "STM32F103RD",
            _ => "STM32F103RE",
        },
        0x412 => "STM32F102C8",
        0x418 => "STM32F107VC",
        0x420 => match flash_kb.unwrap_or(64) {
            0..=64 => "STM32F100C8",
            _ => "STM32F100CB",
        },
        0x428 => "STM32F100RE",
        0x430 => "STM32F103ZG",
        // ---- STM32F0 ----
        0x440 => "STM32F030C8",
        0x442 => "STM32F091RC",
        0x444 => "STM32F030R8",
        0x445 => "STM32F042C6",
        0x448 => "STM32F070RB",
        // ---- STM32F4 ----
        0x413 => "STM32F407VG",
        0x419 => "STM32F439ZI",
        0x421 => "STM32F411RE",
        0x423 => "STM32F401RE",
        0x433 => "STM32F446RE",
        // ---- STM32F7 ----
        0x449 => "STM32F746ZG",
        0x451 => "STM32F767ZI",
        // ---- STM32L4 ----
        0x435 => "STM32L433RC",
        0x461 => "STM32L476RG",
        0x462 => "STM32L496ZG",
        // ---- STM32G4 ----
        0x468 => "STM32G431CB",
        0x469 => "STM32G474RE",
        0x479 => "STM32G491RE",
        // ---- STM32G0 ----
        0x460 => "STM32G070RB",
        0x466 => "STM32G031K8",
        0x467 => "STM32G0B1RE",
        // ---- STM32H7 ----
        0x450 => "STM32H743ZI",
        _ => return None,
    };
    Some(name.into())
}

/// 检查取消令牌（阶段边界）
fn check_cancel(cancel: &CancelToken) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Stm32Error::Cancelled)
    } else {
        Ok(())
    }
}

/// 按格式构造 probe-rs 镜像加载器
fn make_loader(fmt: &ImageFormat) -> Box<dyn probe_rs::flashing::ImageLoader> {
    match fmt {
        ImageFormat::Hex => Box::new(HexLoader),
        ImageFormat::Bin { base_address } => Box::new(BinLoader(BinOptions {
            base_address: *base_address,
            skip: 0,
        })),
        ImageFormat::Elf => Box::new(ElfLoader(ElfOptions::default())),
    }
}

/// 把 probe-rs 进度事件投影为 OpEvent
///
/// 按 operation 维度聚合（ProgressOperation 无 Hash/PartialEq，用 match 索引数组）：
/// AddProgressBar 记录总量，Progress 累计字节数。
fn make_progress<'a>(cb: &'a mut dyn FnMut(OpEvent)) -> FlashProgress<'a> {
    // [Fill, Erase, Program, Verify] 四个槽位：总量与已完成
    let mut totals: [Option<u64>; 4] = [None; 4];
    let mut dones: [u64; 4] = [0; 4];
    FlashProgress::new(move |event| match event {
        ProgressEvent::AddProgressBar { operation, total } => {
            let i = op_index(operation);
            totals[i] = total;
            dones[i] = 0;
        }
        ProgressEvent::Started(operation) => {
            // 阶段切换以 Started 为准：probe-rs 会先把全部 AddProgressBar 发完，
            // 若用 AddProgressBar 切阶段，会定格在最后一个操作（如“写入”）
            cb(OpEvent::StageStart {
                op: op_name(operation),
                total: totals[op_index(operation)],
            });
        }
        ProgressEvent::Progress {
            operation, size, ..
        } => {
            let i = op_index(operation);
            dones[i] += size;
            cb(OpEvent::Progress { done: dones[i] });
        }
        ProgressEvent::Finished(_) => cb(OpEvent::StageEnd),
        ProgressEvent::Failed(operation) => {
            cb(OpEvent::Message(format!("阶段失败：{}", op_name(operation))));
        }
        ProgressEvent::DiagnosticMessage { message } => cb(OpEvent::Message(message)),
        _ => {}
    })
}

/// ProgressOperation → 槽位索引
fn op_index(op: ProgressOperation) -> usize {
    match op {
        ProgressOperation::Fill => 0,
        ProgressOperation::Erase => 1,
        ProgressOperation::Program => 2,
        ProgressOperation::Verify => 3,
    }
}

/// ProgressOperation → 操作名
fn op_name(op: ProgressOperation) -> String {
    match op {
        ProgressOperation::Fill => "fill".into(),
        ProgressOperation::Erase => "erase".into(),
        ProgressOperation::Program => "program".into(),
        ProgressOperation::Verify => "verify".into(),
    }
}

// ---------------------------------------------------------------- 单元测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_targets_returns_stm32_only() {
        let all = search_targets("");
        assert!(!all.is_empty(), "内置目标库应包含 STM32 型号");
        assert!(all.iter().all(|n| n.starts_with("STM32")));
        assert!(all.iter().any(|n| n.contains("F103")), "应含 F1 系列");
    }

    #[test]
    fn search_targets_keyword_filter() {
        let v = search_targets("F103C8");
        assert_eq!(v, vec!["STM32F103C8".to_string()]);
    }

    #[test]
    fn image_format_loaders_construct() {
        // 仅验证构造不 panic（实际加载需目标会话）
        let _ = make_loader(&ImageFormat::Hex);
        let _ = make_loader(&ImageFormat::Bin {
            base_address: Some(0x0800_0000),
        });
        let _ = make_loader(&ImageFormat::Elf);
    }

    #[test]
    fn cancel_token_checked() {
        let cancel: CancelToken = Arc::new(AtomicBool::new(false));
        assert!(check_cancel(&cancel).is_ok());
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(check_cancel(&cancel), Err(Stm32Error::Cancelled)));
    }
}

#[cfg(test)]
mod identify_tests {
    use super::*;

    /// DEV_ID 表中的代表型号必须真实存在于 probe-rs 目标库
    #[test]
    fn identify_names_exist_in_registry() {
        let all = search_targets("");
        for dev_id in [
            0x410u16, 0x414, 0x412, 0x418, 0x420, 0x428, 0x430, 0x440, 0x442, 0x444, 0x445,
            0x448, 0x413, 0x419, 0x421, 0x423, 0x433, 0x449, 0x451, 0x435, 0x461, 0x462, 0x468,
            0x469, 0x479, 0x460, 0x466, 0x467, 0x450,
        ] {
            for kb in [None, Some(64), Some(256), Some(512)] {
                if let Some(name) = identify(dev_id, kb) {
                    assert!(
                        all.contains(&name),
                        "DEV_ID=0x{dev_id:03X} kb={kb:?} → {name} 不在目标库"
                    );
                }
            }
        }
    }

    #[test]
    fn identify_f1_density_split() {
        assert_eq!(identify(0x410, Some(64)).as_deref(), Some("STM32F103C8"));
        assert_eq!(identify(0x410, Some(128)).as_deref(), Some("STM32F103CB"));
        assert_eq!(identify(0x414, Some(256)).as_deref(), Some("STM32F103RC"));
        assert_eq!(identify(0x999, None), None);
    }
}
