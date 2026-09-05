//! 错误类型：L1 设备状态码与上位机统一错误枚举。

use thiserror::Error;

/// L1 响应 STATUS 字节（响应帧 `[0x80][STATUS][DATA...]` 的第 2 字节）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    /// 0x00：成功
    Ok,
    /// 0x01：目标侧操作超时（如握手/擦除等待）
    Timeout,
    /// 0x02：目标已加安全锁
    SecurityLocked,
    /// 0x03：L2 串行通讯 ACK 错误
    AckError,
    /// 0x04：目标检查失败
    TargetChkError,
    /// 0x05：参数非法
    BadParam,
    /// 0x06：UART 帧错误
    UartError,
    /// 0x07：当前状态下不允许该命令
    StateError,
    /// 0x08：L2 帧 CRC 错误
    FrameCrcError,
    /// 0x09：固件忙
    Busy,
    /// 0x0A：命令不支持
    Unsupported,
    /// 0x0B：操作被 ABORT(0x12) 中止
    Aborted,
    /// 0x0C：目标电源故障（过载/短路/未接目标，固件已自动关断）
    PowerFault,
    /// 未定义状态码
    Unknown(u8),
}

impl StatusCode {
    /// STATUS 字节 → 枚举（未定义值落入 `Unknown`）
    pub fn from_u8(v: u8) -> Self {
        match v {
            0x00 => Self::Ok,
            0x01 => Self::Timeout,
            0x02 => Self::SecurityLocked,
            0x03 => Self::AckError,
            0x04 => Self::TargetChkError,
            0x05 => Self::BadParam,
            0x06 => Self::UartError,
            0x07 => Self::StateError,
            0x08 => Self::FrameCrcError,
            0x09 => Self::Busy,
            0x0A => Self::Unsupported,
            0x0B => Self::Aborted,
            0x0C => Self::PowerFault,
            other => Self::Unknown(other),
        }
    }
}

/// 上位机统一错误类型
#[derive(Debug, Error)]
pub enum ProgError {
    /// 设备返回非零状态码
    #[error("device status: {0:?}")]
    DeviceStatus(StatusCode),

    /// 目标已加安全锁（需整片擦除解锁）
    #[error("target is security-locked")]
    SecurityLocked,

    /// 传输层错误（USB/帧格式）
    #[error("transport error: {0}")]
    Transport(String),

    /// 命令等待响应超时（固件可能仍在执行——超时后不得立即发新命令，
    /// 必须先吸收迟到响应，见 vendor::drain_late）
    #[error("command response timeout")]
    Timeout,

    /// 烧录文件解析/清洗错误
    #[error("hex file error: {0}")]
    HexFile(String),

    /// 调用参数非法
    #[error("invalid parameter: {0}")]
    BadParam(String),

    /// 用户取消（含 ABORTED(0x0B) 映射）
    #[error("cancelled")]
    Cancelled,

    /// IO 错误
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 核心库统一结果类型
pub type Result<T> = std::result::Result<T, ProgError>;
