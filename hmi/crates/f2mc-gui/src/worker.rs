//! 后台工作线程：UI 命令 → 执行烧录流程 → UI 事件
//!
//! ```text
//! [egui 主线程] --UiCommand--> [worker 线程] --UiEvent--> [egui 主线程]
//!    渲染/交互                  执行烧录流程              进度/日志/结果
//! ```
//! ⚠ 烧录阻塞 I/O 绝不许在 egui 主线程执行。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use f2mc_core::chipdef::chips;
use f2mc_core::flow::{self, FlowEvent, FlowOptions};
use f2mc_core::hexfile;
use f2mc_core::proto::F2mcClient;
use f2mc_core::transport::{self, DapChannelInfo, DapTransport};
use f2mc_core::{ProgError, Result as CoreResult};
use stm32_flash::{self as stm32, ImageFormat, ProbeInfo, Stm32Error, Stm32Session, TargetChoice};

/// 取消令牌（与 flow::CancelToken 同型）
pub type CancelToken = Arc<AtomicBool>;

type Client = F2mcClient<Box<dyn DapTransport>>;

/// 一次烧录/校验任务的参数
#[derive(Debug, Clone)]
pub struct JobCfg {
    /// 型号在 chips() 中的索引
    pub chip_idx: usize,
    /// 烧录文件路径
    pub hex_path: PathBuf,
    /// 烧录后写 0xFFFC 安全位
    pub write_secure: bool,
    /// 烧录完成后复位运行
    pub reset_after: bool,
}

/// UI → worker 命令
#[derive(Debug)]
pub enum UiCommand {
    /// 重新枚举 CMSIS-DAP 通道
    RefreshChannels,
    /// 连接编程器（通道来自枚举列表）
    Connect { channel: DapChannelInfo },
    /// 断开连接（先发 DISCONNECT 通知固件）
    Disconnect,
    /// 烧录（擦除+写入+校验）
    Program(JobCfg),
    /// 仅整片擦除
    EraseOnly,
    /// 仅校验
    VerifyOnly(JobCfg),
    /// 全地址空间读回并保存到文件
    ReadOut { out: PathBuf },
    /// 复位目标运行用户程序
    ResetRun,
    /// 目标电源开关（F2MC-LINK v1.1）：true=上电 / false=断电（断电使固件状态机复位 IDLE）
    SetPower(bool),
    // ---------------- STM32 路径（probe-rs） ----------------
    /// 枚举调试探针
    Stm32RefreshProbes,
    /// 连接 STM32 目标；serial=None 取第一个探针
    Stm32Connect {
        serial: Option<String>,
        /// 自动识别型号（false 时用 target_name）
        auto: bool,
        target_name: String,
        /// SWD 速率 kHz
        speed_khz: u32,
    },
    /// 烧录 STM32（擦除+写入+校验）
    Stm32Program {
        path: PathBuf,
        fmt: ImageFormat,
        /// 烧录后自动读回校验
        verify_after: bool,
        /// 烧录后软件复位运行
        reset_after: bool,
    },
    /// STM32 整片擦除
    Stm32Erase,
    /// STM32 仅校验
    Stm32Verify { path: PathBuf, fmt: ImageFormat },
    /// STM32 复位运行（SYSRESETREQ 软复位）
    Stm32ResetRun,
}

/// worker → UI 事件
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// 通道枚举结果
    Channels(Vec<DapChannelInfo>),
    /// 已连接（通道描述 + 设备标识 + 固件版本）
    Connected(String),
    /// 已断开
    Disconnected,
    /// 普通日志
    Log(String),
    /// 告警
    Warn(String),
    /// 阶段切换（阶段显示名）
    Stage(&'static str),
    /// 进度
    Progress { done: usize, total: usize },
    /// 任务成功完成
    Done(String),
    /// 任务失败
    Failed(String),
    /// STM32 探针枚举结果
    Stm32Probes(Vec<ProbeInfo>),
}

fn send(tx: &Sender<UiEvent>, ctx: &egui::Context, ev: UiEvent) {
    let _ = tx.send(ev);
    ctx.request_repaint();
}

