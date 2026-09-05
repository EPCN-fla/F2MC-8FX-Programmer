//! L1 vendor 命令收发：组帧、响应解析、重试与迟到响应吸收。
//!
//! 请求: `[0x80] [CMD] [LEN_L] [LEN_H] [PAYLOAD(≤56B)]`
//! 响应: `[0x80] [STATUS] [DATA...]`

use std::time::Duration;

use crate::error::{ProgError, Result, StatusCode};
use crate::transport::DapTransport;

/// CMSIS-DAP vendor 命令首字节（DAP_Transfer vendor ID 起点）
pub const ID_DAP_VENDOR0: u8 = 0x80;
/// 单包应用层载荷上限（v1/v2 两通道取最小余量）
pub const MAX_PAYLOAD: usize = 56;

/// 发送一条 vendor 命令，返回 STATUS 之后的 DATA。
///
/// @param t 传输层
/// @param cmd L1 命令 ID
/// @param payload 应用层载荷（≤56B）
/// @param expect_len 调用方按命令语义期望的 DATA 长度（如 READ_DATA 按剩余量）；
/// `None` 时按尾部 0x00 裁剪（仅用于 PING 这类字符串响应）。
/// @param timeout 单条命令响应等待超时
/// @returns 响应 DATA 段
/// @warning 传输层格式错误（首字节≠0x80 / 缺 STATUS / 长度不符）重试一次；
/// 设备状态码错误不重试；超时也不重试——固件可能仍在执行（如 ENTER_PGM 失败
/// 路径最长 ~7 s），先吸收迟到响应再上报，避免错位成下一命令的响应。
pub fn transact<T: DapTransport + ?Sized>(
    t: &mut T,
    cmd: u8,
    payload: &[u8],
    expect_len: Option<usize>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD {
        return Err(ProgError::BadParam(format!(
            "vendor payload {} > {} bytes",
            payload.len(),
            MAX_PAYLOAD
        )));
    }

    let mut req = Vec::with_capacity(4 + payload.len());
    req.push(ID_DAP_VENDOR0);
    req.push(cmd);
    req.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    req.extend_from_slice(payload);

    let mut last_err = None;
    for attempt in 0..2 {
        match try_once(t, &req, expect_len, timeout) {
            Ok(data) => return Ok(data),
            Err(e @ ProgError::DeviceStatus(_)) | Err(e @ ProgError::SecurityLocked) => {
                // 设备状态码：按协议不得重试
                return Err(e);
            }
            Err(ProgError::Timeout) => {
                // ⚠ 超时后不得立即重发——固件可能仍在执行
                // （如 ENTER_PGM 失败路径最长 ~7 s），先吸收迟到响应再上报
                drain_late(t, cmd);
                return Err(ProgError::Timeout);
            }
            Err(e) => {
                tracing::warn!(cmd, attempt, "vendor transact retryable error: {e}");
                drain_late(t, cmd);
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap())
}

/// 吸收超时/错误后可能迟到的响应，防止错位成下一命令的响应
fn drain_late<T: DapTransport + ?Sized>(t: &mut T, cmd: u8) {
    for _ in 0..3 {
        match t.drain(Duration::from_millis(300)) {
            Ok(Some(bytes)) => {
                tracing::warn!(cmd, "drained late response ({} B)", bytes.len());
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(cmd, "drain failed: {e}");
                break;
            }
        }
    }
}

fn try_once<T: DapTransport + ?Sized>(
    t: &mut T,
    req: &[u8],
    expect_len: Option<usize>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let resp = t.dap_transfer(req, timeout)?;

    if resp.len() < 2 {
        return Err(ProgError::Transport(format!(
            "short response ({} bytes)",
            resp.len()
        )));
    }
    if resp[0] != ID_DAP_VENDOR0 {
        return Err(ProgError::Transport(format!(
            "bad response id 0x{:02X} (expect 0x80)",
            resp[0]
        )));
    }

    let status = StatusCode::from_u8(resp[1]);
    match status {
        StatusCode::Ok => {}
        StatusCode::SecurityLocked => return Err(ProgError::SecurityLocked),
        // 原命令被 ABORT(0x12) 中止：映射为用户取消
        StatusCode::Aborted => return Err(ProgError::Cancelled),
        other => return Err(ProgError::DeviceStatus(other)),
    }

    let mut data = resp[2..].to_vec();
    match expect_len {
        Some(n) => {
            if data.len() < n {
                return Err(ProgError::Transport(format!(
                    "response data {} < expected {n}",
                    data.len()
                )));
            }
            data.truncate(n);
        }
        None => {
            // 无长度语义（PING）：裁剪尾部填充 0x00
            while data.last() == Some(&0) {
                data.pop();
            }
        }
    }
    Ok(data)
}
