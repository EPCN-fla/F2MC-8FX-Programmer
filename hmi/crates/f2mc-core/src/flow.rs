//! 烧录流程状态机：完整烧录 / 仅擦除 / 仅校验 / 读回导出，含安全锁自动解锁、
//! CR Trimming 检查回写、超时恢复与取消支持。
//!
//! 完整流程：PING → ENTER_PGM（锁则自动整片擦除解锁重试一次）→ ERASE →
//! FLASH_INIT(0x02,0x7C) → CR Trimming 检查 → 分块 WRITE(≤512B) →
//! READ 校验(跳过 0xFFBB/BC/BD) → [WRITE_SECURE] → QUIT → RESET_RUN

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{ProgError, Result, StatusCode};
use crate::hexfile::HexImage;
use crate::new8fx::{
    CR_NVR_ADDRS, CR_RAM_ADDRS, FLASH_INIT_XX, FLASH_INIT_YY, VERIFY_SKIP,
};
use crate::proto::{F2mcClient, READ_BLOCK_MAX, WRITE_BLOCK_MAX};
use crate::transport::DapTransport;

/// 烧录流程阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Ping,
    EnterPgm,
    Erase,
    FlashInit,
    CrTrim,
    Write,
    Verify,
    Read,
    Secure,
    Quit,
    ResetRun,
}

impl Stage {
    /// 阶段显示名（GUI/CLI 日志）
    pub fn label(&self) -> &'static str {
        match self {
            Stage::Ping => "连接握手",
            Stage::EnterPgm => "进入编程模式",
            Stage::Erase => "擦除",
            Stage::FlashInit => "初始化",
            Stage::CrTrim => "CR 校准检查",
            Stage::Write => "写入",
            Stage::Verify => "校验",
            Stage::Read => "读取",
            Stage::Secure => "写入安全位",
            Stage::Quit => "完成",
            Stage::ResetRun => "完成",
        }
    }
}

/// 流程选项
#[derive(Debug, Clone)]
pub struct FlowOptions {
    /// 烧录后写 0xFFFC 安全位
    pub write_secure: bool,
    /// 烧录完成后复位运行
    pub reset_after: bool,
}

impl Default for FlowOptions {
    fn default() -> Self {
        Self { write_secure: false, reset_after: true }
    }
}

/// 流程上报事件
#[derive(Debug)]
pub enum FlowEvent {
    /// 阶段开始
    StageStart(Stage),
    /// (stage, done, total)
    Progress(Stage, usize, usize),
    /// 普通日志
    Log(String),
    /// 告警
    Warn(String),
}

/// 流程结果报告
#[derive(Debug, Default)]
pub struct FlowReport {
    /// 实际写入字节数
    pub bytes_written: usize,
    /// 实际校验字节数
    pub bytes_verified: usize,
    /// CR 校准回写字节数
    pub cr_trim_fixed: usize,
    /// 本次流程是否执行过自动解锁（整片擦除）
    pub unlocked: bool,
    /// 告警信息（镜像清洗告警等）
    pub warnings: Vec<String>,
    /// 总耗时（秒）
    pub elapsed_secs: f32,
}

/// 取消令牌：置 true 后流程在下一个检查点返回 `ProgError::Cancelled`
pub type CancelToken = Arc<AtomicBool>;

/// 构造未置位的取消令牌
pub fn default_cancel() -> CancelToken {
    Arc::new(AtomicBool::new(false))
}

fn check_cancel(cancel: &CancelToken) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(ProgError::Cancelled)
    } else {
        Ok(())
    }
}