fn forward(tx: &Sender<UiEvent>, ctx: &egui::Context, e: FlowEvent) {
    match e {
        FlowEvent::StageStart(s) => send(tx, ctx, UiEvent::Stage(s.label())),
        FlowEvent::Progress(_, done, total) => send(tx, ctx, UiEvent::Progress { done, total }),
        FlowEvent::Log(m) => send(tx, ctx, UiEvent::Log(m)),
        FlowEvent::Warn(m) => send(tx, ctx, UiEvent::Warn(m)),
    }
}

fn finish(tx: &Sender<UiEvent>, ctx: &egui::Context, r: CoreResult<()>, ok_msg: &str) {
    match r {
        Ok(()) => send(tx, ctx, UiEvent::Done(ok_msg.into())),
        Err(ProgError::Cancelled) => send(
            tx,
            ctx,
            UiEvent::Failed("已取消（若中止于进入编程模式，重试前建议断电重启目标板）".into()),
        ),
        Err(ProgError::SecurityLocked) => {
            send(tx, ctx, UiEvent::Failed("目标已加安全锁（需整片擦除解锁）".into()))
        }
        Err(ProgError::DeviceStatus(f2mc_core::error::StatusCode::Unsupported)) => send(
            tx,
            ctx,
            UiEvent::Failed("编程器无复位硬件，请人工断电重启目标板".into()),
        ),
        Err(ProgError::DeviceStatus(f2mc_core::error::StatusCode::PowerFault)) => send(
            tx,
            ctx,
            UiEvent::Failed("目标电源故障：过载/短路/未接目标（已自动关断）".into()),
        ),
        Err(e) => send(tx, ctx, UiEvent::Failed(format!("{e}"))),
    }
}

/// 后台工作线程句柄：接收 UI 命令、执行烧录流程、回发 UI 事件
pub struct Worker {
    rx: Receiver<UiCommand>,
    tx: Sender<UiEvent>,
    ctx: egui::Context,
    cancel: CancelToken,
    client: Option<Client>,
    /// STM32 会话（probe-rs）
    stm32: Option<Stm32Session>,
}

impl Worker {
    /// 启动 worker 线程（名为 "f2mc-worker"）
    ///
    /// @param rx UI 命令接收端
    /// @param tx UI 事件发送端
    /// @param ctx egui 上下文（事件后 request_repaint 唤醒 UI）
    /// @param cancel 取消令牌（连接时接入传输层自动 ABORT）
    pub fn spawn(
        rx: Receiver<UiCommand>,
        tx: Sender<UiEvent>,
        ctx: egui::Context,
        cancel: CancelToken,
    ) -> std::thread::JoinHandle<()> {
        let w = Worker { rx, tx, ctx, cancel, client: None, stm32: None };
        std::thread::Builder::new()
            .name("f2mc-worker".into())
            .spawn(move || w.run())
            .expect("spawn worker")
    }

    fn run(mut self) {
        while let Ok(cmd) = self.rx.recv() {
            self.handle(cmd);
        }
    }

    fn client(&mut self) -> CoreResult<&mut Client> {
        self.cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        self.client.as_mut().ok_or_else(|| ProgError::Transport("not connected".into()))
    }

    fn stm32_session(&mut self) -> Result<&mut Stm32Session, Stm32Error> {
        self.cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        self.stm32.as_mut().ok_or(Stm32Error::NoProbe)
    }

