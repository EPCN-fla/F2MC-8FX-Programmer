//! L1 命令集封装：16 条 vendor 命令的强类型客户端，含 BEGIN/DATA/COMMIT 分块流。
//!
//! 帧格式：请求 `[0x80][CMD][LEN_L][LEN_H][PAYLOAD≤56B]`，响应 `[0x80][STATUS][DATA]`。
//! 状态机：IDLE →（ENTER_PGM）→ SYNCED →（ERASE）→ ERASED →（FLASH_INIT）→ RW_MODE。
//! 协议全貌见 README.md「通信协议」章节。


use crate::error::{ProgError, Result};
use crate::transport::DapTransport;
use crate::vendor;

/// L1 命令 ID 表
pub mod cmd {
    pub const PING: u8 = 0x01;
    pub const SET_POWER: u8 = 0x02;
    pub const ENTER_PGM: u8 = 0x03;
    pub const ERASE: u8 = 0x04;
    pub const FLASH_INIT: u8 = 0x05;
    pub const WRITE_BEGIN: u8 = 0x06;
    pub const WRITE_DATA: u8 = 0x07;
    pub const WRITE_COMMIT: u8 = 0x08;
    pub const READ_BEGIN: u8 = 0x09;
    pub const READ_DATA: u8 = 0x0A;
    pub const CR_TRIM_WRITE: u8 = 0x0B;
    pub const QUIT: u8 = 0x0C;
    pub const RESET_RUN: u8 = 0x0D;
    pub const GET_STATE: u8 = 0x0E;
    pub const WRITE_SECURE: u8 = 0x0F;
    pub const SEND_BREAK: u8 = 0x10;
    pub const DISCONNECT: u8 = 0x11;
    pub const ABORT: u8 = 0x12;
    pub const SET_CHIP: u8 = 0x13;
}

/// 各命令的单条响应等待超时（上位机侧）
///
/// @note ERASE 上限 90 s：固件侧等待上限 60 s，上位机留余量；超时≠失败，需恢复流程。
pub mod timeout {
    use std::time::Duration;
    pub const PING: Duration = Duration::from_secs(1);
    /// SET_POWER：上电含 3 s 上升确认
    pub const SET_POWER: Duration = Duration::from_secs(5);
    /// ENTER_PGM 须覆盖固件最坏路径：整循环重试 ×3（握手失败时放电→上电→
    /// 保持→握手重进，参照 YM02 恢复行为）——单次最坏 ~13s（放电≤10s+上电
    /// ≤3s+稳定≤10s 等不会同时拉满），3 次 ≈ 40s 上限
    pub const ENTER_PGM: Duration = Duration::from_secs(40);
    pub const ERASE: Duration = Duration::from_secs(90);
    pub const FLASH_INIT: Duration = Duration::from_secs(5);
    pub const WRITE_BEGIN: Duration = Duration::from_secs(1);
    pub const WRITE_DATA: Duration = Duration::from_secs(1);
    pub const WRITE_COMMIT: Duration = Duration::from_secs(5);
    pub const READ_BEGIN: Duration = Duration::from_secs(2);
    pub const READ_DATA: Duration = Duration::from_secs(1);
    pub const CR_TRIM_WRITE: Duration = Duration::from_secs(2);
    pub const QUIT: Duration = Duration::from_secs(2);
    /// RESET_RUN：新固件（0.2.0 修订）做真实的断电→放电→上电循环（最坏 ~13s）；
    /// 旧固件恒回 UNSUPPORTED（秒回，由上位机自行 set_power 兜底）
    pub const RESET_RUN: Duration = Duration::from_secs(15);
    pub const GET_STATE: Duration = Duration::from_secs(1);
    pub const WRITE_SECURE: Duration = Duration::from_secs(2);
    pub const SEND_BREAK: Duration = Duration::from_secs(1);
    pub const DISCONNECT: Duration = Duration::from_secs(1);
    pub const ABORT: Duration = Duration::from_secs(1);
    pub const SET_CHIP: Duration = Duration::from_secs(1);
}

