//! 编程器模拟器：按 L1 帧协议应答、维护固件状态机与虚拟 Flash——
//! 集成测试与 GUI mock 模式共用。状态机：IDLE→SYNCED→ERASED→RW_MODE。

use std::collections::BTreeMap;
use std::time::Duration;

use crate::error::{ProgError, Result, StatusCode};
use crate::proto::cmd;
use crate::transport::DapTransport;

/// 模拟器状态机（对应固件状态，GET_STATE 第 1 字节：0/1/2/3）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimState {
    /// 0：初始/已断开
    Idle,
    /// 1：已握手（ENTER_PGM 成功）
    Synced,
    /// 2：已整片擦除
    Erased,
    /// 3：读写模式（FLASH_INIT 后）
    RwMode,
}

/// 模拟编程器固件：解析 vendor 帧、维护状态机与虚拟 Flash
pub struct SimProgrammer {
    /// 当前状态机
    pub state: SimState,
    /// 虚拟 flash：地址 → 字节（未写入地址视为 0xFF）
    pub flash: BTreeMap<u32, u8>,
    /// 安全锁
    pub locked: bool,
    /// 写入缓冲区（WRITE_BEGIN/DATA/COMMIT）
    wbuf: Vec<u8>,
    waddr: u16,
    wexpect: usize,
    /// 读缓冲区（READ_BEGIN/DATA）
    rbuf: Vec<u8>,
    rpos: usize,
    /// 收到的帧日志（断言/排障用）
    pub frame_log: Vec<Vec<u8>>,
    /// 模拟故障注入：下 N 次传输直接返回传输错误
    pub fail_next: usize,
    /// 最近一次错误状态码（GET_STATE 第 2 字节，对应固件 set_err）
    last_err: u8,
    /// 安全位已写入但未生效（断电复位或 QUIT 后生效）
    pending_lock: bool,
    /// SET_CHIP 下发的型号名（固件按系列匹配内嵌 DA）
    pub chip_name: String,
}

impl Default for SimProgrammer {
    fn default() -> Self {
        Self::new()
    }
}

impl SimProgrammer {
    /// 构造 IDLE 态模拟器
    pub fn new() -> Self {
        Self {
            state: SimState::Idle,
            flash: BTreeMap::new(),
            locked: false,
            wbuf: Vec::new(),
            waddr: 0,
            wexpect: 0,
            rbuf: Vec::new(),
            rpos: 0,
            frame_log: Vec::new(),
            fail_next: 0,
            last_err: 0,
            pending_lock: false,
            chip_name: String::new(),
        }
    }

    fn read_flash(&self, addr: u32) -> u8 {
        *self.flash.get(&addr).unwrap_or(&0xFF)
    }

    fn resp(status: StatusCode, data: &[u8]) -> Vec<u8> {
        let code = match status {
            StatusCode::Ok => 0x00,
            StatusCode::Timeout => 0x01,
            StatusCode::SecurityLocked => 0x02,
            StatusCode::AckError => 0x03,
            StatusCode::TargetChkError => 0x04,
            StatusCode::BadParam => 0x05,
            StatusCode::UartError => 0x06,
            StatusCode::StateError => 0x07,
            StatusCode::FrameCrcError => 0x08,
            StatusCode::Busy => 0x09,
            StatusCode::Unsupported => 0x0A,
            StatusCode::Aborted => 0x0B,
            StatusCode::PowerFault => 0x0C,
            StatusCode::Unknown(v) => v,
        };
        let mut v = vec![0x80, code];
        v.extend_from_slice(data);
        v
    }