    /// OpEvent → UiEvent 投影（probe-rs 进度粒度映射到阶段/进度条）
    fn stm32_forward(tx: &Sender<UiEvent>, ctx: &egui::Context) -> impl FnMut(stm32::OpEvent) {
        let tx = tx.clone();
        let ctx = ctx.clone();
        let mut cur_total: u64 = 0;
        move |e| match e {
            stm32::OpEvent::StageStart { op, total } => {
                cur_total = total.unwrap_or(0);
                let label: &'static str = match op.as_str() {
                    "erase" => "擦除",
                    "program" => "写入",
                    "verify" => "校验",
                    "fill" => "填充",
                    _ => "执行",
                };
                send(&tx, &ctx, UiEvent::Stage(label));
                send(&tx, &ctx, UiEvent::Progress { done: 0, total: cur_total as usize });
            }
            stm32::OpEvent::Progress { done } => send(
                &tx,
                &ctx,
                UiEvent::Progress { done: done as usize, total: cur_total as usize },
            ),
            stm32::OpEvent::StageEnd => {}
            stm32::OpEvent::Message(m) => send(&tx, &ctx, UiEvent::Log(m)),
        }
    }

    fn finish_stm32(tx: &Sender<UiEvent>, ctx: &egui::Context, r: Result<(), Stm32Error>, ok: &str) {
        match r {
            Ok(()) => send(tx, ctx, UiEvent::Done(ok.into())),
            Err(Stm32Error::Cancelled) => send(tx, ctx, UiEvent::Failed("已取消".into())),
            Err(e) => send(tx, ctx, UiEvent::Failed(format!("{e}"))),
        }
    }

    fn handle(&mut self, cmd: UiCommand) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let cancel = self.cancel.clone();
        match cmd {
            UiCommand::RefreshChannels => {
                let list = transport::enumerate();
                send(&tx, &ctx, UiEvent::Channels(list));
            }
            UiCommand::Connect { channel } => {
                self.client = None;
                let result: CoreResult<(Box<dyn DapTransport>, String)> =
                    transport::open(&channel).map(|t| (t, channel.to_string()));
                match result {
                    Ok((mut t, desc)) => {
                        // 取消令牌接入传输层：读等待期间自动发 ABORT(0x12)
                        t.set_cancel(cancel.clone());
                        let mut client = F2mcClient::new(t);
                        match client.ping() {
                            Ok(id) => {
                                let fw = client
                                    .fw_version
                                    .map(|v| format!("v{}.{}.{}", v[0], v[1], v[2]))
                                    .unwrap_or_else(|| "v?".into());
                                send(&tx, &ctx, UiEvent::Connected(format!("{desc} | {id} {fw}")));
                                send(&tx, &ctx, UiEvent::Log(format!("已连接：{desc}（{id} {fw}）")));
                                self.client = Some(client);
                            }
                            Err(e) => send(&tx, &ctx, UiEvent::Failed(format!("PING 失败：{e}"))),
                        }
                    }
                    Err(e) => send(&tx, &ctx, UiEvent::Failed(format!("打开通道失败：{e}"))),
                }
            }
            UiCommand::Disconnect => {
                // 关闭设备前发 DISCONNECT（0x11）通知固件熄 LED、复位状态机
                if let Some(c) = self.client.as_mut() {
                    match c.disconnect() {
                        Ok(()) => send(&tx, &ctx, UiEvent::Log("已发送 DISCONNECT".into())),
                        Err(e) => send(
                            &tx,
                            &ctx,
                            UiEvent::Warn(format!("DISCONNECT 发送失败（忽略）：{e}")),
                        ),
                    }
                }
                self.client = None;
                // 断开时同时释放 STM32 会话（两个家族不同时连接）
                self.stm32 = None;
                send(&tx, &ctx, UiEvent::Disconnected);
                send(&tx, &ctx, UiEvent::Log("已断开连接".into()));
            }
            UiCommand::Program(job) => {
                let r = self.run_program(&job);
                finish(&tx, &ctx, r.map(|_| ()), "烧录完成");
            }
            UiCommand::EraseOnly => {
                let r = match self.client() {
                    Ok(c) => flow::erase_only(c, &mut |e| forward(&tx, &ctx, e), &cancel),
                    Err(e) => Err(e),
                };
                finish(&tx, &ctx, r, "擦除完成");
            }
            UiCommand::VerifyOnly(job) => {
                let r = self.run_verify(&job);
                finish(&tx, &ctx, r, "校验通过");
            }
            UiCommand::ReadOut { out } => {
                // 全地址空间读取（0x1000~0xFFFF，含下 bank）
                let start = 0x1000u16;
                let len = (0x10000 - 0x1000) as usize;
                let r = match self.client() {
                    Ok(c) => flow::read_out(c, start, len, &mut |e| forward(&tx, &ctx, e), &cancel),
                    Err(e) => Err(e),
                };
                match r {
                    Ok(data) => match write_readout_file(&out, start as u32, &data) {
                        Ok(()) => send(&tx, &ctx, UiEvent::Done(format!(
                            "读取完成：{} 字节 → {}",
                            data.len(),
                            out.display()
                        ))),
                        Err(e) => send(&tx, &ctx, UiEvent::Failed(format!("保存失败：{e}"))),
                    },
                    Err(e) => send(&tx, &ctx, UiEvent::Failed(format!("读取失败：{e}"))),
                }
            }
            UiCommand::ResetRun => {
                let r = match self.client() {
                    Ok(c) => c.reset_run(),
                    Err(e) => Err(e),
                };
                finish(&tx, &ctx, r, "已复位运行");
            }
            UiCommand::SetPower(on) => {
                let r = match self.client() {
                    Ok(c) => c.set_power(on),
                    Err(e) => Err(e),
                };
                finish(&tx, &ctx, r, if on { "目标已上电" } else { "目标已断电" });
            }
            // ---------------- STM32 路径 ----------------
            UiCommand::Stm32RefreshProbes => {
                let probes = stm32::list_probes();
                send(&tx, &ctx, UiEvent::Log(format!("枚举到 {} 个调试探针", probes.len())));
                send(&tx, &ctx, UiEvent::Stm32Probes(probes));
            }
            UiCommand::Stm32Connect { serial, auto, target_name, speed_khz } => {
                self.stm32 = None;
                self.client = None; // 两个家族不同时连接
                let choice = if auto {
                    TargetChoice::Auto
                } else {
                    TargetChoice::Named(target_name.clone())
                };
                match Stm32Session::connect(serial.as_deref(), &choice, speed_khz) {
                    Ok(s) => {
                        let name = s.target_name().to_string();
                        self.stm32 = Some(s);
                        let mode = if auto { "自动识别" } else { "手动指定" };
                        send(&tx, &ctx, UiEvent::Connected(format!("STM32 | {name}（{mode}）")));
                        send(&tx, &ctx, UiEvent::Log(format!("已连接 STM32 目标：{name}（{mode}，SWD {}MHz）", speed_khz / 1000)));
                    }
                    Err(e) => send(&tx, &ctx, UiEvent::Failed(format!("{e}"))),
                }
            }
            UiCommand::Stm32Program { path, fmt, verify_after, reset_after } => {
                let cancel = self.cancel.clone();
                let r = match self.stm32_session() {
                    Ok(s) => {
                        let mut cb = Self::stm32_forward(&tx, &ctx);
                        s.program(&path, &fmt, verify_after, &mut cb, &cancel).and_then(|_| {
                            if reset_after {
                                s.reset_run()
                            } else {
                                Ok(())
                            }
                        })
                    }
                    Err(e) => Err(e),
                };
                let ok = if reset_after { "烧录完成（已复位运行）" } else { "烧录完成" };
                Self::finish_stm32(&tx, &ctx, r, ok);
            }
            UiCommand::Stm32Erase => {
                let cancel = self.cancel.clone();
                let r = match self.stm32_session() {
                    Ok(s) => {
                        let mut cb = Self::stm32_forward(&tx, &ctx);
                        s.erase_all(&mut cb, &cancel)
                    }
                    Err(e) => Err(e),
                };
                Self::finish_stm32(&tx, &ctx, r, "整片擦除完成");
            }
            UiCommand::Stm32Verify { path, fmt } => {
                let cancel = self.cancel.clone();
                let r = match self.stm32_session() {
                    Ok(s) => {
                        let mut cb = Self::stm32_forward(&tx, &ctx);
                        s.verify(&path, &fmt, &mut cb, &cancel)
                    }
                    Err(e) => Err(e),
                };
                Self::finish_stm32(&tx, &ctx, r, "校验通过");
            }
            UiCommand::Stm32ResetRun => {
                let r = match self.stm32_session() {
                    Ok(s) => s.reset_run(),
                    Err(e) => Err(e),
                };
                Self::finish_stm32(&tx, &ctx, r, "已复位运行");
            }
        }
    }

    fn run_program(&mut self, job: &JobCfg) -> CoreResult<()> {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let cancel = self.cancel.clone();
        let client = self.client()?;

        let chip = &chips()[job.chip_idx.min(chips().len() - 1)];
        let text = std::fs::read_to_string(&job.hex_path)?;
        let img = hexfile::parse(&text, chip)?;
        for w in &img.warnings {
            send(&tx, &ctx, UiEvent::Warn(w.clone()));
        }
        let opts = FlowOptions { write_secure: job.write_secure, reset_after: job.reset_after };
        let report = flow::program(client, &img, &opts, &mut |e| forward(&tx, &ctx, e), &cancel)?;
        if report.unlocked {
            send(&tx, &ctx, UiEvent::Warn("目标曾加安全锁，已自动解锁".into()));
        }
        if report.cr_trim_fixed > 0 {
            send(&tx, &ctx, UiEvent::Warn(format!("CR 校准回写 {} 字节", report.cr_trim_fixed)));
        }
        send(&tx, &ctx, UiEvent::Log(format!(
            "写入 {} B，校验 {} B，耗时 {:.1} s",
            report.bytes_written, report.bytes_verified, report.elapsed_secs
        )));
        Ok(())
    }

    fn run_verify(&mut self, job: &JobCfg) -> CoreResult<()> {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let cancel = self.cancel.clone();
        let client = self.client()?;

        let chip = &chips()[job.chip_idx.min(chips().len() - 1)];
        let text = std::fs::read_to_string(&job.hex_path)?;
        let img = hexfile::parse(&text, chip)?;
        flow::verify_only(client, &img, &mut |e| forward(&tx, &ctx, e), &cancel)
    }
}