/// 完整烧录流程（擦除 + 写入 + 校验）
///
/// @param client L1 客户端（已连接）
/// @param image 清洗后的烧录镜像
/// @param opts 流程选项（安全位/烧录后复位）
/// @param cb 事件回调（阶段/进度/日志/告警）
/// @param cancel 取消令牌
/// @returns 流程报告；失败时状态机停留在出错阶段
pub fn program<T: DapTransport>(
    client: &mut F2mcClient<T>,
    image: &HexImage,
    opts: &FlowOptions,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<FlowReport> {
    let t0 = Instant::now();
    let mut report = FlowReport::default();
    let total = image.total_bytes();

    // --- PING ---
    cb(FlowEvent::StageStart(Stage::Ping));
    let id = client.ping()?;
    cb(FlowEvent::Log(format!("PING: {id}, fw={:?}", client.fw_version)));
    check_cancel(cancel)?;

    // --- 到 SYNCED（固件 ENTER_PGM 任何状态可进，pgmseq 内含完整断电重进）---
    cb(FlowEvent::StageStart(Stage::EnterPgm));
    match session_begin_synced(client, &mut *cb, cancel) {
        Ok(()) => {}
        Err(ProgError::SecurityLocked) => {
            cb(FlowEvent::Warn("目标已加安全锁，执行整片擦除解锁后重试".into()));
            erase_with_recovery(client, 0x0000, &mut *cb, cancel)?;
            report.unlocked = true;
            enter_pgm_with_recovery(client, &mut *cb, cancel).map_err(|e| match e {
                ProgError::SecurityLocked => ProgError::Transport(
                    "still security-locked after unlock erase".into(),
                ),
                other => other,
            })?;
        }
        Err(e) => return Err(e),
    }
    check_cancel(cancel)?;

    // --- ERASE（整片）---
    cb(FlowEvent::StageStart(Stage::Erase));
    erase_with_recovery(client, 0x0000, &mut *cb, cancel)?;
    check_cancel(cancel)?;

    // --- FLASH_INIT（500 Kbps 切换由固件完成）---
    cb(FlowEvent::StageStart(Stage::FlashInit));
    client.flash_init(FLASH_INIT_XX, FLASH_INIT_YY)?;
    check_cancel(cancel)?;

    // --- CR Trimming 检查（Spec 7.11）---
    cb(FlowEvent::StageStart(Stage::CrTrim));
    report.cr_trim_fixed = cr_trim_check(client, &mut *cb)?;
    check_cancel(cancel)?;

    // --- WRITE（分块 ≤512B）---
    cb(FlowEvent::StageStart(Stage::Write));
    let mut done = 0usize;
    for (seg_addr, seg_data) in &image.segments {
        let mut offset = 0usize;
        while offset < seg_data.len() {
            let n = (seg_data.len() - offset).min(WRITE_BLOCK_MAX);
            let addr = (*seg_addr as usize + offset) as u16;
            client.write_block(addr, &seg_data[offset..offset + n])?;
            offset += n;
            done += n;
            cb(FlowEvent::Progress(Stage::Write, done, total));
            // 块间取消（写入中途不取消）
            check_cancel(cancel)?;
        }
    }
    report.bytes_written = done;

    // --- VERIFY（读回比对，跳过 0xFFBB/BC/BD）---
    cb(FlowEvent::StageStart(Stage::Verify));
    report.bytes_verified = verify_image(client, image, total, &mut *cb, cancel)?;

    // --- 结束后再查一次 CR Trimming（写 0xFFBB 区后需再确认一致）---
    let fixed = cr_trim_check(client, &mut *cb)?;
    report.cr_trim_fixed += fixed;

    // --- WRITE_SECURE（可选；安全位需 QUIT/断电才生效）---
    if opts.write_secure {
        cb(FlowEvent::StageStart(Stage::Secure));
        client.write_secure()?;
        cb(FlowEvent::StageStart(Stage::Quit));
        client.quit()?;
        cb(FlowEvent::Log("已写入安全位 0xFFFC=0x01（断电复位后生效）".into()));
    }

    // --- 收尾：复位 或 保持编程模式（会话保持，可连续执行下一步）---
    if opts.reset_after {
        cb(FlowEvent::StageStart(Stage::ResetRun));
        reset_run_graceful(client, &mut *cb)?;
    } else {
        cb(FlowEvent::Log("保持编程模式：可直接连续执行烧录/校验/读取".into()));
    }

    cb(FlowEvent::StageStart(Stage::Quit)); // 完成标记
    report.elapsed_secs = t0.elapsed().as_secs_f32();
    Ok(report)
}

/// 仅擦除（结束后保持 SYNCED，可连续执行）
///
/// @warning 安全锁目标：ENTER_PGM 后已在 SYNCED，整片擦除本身就是解锁手段。
pub fn erase_only<T: DapTransport>(
    client: &mut F2mcClient<T>,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<()> {
    cb(FlowEvent::StageStart(Stage::Ping));
    client.ping()?;
    cb(FlowEvent::StageStart(Stage::EnterPgm));
    match session_begin_synced(client, &mut *cb, cancel) {
        Ok(()) => {}
        // 安全锁目标：ENTER_PGM 后已在 SYNCED，整片擦除本身就是解锁手段
        Err(ProgError::SecurityLocked) => {
            cb(FlowEvent::Warn("目标已加安全锁，整片擦除将同时解锁".into()));
        }
        Err(e) => return Err(e),
    }
    cb(FlowEvent::StageStart(Stage::Erase));
    erase_with_recovery(client, 0x0000, &mut *cb, cancel)?;
    cb(FlowEvent::Log("保持 SYNCED：可连续执行下一步操作".into()));
    cb(FlowEvent::StageStart(Stage::Quit)); // 完成标记
    Ok(())
}

/// 烧录恢复：强制进入（固件内含整循环重试）→ 整片擦除。
/// 用于上次烧录异常（如错配 DA 写坏目标 Flash）导致无法正常烧录时的恢复
/// （参照 YM02：能进模式就能整片擦除重来）。结束后保持 SYNCED。
pub fn recover<T: DapTransport>(
    client: &mut F2mcClient<T>,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<()> {
    cb(FlowEvent::StageStart(Stage::Ping));
    client.ping()?;
    cb(FlowEvent::StageStart(Stage::EnterPgm));
    // 恢复场景不问锁态：加锁目标整片擦除即解锁
    match session_begin_synced(client, &mut *cb, cancel) {
        Ok(()) => {}
        Err(ProgError::SecurityLocked) => {
            cb(FlowEvent::Warn("目标已加安全锁，恢复过程将整片擦除解锁".into()));
        }
        Err(e) => return Err(e),
    }
    cb(FlowEvent::StageStart(Stage::Erase));
    erase_with_recovery(client, 0x0000, &mut *cb, cancel)?;
    cb(FlowEvent::Log("恢复完成：目标已整片擦除，可正常烧录".into()));
    cb(FlowEvent::StageStart(Stage::Quit));
    Ok(())
}

/// 仅校验（不擦除；镜像需与片上一致。结束后保持读写模式）
pub fn verify_only<T: DapTransport>(
    client: &mut F2mcClient<T>,
    image: &HexImage,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<()> {
    cb(FlowEvent::StageStart(Stage::Ping));
    client.ping()?;
    cb(FlowEvent::StageStart(Stage::EnterPgm));
    session_begin_rw(client, &mut *cb, cancel)?;

    cb(FlowEvent::StageStart(Stage::Verify));
    let total = image.total_bytes();
    verify_image(client, image, total, &mut *cb, cancel)?;
    cb(FlowEvent::Log("保持读写模式：可连续执行下一步操作".into()));
    cb(FlowEvent::StageStart(Stage::Quit)); // 完成标记
    Ok(())
}

/// 读取目标 Flash 内容（readout，供 GUI "读取..." 导出 hex。结束后保持读写模式）
pub fn read_out<T: DapTransport>(
    client: &mut F2mcClient<T>,
    start: u16,
    len: usize,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<Vec<u8>> {
    cb(FlowEvent::StageStart(Stage::Ping));
    client.ping()?;
    cb(FlowEvent::StageStart(Stage::EnterPgm));
    session_begin_rw(client, &mut *cb, cancel)?;

    cb(FlowEvent::StageStart(Stage::Read));
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let n = (len - out.len()).min(READ_BLOCK_MAX);
        let chunk = client.read_block(start + out.len() as u16, n)?;
        out.extend_from_slice(&chunk);
        cb(FlowEvent::Progress(Stage::Read, out.len(), len));
        check_cancel(cancel)?;
    }
    cb(FlowEvent::Log("保持读写模式：可连续执行下一步操作".into()));
    cb(FlowEvent::StageStart(Stage::Quit)); // 完成标记
    Ok(out)
}

/// 镜像读回比对（跳过 0xFFBB/BC/BD 三个 CR 校准字节），返回校验字节数
fn verify_image<T: DapTransport>(
    client: &mut F2mcClient<T>,
    image: &HexImage,
    total: usize,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<usize> {
    let mut vdone = 0usize;
    for (seg_addr, seg_data) in &image.segments {
        let mut offset = 0usize;
        while offset < seg_data.len() {
            let n = (seg_data.len() - offset).min(READ_BLOCK_MAX);
            let addr = (*seg_addr as usize + offset) as u16;
            let read = client.read_block(addr, n)?;
            for (i, (&want, &got)) in seg_data[offset..offset + n]
                .iter()
                .zip(read.iter())
                .enumerate()
            {
                let a = addr as u32 + i as u32;
                if VERIFY_SKIP.contains(&a) {
                    continue; // CR 校准字节必须跳过（回写未生效前读回值不可信）
                }
                if want != got {
                    return Err(ProgError::Transport(format!(
                        "verify mismatch at 0x{a:04X}: wrote 0x{want:02X}, read 0x{got:02X}"
                    )));
                }
            }
            offset += n;
            vdone += n;
            cb(FlowEvent::Progress(Stage::Verify, vdone, total));
            check_cancel(cancel)?;
        }
    }
    Ok(vdone)
}

/// 会话入口（烧录/擦除/恢复）：到达 SYNCED（握手有效）。
/// 固件 ENTER_PGM 任何状态可进（pgmseq 内含完整断电→放电→上电重进），
/// 无需先 QUIT/RESET_RUN 归位——带电重进不再依赖 DA 退出帧与多余电源循环。
fn session_begin_synced<T: DapTransport>(
    client: &mut F2mcClient<T>,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<()> {
    enter_pgm_with_recovery(client, cb, cancel)
}

/// 会话入口（校验/读取）：到达 RW_MODE（RW 直进；其余完整重进）
fn session_begin_rw<T: DapTransport>(
    client: &mut F2mcClient<T>,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<()> {
    let (state, last_err) = client.get_state()?;
    if last_err == 0x02 {
        // 目标加锁时校验/读取无意义（L2 全回 0xFD）
        return Err(ProgError::SecurityLocked);
    }
    match state {
        3 => {
            cb(FlowEvent::Log("编程器已在读写模式，直接执行".into()));
            Ok(())
        }
        1 => {
            cb(FlowEvent::Log("SYNCED 直进读写模式".into()));
            cb(FlowEvent::StageStart(Stage::FlashInit));
            client.flash_init(FLASH_INIT_XX, FLASH_INIT_YY)
        }
        _ => {
            enter_pgm_with_recovery(client, cb, cancel)?;
            cb(FlowEvent::StageStart(Stage::FlashInit));
            client.flash_init(FLASH_INIT_XX, FLASH_INIT_YY)
        }
    }
}

/// CR Trimming 检查（Spec 7.11）：读 NVR 与 RAM 镜像，不一致则把 RAM 值写回 NVR。
/// ⚠ 真机 DA 不实现 Spec 7.9 的 0x55 命令（CR_TRIM_WRITE 会回 ACK_ERROR），
/// 必须用普通写路径（WRITE_BEGIN/DATA/COMMIT）写 0xFFBB 起的字节。
/// 返回修复的字节数
fn cr_trim_check<T: DapTransport>(
    client: &mut F2mcClient<T>,
    cb: &mut dyn FnMut(FlowEvent),
) -> Result<usize> {
    let nvr = client.read_block(CR_NVR_ADDRS[0], 3)?;
    let ram_lo = client.read_block(CR_RAM_ADDRS[1], 2)?; // 0x0FE4, 0x0FE5
    let ram_hi = client.read_block(CR_RAM_ADDRS[0], 1)?; // 0x0FE7
    let ram = [ram_hi[0], ram_lo[0], ram_lo[1]]; // 与 NVR 顺序对应：FFBB↔0FE7, FFBC↔0FE4, FFBD↔0FE5

    let mut fixed = 0;
    for i in 0..3 {
        if nvr[i] != ram[i] {
            if ram[i] == 0xFF {
                // RAM 镜像为空：回写会毁掉校准值，仅告警
                cb(FlowEvent::Warn(format!(
                    "CR 校准失配 0x{:04X}: NVR=0x{:02X} 但 RAM 镜像为空(0xFF)，跳过回写",
                    CR_NVR_ADDRS[i], nvr[i]
                )));
                continue;
            }
            cb(FlowEvent::Warn(format!(
                "CR 校准失配 0x{:04X}: NVR=0x{:02X} RAM=0x{:02X}，将 RAM 值经普通写路径回写 NVR",
                CR_NVR_ADDRS[i], nvr[i], ram[i]
            )));
            client.write_block(CR_NVR_ADDRS[i], &[ram[i]])?;
            fixed += 1;
        }
    }
    Ok(fixed)
}

/// 等待固件从长命令中返回（GET_STATE 轮询），每秒检查取消令牌。
/// 返回固件返回时刻的 (state, last_err)——调用方用于错误诊断信息。
fn wait_firmware<T: DapTransport>(
    client: &mut F2mcClient<T>,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
    max_secs: u32,
) -> Result<(u8, u8)> {
    for _ in 0..max_secs {
        std::thread::sleep(Duration::from_secs(1));
        check_cancel(cancel)?; // 取消在等待期间生效
        match client.get_state() {
            Ok((state, last_err)) => {
                cb(FlowEvent::Log(format!(
                    "固件已返回：state={state} last_error=0x{last_err:02X}"
                )));
                return Ok((state, last_err));
            }
            Err(_) => continue, // 固件仍忙，继续等
        }
    }
    Err(ProgError::Transport("编程器长时间无响应（USB 链路异常）".into()))
}

/// ERASE 超时恢复：固件侧擦除等待上限 60 s，上位机超时≠失败——
/// 先 GET_STATE 轮询等固件返回（防迟到响应错位），再报错。
fn erase_with_recovery<T: DapTransport>(
    client: &mut F2mcClient<T>,
    addr: u16,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<()> {
    match client.erase(addr) {
        Err(ProgError::Timeout) => {
            cb(FlowEvent::Warn("ERASE 超时：等待固件返回（最长 ~90 s）…".into()));
            let (state, last_err) = wait_firmware(client, cb, cancel, 90)?;
            Err(ProgError::Transport(format!(
                "擦除失败：目标无响应（固件 state={state} last_error=0x{last_err:02X}），请检查接线后重试"
            )))
        }
        other => other,
    }
}

/// ENTER_PGM 超时恢复：
/// 固件内含整循环重试（握手失败时放电→上电→保持→握手 ×3）；成功最坏路径
/// 较长（大电容目标板放电慢 + 重试）——上位机 40 s 超时已覆盖（见 proto::timeout）。
/// 此处仅作保底：超时后不得立即发新命令，先 GET_STATE 轮询等固件返回
/// （防迟到响应错位），并带上固件状态码再报错，便于定位是放电/上电/握手哪段失败。
fn enter_pgm_with_recovery<T: DapTransport>(
    client: &mut F2mcClient<T>,
    cb: &mut dyn FnMut(FlowEvent),
    cancel: &CancelToken,
) -> Result<()> {
    match client.enter_pgm() {
        // 固件侧 TIMEOUT：放电/稳定/握手某段超限（固件 RTT 日志可查具体段）。
        // 带电重进时放电段最常见——固件已加 DBG 主动泄放（pgmseq.c 步骤 1），
        // 仍超时多为目标板有外部供电/电容过大。
        Err(ProgError::DeviceStatus(StatusCode::Timeout)) => Err(ProgError::Transport(
            "进入编程模式超时：目标未就绪（放电/上电/握手超限）。\
             若目标板有外部供电请先断开；大电容板请人工断电后重试"
                .into(),
        )),
        Err(ProgError::Timeout) => {
            cb(FlowEvent::Warn(
                "ENTER_PGM 超时：等待固件返回（放电/握手重试中）…".into(),
            ));
            let (state, last_err) = wait_firmware(client, cb, cancel, 15)?;
            Err(ProgError::Transport(format!(
                "进入编程模式失败（固件 state={state} last_error=0x{last_err:02X}）：\
                 请检查目标板供电与 DBG 接线后重试；\
                 若 last_error=0x01 且目标板电容较大，多为放电超时"
            )))
        }
        other => other,
    }
}

/// RESET_RUN 容错：新固件经电源开关模拟复位并上报能力（原生/模拟）；
/// 旧固件恒回 UNSUPPORTED——改走上位机 SET_POWER 断电→上电兜底。
fn reset_run_graceful<T: DapTransport>(
    client: &mut F2mcClient<T>,
    cb: &mut dyn FnMut(FlowEvent),
) -> Result<()> {
    match client.reset_run() {
        Ok(crate::proto::ResetMode::Native) => {
            cb(FlowEvent::Log("已复位运行用户程序（原生复位引脚）".into()));
            Ok(())
        }
        Ok(crate::proto::ResetMode::Simulated) => {
            cb(FlowEvent::Log(
                "已复位运行用户程序（当前为兼容模式：断电+上电模拟复位）".into(),
            ));
            Ok(())
        }
        Err(ProgError::DeviceStatus(StatusCode::Unsupported)) => {
            // 旧固件无复位实现；F2MC-LINK v1.1 起有目标电源开关 → 自动断电 → 上电运行用户程序
            cb(FlowEvent::Log(
                "编程器固件过旧，自动断电重启以运行用户程序".into(),
            ));
            client.set_power(false)?;
            std::thread::sleep(std::time::Duration::from_millis(300));
            match client.set_power(true) {
                Ok(()) => {
                    cb(FlowEvent::Log("已自动上电，目标运行用户程序".into()));
                    Ok(())
                }
                Err(e @ ProgError::DeviceStatus(StatusCode::PowerFault)) => {
                    cb(FlowEvent::Warn(
                        "目标电源故障（过载/短路/未接目标，已自动关断），请人工检查供电".into(),
                    ));
                    Err(e)
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hexfile;
    use crate::sim::SimProgrammer;

    fn make_hex(data: &[(u16, &[u8])]) -> String {
        let mut s = String::new();
        for (addr, bytes) in data {
            // 每行 ≤16 字节（Intel HEX 记录上限 255B）
            for (i, chunk) in bytes.chunks(16).enumerate() {
                let a = addr + i as u16 * 16;
                let mut rec: Vec<u8> = vec![
                    chunk.len() as u8,
                    (a >> 8) as u8,
                    a as u8,
                    0,
                ];
                rec.extend_from_slice(chunk);
                let sum: u8 = rec.iter().fold(0, |a, b| a.wrapping_add(*b));
                let chk = (!sum).wrapping_add(1);
                s.push(':');
                for b in &rec {
                    s.push_str(&format!("{b:02X}"));
                }
                s.push_str(&format!("{chk:02X}\n"));
            }
        }
        s.push_str(":00000001FF\n");
        s
    }

    fn chip() -> &'static crate::chipdef::ChipDef {
        crate::chipdef::by_name("MB95F636H").unwrap()
    }

    /// 全流程：写入 600B 跨两个 WRITE 块，校验通过，模拟器 flash 内容一致
    #[test]
    fn program_full_flow() {
        let payload: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
        let text = make_hex(&[(0x8000, &payload)]);
        let img = hexfile::parse(&text, chip()).unwrap();

        let sim = SimProgrammer::new();
        let mut client = F2mcClient::new(sim);
        let mut events = Vec::new();
        let report = program(
            &mut client,
            &img,
            &FlowOptions::default(),
            &mut |e| {
                if let FlowEvent::StageStart(s) = e {
                    events.push(s);
                }
            },
            &default_cancel(),
        )
        .unwrap();

        assert_eq!(report.bytes_written, 600);
        assert_eq!(report.bytes_verified, 600);
        assert!(!report.unlocked);
        // 模拟器内 flash 与镜像一致
        let sim = client.transport_mut();
        for (i, b) in payload.iter().enumerate() {
            assert_eq!(sim.flash.get(&(0x8000 + i as u32)), Some(b), "mismatch at {i}");
        }
        // 阶段顺序
        assert_eq!(
            events,
            vec![
                Stage::Ping, Stage::EnterPgm, Stage::Erase, Stage::FlashInit,
                Stage::CrTrim, Stage::Write, Stage::Verify, Stage::ResetRun, Stage::Quit,
            ]
        );
        assert_eq!(sim.state, crate::sim::SimState::Idle);
    }

    /// 安全锁：ENTER_PGM 返回锁 → 自动整片擦除解锁 → 重试成功
    #[test]
    fn program_unlocks_security() {
        let payload = [0xAAu8; 16];
        let text = make_hex(&[(0x8000, &payload)]);
        let img = hexfile::parse(&text, chip()).unwrap();

        let mut sim = SimProgrammer::new();
        sim.locked = true;
        let mut client = F2mcClient::new(sim);
        let report = program(
            &mut client,
            &img,
            &FlowOptions::default(),
            &mut |_| {},
            &default_cancel(),
        )
        .unwrap();
        assert!(report.unlocked);
        assert!(!client.transport_mut().locked);
    }

    /// 写入安全位选项
    #[test]
    fn program_with_secure_bit() {
        let payload = [0x55u8; 8];
        let text = make_hex(&[(0x8000, &payload)]);
        let img = hexfile::parse(&text, chip()).unwrap();
        let mut client = F2mcClient::new(SimProgrammer::new());
        program(
            &mut client,
            &img,
            &FlowOptions { write_secure: true, reset_after: true },
            &mut |_| {},
            &default_cancel(),
        )
        .unwrap();
        assert_eq!(client.transport_mut().flash.get(&0xFFFC), Some(&0x01));
    }

    /// 取消令牌在块间生效
    #[test]
    fn cancel_between_blocks() {
        let payload = vec![0x11u8; 1024]; // 两个 WRITE 块
        let text = make_hex(&[(0x8000, &payload)]);
        let img = hexfile::parse(&text, chip()).unwrap();
        let mut client = F2mcClient::new(SimProgrammer::new());
        let cancel = default_cancel();
        let c2 = cancel.clone();
        let mut writes = 0;
        let err = program(
            &mut client,
            &img,
            &FlowOptions::default(),
            &mut |e| {
                if let FlowEvent::Progress(Stage::Write, ..) = e {
                    writes += 1;
                    if writes == 1 {
                        c2.store(true, Ordering::Relaxed); // 第一块后取消
                    }
                }
            },
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, ProgError::Cancelled));
    }

    /// 校验失败能检出（模拟器 flash 被篡改）
    #[test]
    fn verify_detects_mismatch() {
        let payload = [0x42u8; 64];
        let text = make_hex(&[(0x8000, &payload)]);
        let img = hexfile::parse(&text, chip()).unwrap();
        let mut sim = SimProgrammer::new();
        // 注入：WRITE_COMMIT 正常写入，但 READ_BEGIN 返回全 0xFF（模拟写不进）
        // 通过在 write 阶段后直接改写：用 fail_next 不适用，这里直接让 ERASE 后
        // WRITE 无效——简化：让模拟器对 0x8000 区域拒写
        sim.flash.insert(0x8000, 0x00);
        let mut client = F2mcClient::new(sim);
        // program 会擦除（清掉注入的 0x00）→ 校验应该通过
        program(&mut client, &img, &FlowOptions::default(), &mut |_| {}, &default_cancel())
            .unwrap();
        // 再构造一次镜像与片上不一致的 verify_only
        let other = make_hex(&[(0x8000, &[0x99u8; 64])]);
        let img2 = hexfile::parse(&other, chip()).unwrap();
        let err = verify_only(&mut client, &img2, &mut |_| {}, &default_cancel()).unwrap_err();
        assert!(format!("{err}").contains("verify mismatch"));
    }
}