/// 单块写上限（WRITE_BEGIN/DATA/COMMIT 一个事务的总数据量）
pub const WRITE_BLOCK_MAX: usize = 512;
/// 单块读上限（READ_BEGIN/DATA 一个事务的总数据量）
pub const READ_BLOCK_MAX: usize = 1024;

/// 复位能力（RESET_RUN 响应 DATA[0]）：固件按自身硬件自动判定上报
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// 原生：复位引脚直接实现
    Native,
    /// 模拟：断电+上电实现（兼容模式）
    Simulated,
}

/// F2MC-8FX 编程器 L1 客户端（泛型于传输层，真机/模拟器/Mock 可互换）
pub struct F2mcClient<T: DapTransport> {
    t: T,
    /// 固件版本（PING 后填充）
    pub fw_version: Option<[u8; 3]>,
}

impl<T: DapTransport> F2mcClient<T> {
    /// 以指定传输层构造客户端
    pub fn new(t: T) -> Self {
        Self { t, fw_version: None }
    }

    /// 可变访问底层传输（模拟器/Mock 断言用）
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.t
    }

    /// PING(0x01)：连接握手。
    ///
    /// @returns 设备标识字符串（应为 "F2MC-LINK"），固件版本缓存到 `fw_version`
    pub fn ping(&mut self) -> Result<String> {
        // 响应固定 12 B："F2MC-LINK"(9B) + FW版本(3B)
        // 不能用尾零裁剪：版本号如 [1,0,0] 的尾零会被误裁导致版本解析失败
        let data = vendor::transact(&mut self.t, cmd::PING, &[], Some(12), timeout::PING)?;
        self.fw_version = Some([data[9], data[10], data[11]]);
        let id = String::from_utf8_lossy(&data[..9]).into_owned();
        Ok(id)
    }

    /// SET_POWER(0x02)：目标供电开关（payload = [on]）
    pub fn set_power(&mut self, on: bool) -> Result<()> {
        vendor::transact(&mut self.t, cmd::SET_POWER, &[on as u8], Some(0), timeout::SET_POWER)?;
        Ok(())
    }

    /// ENTER_PGM(0x03)：与目标握手进入编程模式（仅 IDLE 态可用）。
    ///
    /// @warning 目标无响应时固件失败路径最长 ~7 s 才返回，超时后不得立即发新命令，
    /// 须走 enter_pgm_with_recovery 等固件返回（见 flow.rs）。
    pub fn enter_pgm(&mut self) -> Result<()> {
        vendor::transact(&mut self.t, cmd::ENTER_PGM, &[], Some(0), timeout::ENTER_PGM)?;
        Ok(())
    }

    /// ERASE(0x04)：擦除（仅 SYNCED 态可用）。
    ///
    /// @param addr 0x0000 = 整片擦除（同时解除安全锁）；否则扇区擦除
    pub fn erase(&mut self, addr: u16) -> Result<()> {
        let payload = addr.to_be_bytes(); // [AddrH, AddrL]
        vendor::transact(&mut self.t, cmd::ERASE, &payload, Some(0), timeout::ERASE)?;
        Ok(())
    }

    /// FLASH_INIT(0x05)：初始化目标 Flash 接口并切高速时钟（SYNCED/ERASED 可用），
    /// 成功后进入 RW_MODE。xx/yy 为时钟配置参数（见 new8fx::FLASH_INIT_XX/YY）。
    /// DA 由固件内嵌表按 SET_CHIP 下发的型号匹配（docs/DA 结构解析.md）。
    pub fn flash_init(&mut self, xx: u8, yy: u8) -> Result<()> {
        vendor::transact(&mut self.t, cmd::FLASH_INIT, &[xx, yy], Some(0), timeout::FLASH_INIT)?;
        Ok(())
    }

    /// SET_CHIP(0x13)：下发型号名（如 "MB95F698K"），固件按系列匹配内嵌 DA。
    /// 每次操作前调用一次即可（固件在 DISCONNECT 前保持匹配结果）。
    pub fn set_chip(&mut self, chip_name: &str) -> Result<()> {
        vendor::transact(
            &mut self.t,
            cmd::SET_CHIP,
            chip_name.as_bytes(),
            Some(0),
            timeout::SET_CHIP,
        )?;
        Ok(())
    }

    /// 大块写（≤512B，仅 RW_MODE）：WRITE_BEGIN → WRITE_DATA×N（每包 ≤56B）→ WRITE_COMMIT
    ///
    /// @param addr 目标起始地址
    /// @param data 数据（1..=512 字节）
    pub fn write_block(&mut self, addr: u16, data: &[u8]) -> Result<()> {
        if data.is_empty() || data.len() > WRITE_BLOCK_MAX {
            return Err(ProgError::BadParam(format!(
                "write_block len {} out of range 1..={WRITE_BLOCK_MAX}",
                data.len()
            )));
        }
        let mut begin = Vec::with_capacity(4);
        begin.extend_from_slice(&addr.to_be_bytes());
        begin.extend_from_slice(&(data.len() as u16).to_be_bytes());
        vendor::transact(&mut self.t, cmd::WRITE_BEGIN, &begin, Some(0), timeout::WRITE_BEGIN)?;

        for chunk in data.chunks(vendor::MAX_PAYLOAD) {
            vendor::transact(&mut self.t, cmd::WRITE_DATA, chunk, Some(0), timeout::WRITE_DATA)?;
        }

        vendor::transact(&mut self.t, cmd::WRITE_COMMIT, &[], Some(0), timeout::WRITE_COMMIT)?;
        Ok(())
    }

    /// 大块读（≤1024B，仅 RW_MODE）：READ_BEGIN → READ_DATA×N（固件在 BEGIN 时已完成 L2 读）
    ///
    /// @param addr 目标起始地址
    /// @param len 读取长度（1..=1024）
    /// @returns 读回的数据（len 字节）
    pub fn read_block(&mut self, addr: u16, len: usize) -> Result<Vec<u8>> {
        if len == 0 || len > READ_BLOCK_MAX {
            return Err(ProgError::BadParam(format!(
                "read_block len {len} out of range 1..={READ_BLOCK_MAX}"
            )));
        }
        let mut begin = Vec::with_capacity(4);
        begin.extend_from_slice(&addr.to_be_bytes());
        begin.extend_from_slice(&(len as u16).to_be_bytes());
        vendor::transact(&mut self.t, cmd::READ_BEGIN, &begin, Some(0), timeout::READ_BEGIN)?;

        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let want = (len - out.len()).min(vendor::MAX_PAYLOAD);
            let chunk = vendor::transact(&mut self.t, cmd::READ_DATA, &[], Some(want), timeout::READ_DATA)?;
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    /// ⚠ 保留的协议命令：真机 DA 不实现 Spec 7.9 的 0x55 命令，固件恒回 ACK_ERROR。
    /// CR 校准回写请改用普通写路径 write_block(0xFFBB, ..)。
    pub fn cr_trim_write(&mut self, addr: u16, data: u8) -> Result<()> {
        let payload = [addr.to_be_bytes()[0], addr.to_be_bytes()[1], data];
        vendor::transact(&mut self.t, cmd::CR_TRIM_WRITE, &payload, Some(0), timeout::CR_TRIM_WRITE)?;
        Ok(())
    }

    /// QUIT(0x0C)：退出读写模式回 SYNCED（仅 RW_MODE 可用）。
    ///
    /// @warning QUIT 后握手失效（目标退回 bootloader 世界），下次擦除/读写前必须
    /// 复位状态机并重新 ENTER_PGM。
    pub fn quit(&mut self) -> Result<()> {
        vendor::transact(&mut self.t, cmd::QUIT, &[], Some(0), timeout::QUIT)?;
        Ok(())
    }

    /// RESET_RUN(0x0D)：复位目标运行用户程序。
    ///
    /// @note 新固件（0.2.0 修订）经电源开关做真实的断电→主动放电→上电循环，
    /// 响应 DATA[0] 上报复位能力（ResetMode）；旧固件恒回 UNSUPPORTED 并把
    /// 状态机复位到 IDLE——该错误码属预期，调用侧应容错（见 flow::reset_run_graceful）。
    pub fn reset_run(&mut self) -> Result<ResetMode> {
        let r = vendor::transact(&mut self.t, cmd::RESET_RUN, &[], Some(1), timeout::RESET_RUN)?;
        Ok(match r.first().copied().unwrap_or(0x01) {
            0x00 => ResetMode::Native,
            _ => ResetMode::Simulated,
        })
    }

    /// GET_STATE(0x0E)：查询状态机。
    ///
    /// @returns (state, last_err)：state 0=IDLE 1=SYNCED 2=ERASED 3=RW_MODE；
    /// last_err 为最近一次非零状态码（如 0x02 安全锁）。
    /// @warning GET_STATE 无法区分「刚握手」与「QUIT 后」的 SYNCED，重进流程一律
    /// 按握手失效处理（见 flow::session_begin_synced）。
    pub fn get_state(&mut self) -> Result<(u8, u8)> {
        let data = vendor::transact(&mut self.t, cmd::GET_STATE, &[], Some(2), timeout::GET_STATE)?;
        Ok((data[0], data[1]))
    }

    /// WRITE_SECURE(0x0F)：写 0xFFFC 安全位（仅 RW_MODE），QUIT/断电复位后生效。
    pub fn write_secure(&mut self) -> Result<()> {
        vendor::transact(&mut self.t, cmd::WRITE_SECURE, &[], Some(0), timeout::WRITE_SECURE)?;
        Ok(())
    }

    /// SEND_BREAK(0x10)：向目标发送 BREAK 信号（辅助握手）
    pub fn send_break(&mut self) -> Result<()> {
        vendor::transact(&mut self.t, cmd::SEND_BREAK, &[], Some(0), timeout::SEND_BREAK)?;
        Ok(())
    }

    /// DISCONNECT(0x11)：上位机断开通知——固件熄 LED、状态机复位 IDLE，
    /// 目标侧状态不变。关闭设备前应发送；发送失败不阻塞断开流程。
    pub fn disconnect(&mut self) -> Result<()> {
        vendor::transact(&mut self.t, cmd::DISCONNECT, &[], Some(0), timeout::DISCONNECT)?;
        Ok(())
    }

    /// ABORT(0x12)：强制中止固件当前长操作。
    /// 带外生效（固件 USB ISR 立即置中止标志），原命令将以 ABORTED(0x0B) 应答。
    /// 幂等：无操作在执行时发送无害。⚠ 中止 ENTER_PGM 后目标可能处于半初始化态。
    /// 注：常规取消由传输层在等待期间自动发 ABORT（DapTransport::set_cancel），
    /// 本方法供显式调用。
    pub fn abort(&mut self) -> Result<()> {
        vendor::transact(&mut self.t, cmd::ABORT, &[], Some(0), timeout::ABORT)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// ABORTED(0x0B) 状态码 → ProgError::Cancelled（ABORT 语义）
    #[test]
    fn aborted_maps_to_cancelled() {
        let mock = crate::transport::MockTransport::new()
            .on(&[0x80, cmd::ENTER_PGM], &[0x80, 0x0B]);
        let mut client = F2mcClient::new(mock);
        let err = client.enter_pgm().unwrap_err();
        assert!(matches!(err, ProgError::Cancelled));
    }

    use super::*;
    use crate::error::ProgError;
    use crate::sim::SimProgrammer;
    use crate::transport::MockTransport;

    /// WRITE_BEGIN/DATA/COMMIT 帧格式与 ≤56B 拆包
    #[test]
    fn write_block_framing() {
        let mut client = F2mcClient::new(SimProgrammer::new());
        client.ping().unwrap();
        client.enter_pgm().unwrap();
        client.erase(0).unwrap();
        client.flash_init(0x02, 0x7C).unwrap();

        let data = vec![0xABu8; 130]; // 130 = 56+56+18 → 3 个 DATA 包
        client.write_block(0x8123, &data).unwrap();

        let log = &client.transport_mut().frame_log;
        // WRITE_BEGIN: [0x80][0x06][04 00][AddrH AddrL LenH LenL]
        let begin = log.iter().find(|f| f[1] == cmd::WRITE_BEGIN).unwrap();
        assert_eq!(&begin[4..8], &[0x81, 0x23, 0x00, 130]);
        let datas: Vec<_> = log.iter().filter(|f| f[1] == cmd::WRITE_DATA).collect();
        assert_eq!(datas.len(), 3);
        assert_eq!(datas[0].len() - 4, 56);
        assert_eq!(datas[1].len() - 4, 56);
        assert_eq!(datas[2].len() - 4, 18);
        // 模拟器 flash 已写入
        assert_eq!(client.transport_mut().flash.get(&0x8123), Some(&0xAB));
        assert_eq!(client.transport_mut().flash.get(&(0x8123 + 129)), Some(&0xAB));
    }

    /// READ_BEGIN/DATA 分块取回
    #[test]
    fn read_block_framing() {
        let mut client = F2mcClient::new(SimProgrammer::new());
        client.ping().unwrap();
        client.enter_pgm().unwrap();
        client.erase(0).unwrap();
        client.flash_init(0x02, 0x7C).unwrap();
        client.write_block(0x9000, &[7u8; 100]).unwrap();

        let got = client.read_block(0x9000, 100).unwrap();
        assert_eq!(got, vec![7u8; 100]);
        let n_read_data = client
            .transport_mut()
            .frame_log
            .iter()
            .filter(|f| f[1] == cmd::READ_DATA)
            .count();
        assert_eq!(n_read_data, 2); // 56 + 44
    }

    /// 传输格式错误重试一次；状态码错误不重试
    #[test]
    fn retry_rules() {
        // 坏响应（首字节≠0x80）注入一次 → 第二次成功
        let mock = MockTransport::new()
            .on(&[0x80, cmd::PING], &[0x00, 0x00]) // 第一次：坏帧
            ;
        // MockTransport 按序匹配第一条规则，无法表达“只坏一次”，
        // 改用脚本化 SimProgrammer fail_next 验证 vendor 层重试：
        let mut sim = SimProgrammer::new();
        sim.fail_next = 1;
        let mut client = F2mcClient::new(sim);
        client.ping().unwrap(); // fail_next=1 被重试吃掉，第二次成功

        // 状态码错误不重试：锁定后 ENTER_PGM 直接返回 SecurityLocked
        let mut sim2 = SimProgrammer::new();
        sim2.locked = true;
        let mut client2 = F2mcClient::new(sim2);
        let err = client2.enter_pgm().unwrap_err();
        assert!(matches!(err, ProgError::SecurityLocked));
        let n_enter = client2
            .transport_mut()
            .frame_log
            .iter()
            .filter(|f| f[1] == cmd::ENTER_PGM)
            .count();
        assert_eq!(n_enter, 1, "status error must not retry");
        let _ = mock;
    }

    /// set_chip 把型号名下发给固件（固件按系列匹配内嵌 DA）
    #[test]
    fn set_chip_sends_model_name() {
        let mut client = F2mcClient::new(SimProgrammer::new());
        client.set_chip("MB95F698K").unwrap();
        assert_eq!(client.transport_mut().chip_name, "MB95F698K");
        let log = &client.transport_mut().frame_log;
        let f = log.iter().find(|f| f[1] == cmd::SET_CHIP).unwrap();
        assert_eq!(&f[4..], b"MB95F698K");
    }

    /// 状态机防呆：SYNCED 态发 WRITE_BEGIN → STATE_ERROR
    #[test]
    fn state_guard() {
        let mut client = F2mcClient::new(SimProgrammer::new());
        client.ping().unwrap();
        client.enter_pgm().unwrap();
        let err = client.write_block(0x8000, &[1, 2, 3]).unwrap_err();
        assert!(matches!(err, ProgError::DeviceStatus(_)));
    }
}