/// 将读出的数据按扩展名保存：.hex=Intel HEX / .mhx=S-record(S1) / .bin=原始二进制
fn write_readout_file(path: &std::path::Path, start: u32, data: &[u8]) -> std::io::Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mhx" | "s19" | "mot" => write_srec_file(path, start, data),
        "bin" => std::fs::write(path, data),
        _ => write_hex_file(path, start, data), // 默认 Intel HEX
    }
}

/// 将读出的数据写成 Intel HEX 文件（16 B/记录）
fn write_hex_file(path: &std::path::Path, start: u32, data: &[u8]) -> std::io::Result<()> {
    let mut s = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        let addr = (start + (i * 16) as u32) as u16;
        let mut rec: Vec<u8> = vec![chunk.len() as u8, (addr >> 8) as u8, addr as u8, 0];
        rec.extend_from_slice(chunk);
        let sum: u8 = rec.iter().fold(0, |a, b| a.wrapping_add(*b));
        let chk = (!sum).wrapping_add(1);
        s.push(':');
        for b in &rec {
            s.push_str(&format!("{b:02X}"));
        }
        s.push_str(&format!("{chk:02X}\n"));
    }
    s.push_str(":00000001FF\n");
    std::fs::write(path, s)
}

/// 将读出的数据写成 Motorola S-record 文件（S1，16 B/记录）
fn write_srec_file(path: &std::path::Path, start: u32, data: &[u8]) -> std::io::Result<()> {
    let mut s = String::from("S0030000FC\n");
    for (i, chunk) in data.chunks(16).enumerate() {
        let addr = (start + (i * 16) as u32) as u16;
        let count = (chunk.len() + 3) as u8;
        let mut rec: Vec<u8> = vec![count, (addr >> 8) as u8, addr as u8];
        rec.extend_from_slice(chunk);
        let sum: u8 = rec.iter().fold(0, |a, b| a.wrapping_add(*b));
        let chk = !sum;
        s.push_str("S1");
        for b in &rec {
            s.push_str(&format!("{b:02X}"));
        }
        s.push_str(&format!("{chk:02X}\n"));
    }
    s.push_str("S9030000FC\n");
    std::fs::write(path, s)
}