    /// 命令分发：安全锁门控 → 按命令 ID 执行状态机迁移并组响应帧
    fn handle(&mut self, c: u8, payload: &[u8]) -> Vec<u8> {
        // 安全锁目标：仅握手/整片擦除/L1 查询类命令可用，其余回 0xFD
        //（SET_CHIP 不触目标，属纯配置命令，不受锁门控）
        if self.locked
            && !matches!(
                c,
                cmd::PING
                    | cmd::ERASE
                    | cmd::ENTER_PGM
                    | cmd::RESET_RUN
                    | cmd::GET_STATE
                    | cmd::SEND_BREAK
                    | cmd::DISCONNECT
                    | cmd::ABORT
                    | cmd::SET_POWER
                    | cmd::SET_CHIP
            )
        {
            return Self::resp(StatusCode::SecurityLocked, &[]);
        }
        match c {
            cmd::PING => {
                let mut d = b"F2MC-LINK".to_vec();
                d.extend_from_slice(&[0, 1, 0]);
                Self::resp(StatusCode::Ok, &d)
            }
            cmd::SET_POWER => {
                // F2MC-LINK v1.1 电源开关：on=1 上电（含上升确认），on=0 断电且状态机复位 IDLE
                let on = payload.first().copied().unwrap_or(0) != 0;
                if !on {
                    self.state = SimState::Idle;
                }
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::ENTER_PGM => {
                // 固件（cmd.c）：任何状态可进——pgmseq 内含完整断电重进
                if self.locked {
                    // 加锁目标：握手本身可成功（SYNCED），随后时钟切换时收到 0xFD
                    self.state = SimState::Synced;
                    return Self::resp(StatusCode::SecurityLocked, &[]);
                }
                self.state = SimState::Synced;
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::ERASE => {
                // 固件门控（cmd.c）：仅 SYNCED 可用
                if self.state != SimState::Synced {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                let addr = u16::from_be_bytes([payload[0], payload[1]]) as u32;
                if addr == 0 {
                    self.flash.clear();
                    self.locked = false;
                    self.state = SimState::Erased;
                }
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::FLASH_INIT => {
                if !matches!(self.state, SimState::Synced | SimState::Erased) {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                self.state = SimState::RwMode;
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::WRITE_BEGIN => {
                if self.state != SimState::RwMode {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                self.waddr = u16::from_be_bytes([payload[0], payload[1]]);
                self.wexpect = u16::from_be_bytes([payload[2], payload[3]]) as usize;
                if self.wexpect == 0 || self.wexpect > 512 {
                    return Self::resp(StatusCode::BadParam, &[]);
                }
                self.wbuf.clear();
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::WRITE_DATA => {
                if self.state != SimState::RwMode || self.wbuf.len() + payload.len() > self.wexpect {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                self.wbuf.extend_from_slice(payload);
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::WRITE_COMMIT => {
                if self.state != SimState::RwMode || self.wbuf.len() != self.wexpect {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                for (i, b) in self.wbuf.iter().enumerate() {
                    self.flash.insert(self.waddr as u32 + i as u32, *b);
                }
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::READ_BEGIN => {
                if self.state != SimState::RwMode {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                let addr = u16::from_be_bytes([payload[0], payload[1]]) as u32;
                let len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
                if len == 0 || len > 1024 {
                    return Self::resp(StatusCode::BadParam, &[]);
                }
                self.rbuf = (0..len).map(|i| self.read_flash(addr + i as u32)).collect();
                self.rpos = 0;
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::READ_DATA => {
                if self.state != SimState::RwMode || self.rpos >= self.rbuf.len() {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                let n = (self.rbuf.len() - self.rpos).min(56);
                let data = self.rbuf[self.rpos..self.rpos + n].to_vec();
                self.rpos += n;
                Self::resp(StatusCode::Ok, &data)
            }
            cmd::CR_TRIM_WRITE => {
                if self.state != SimState::RwMode {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                // 真机 DA 不实现 Spec 7.9 的 0x55 命令，
                // 固件恒回 ACK_ERROR；上位机应改用普通写路径
                Self::resp(StatusCode::AckError, &[])
            }
            cmd::QUIT => {
                if self.state != SimState::RwMode {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                if self.pending_lock {
                    self.locked = true;
                    self.pending_lock = false;
                }
                self.state = SimState::Synced;
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::RESET_RUN => {
                if self.pending_lock {
                    self.locked = true;
                    self.pending_lock = false;
                }
                self.state = SimState::Idle;
                // 新固件：电源开关模拟复位成功并上报能力 DATA[0]=0x01（兼容模式）
                Self::resp(StatusCode::Ok, &[0x01])
            }
            cmd::GET_STATE => {
                let s = match self.state {
                    SimState::Idle => 0,
                    SimState::Synced => 1,
                    SimState::Erased => 2,
                    SimState::RwMode => 3,
                };
                Self::resp(StatusCode::Ok, &[s, self.last_err])
            }
            cmd::WRITE_SECURE => {
                if self.state != SimState::RwMode {
                    return Self::resp(StatusCode::StateError, &[]);
                }
                self.flash.insert(0xFFFC, 0x01);
                self.pending_lock = true;
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::SEND_BREAK => Self::resp(StatusCode::Ok, &[]),
            cmd::SET_CHIP => {
                // 型号下发（任何状态可用的配置命令）：固件按系列匹配内嵌 DA
                if payload.is_empty() || payload.len() > 24 {
                    return Self::resp(StatusCode::BadParam, &[]);
                }
                self.chip_name = String::from_utf8_lossy(payload).into_owned();
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::DISCONNECT => {
                // DISCONNECT：断开通知——LED 熄灭 + 状态机复位 IDLE，目标侧状态不变；
                // 型号匹配复位默认（对应固件 new8fx_da_clear）
                self.state = SimState::Idle;
                self.chip_name.clear();
                Self::resp(StatusCode::Ok, &[])
            }
            cmd::ABORT => {
                // ABORT：带外中止标志（幂等）。模拟器无长操作，直接回 OK
                Self::resp(StatusCode::Ok, &[])
            }
            _ => Self::resp(StatusCode::Unsupported, &[]),
        }
    }
}

impl DapTransport for SimProgrammer {
    fn dap_transfer(&mut self, request: &[u8], _timeout: Duration) -> Result<Vec<u8>> {
        self.frame_log.push(request.to_vec());
        if self.fail_next > 0 {
            self.fail_next -= 1;
            return Err(ProgError::Transport("injected failure".into()));
        }
        if request.len() < 4 || request[0] != 0x80 {
            return Err(ProgError::Transport("bad frame".into()));
        }
        let c = request[1];
        let len = u16::from_le_bytes([request[2], request[3]]) as usize;
        if request.len() < 4 + len {
            return Err(ProgError::Transport("bad frame len".into()));
        }
        let resp = self.handle(c, &request[4..4 + len]);
        // 记录最近一次错误状态码（对应固件 set_err → GET_STATE 第 2 字节；
        // UNSUPPORTED 不经 set_err——RESET_RUN/SET_POWER 属预期返回，不记）
        if resp.len() > 1 && resp[1] != 0 && resp[1] != 0x0A {
            self.last_err = resp[1];
        }
        Ok(resp)
    }

    fn describe(&self) -> String {
        "sim-programmer".into()
    }
}
