//! 传输层：枚举 CMSIS-DAP 编程器（v2 Bulk 优先 → v1 HID 回退），DAP 包收发，
//! 取消令牌 → 带外 ABORT(0x12) 注入，超时后迟到响应吸收。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{ProgError, Result};

/// 取消令牌（与 flow::CancelToken 同型）
pub type CancelFlag = Arc<AtomicBool>;

/// ABORT 帧（0x12）：带外强制中止当前长操作
const ABORT_FRAME: [u8; 4] = [0x80, 0x12, 0x00, 0x00];
/// 读等待 tick：检查取消令牌的间隔
const CANCEL_TICK: Duration = Duration::from_millis(100);

/// DAP 传输抽象：发送一个请求包（首字节为命令 ID），返回响应包
pub trait DapTransport: Send {
    /// 发送一个 DAP 请求包并等待响应。
    ///
    /// @param request 完整请求包（首字节为命令 ID）
    /// @param timeout 响应等待超时
    /// @returns 响应包
    fn dap_transfer(&mut self, request: &[u8], timeout: Duration) -> Result<Vec<u8>>;
    /// 人类可读的通道描述（日志/UI 显示）
    fn describe(&self) -> String;

    /// 设置取消令牌：读等待期间每 100ms 检查一次，置位时立即发送 ABORT(0x12)
    /// 强制中止固件侧长操作，原命令将以 ABORTED(0x0B) 应答。
    fn set_cancel(&mut self, flag: CancelFlag) {
        let _ = flag;
    }

    /// 吸收超时后迟到的响应（超时后不得立即发新命令，先把迟到响应读掉）。
    /// `Ok(Some)` = 读到迟到包；`Ok(None)` = 无数据。默认实现用于 Mock/模拟器。
    fn drain(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let _ = timeout;
        Ok(None)
    }
}

impl<T: DapTransport + ?Sized> DapTransport for Box<T> {
    fn dap_transfer(&mut self, request: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        (**self).dap_transfer(request, timeout)
    }
    fn describe(&self) -> String {
        (**self).describe()
    }
    fn set_cancel(&mut self, flag: CancelFlag) {
        (**self).set_cancel(flag)
    }
    fn drain(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        (**self).drain(timeout)
    }
}

/// CMSIS-DAP 通道类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    /// CMSIS-DAP v2（Bulk / WinUSB）——优先
    V2Bulk,
    /// CMSIS-DAP v1（HID）——Win7 保底回退
    V1Hid,
}

impl ChannelKind {
    /// 通道类型显示名
    pub fn label(&self) -> &'static str {
        match self {
            ChannelKind::V2Bulk => "CMSIS-DAP v2 (Bulk)",
            ChannelKind::V1Hid => "CMSIS-DAP v1 (HID)",
        }
    }
}

/// 枚举到的编程器通道
#[derive(Debug, Clone)]
pub struct DapChannelInfo {
    /// 通道类型（v2 Bulk / v1 HID）
    pub kind: ChannelKind,
    /// USB VID / PID
    pub vid: u16,
    pub pid: u16,
    /// USB 序列号
    pub serial: Option<String>,
    /// USB 产品名
    pub product: String,
    /// v1: hidapi path；v2: 设备 id + 接口号
    pub path: String,
    /// USB 接口号（v2 claim 用）
    pub interface: u8,
}

impl std::fmt::Display for DapChannelInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} | {} | {:04X}:{:04X}",
            self.kind.label(),
            self.serial.as_deref().unwrap_or("(no serial)"),
            self.vid,
            self.pid
        )
    }
}

/// 枚举所有 CMSIS-DAP 通道，v2 在前（调用侧可自动预选 v2）。
/// 同一编程器同时暴露 v1/v2 两接口：检测到 v2 时抑制同设备（VID:PID+序列号）的 v1。
///
/// @returns 通道列表（可能为空），v2 Bulk 在前、v1 HID 在后
pub fn enumerate() -> Vec<DapChannelInfo> {
    let v2 = enumerate_v2();
    let mut v1 = enumerate_v1();
    v1.retain(|a| {
        !v2.iter().any(|b| {
            a.vid == b.vid
                && a.pid == b.pid
                && match (&a.serial, &b.serial) {
                    (Some(x), Some(y)) => x == y,
                    _ => true, // 无序列号时按 VID:PID 视为同一设备
                }
        })
    });
    let mut out = v2;
    out.extend(v1);
    out
}

// ---------------------------------------------------------------- v2 (Bulk)

fn enumerate_v2() -> Vec<DapChannelInfo> {
    let mut out = Vec::new();
    let devices = match nusb::list_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("nusb list_devices failed: {e}");
            return out;
        }
    };
    for dev in devices {
        for iface in dev.interfaces() {
            let is_dap = iface.interface_string().map(|s| s.contains("CMSIS-DAP")).unwrap_or(false)
                || dev
                    .product_string()
                    .map(|s| s.contains("CMSIS-DAP"))
                    .unwrap_or(false);
            // v2 Bulk 接口通常是 vendor class(0xFF)
            if is_dap && iface.class() == 0xFF {
                out.push(DapChannelInfo {
                    kind: ChannelKind::V2Bulk,
                    vid: dev.vendor_id(),
                    pid: dev.product_id(),
                    serial: dev.serial_number().map(|s| s.to_string()),
                    product: dev.product_string().unwrap_or("(unknown)").to_string(),
                    // nusb DeviceId 无 Display，用 bus:address 定位
                    path: format!("{}:{}", dev.bus_number(), dev.device_address()),
                    interface: iface.interface_number(),
                });
                break; // 每设备只取第一个 DAP bulk 接口
            }
        }
    }
    out
}

// ---------------------------------------------------------------- v1 (HID)

fn enumerate_v1() -> Vec<DapChannelInfo> {
    let mut out = Vec::new();
    let api = match hidapi::HidApi::new() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("hidapi init failed: {e}");
            return out;
        }
    };
    for dev in api.device_list() {
        let name_match = dev
            .product_string()
            .map(|s| s.contains("CMSIS-DAP"))
            .unwrap_or(false)
            || dev
                .manufacturer_string()
                .map(|s| s.contains("CMSIS-DAP"))
                .unwrap_or(false);
        if name_match {
            out.push(DapChannelInfo {
                kind: ChannelKind::V1Hid,
                vid: dev.vendor_id(),
                pid: dev.product_id(),
                serial: dev.serial_number().map(|s| s.to_string()),
                product: dev.product_string().unwrap_or("(unknown)").to_string(),
                path: dev.path().to_string_lossy().into_owned(),
                interface: dev.interface_number() as u8,
            });
        }
    }
    out
}

// ---------------------------------------------------------------- open

/// 按通道信息打开传输层
///
/// @param info 枚举得到的通道
/// @returns 已打开的传输层（trait object）
pub fn open(info: &DapChannelInfo) -> Result<Box<dyn DapTransport>> {
    match info.kind {
        ChannelKind::V1Hid => Ok(Box::new(HidTransport::open(&info.path)?)),
        ChannelKind::V2Bulk => BulkTransport::open(info).map(|t| Box::new(t) as Box<dyn DapTransport>),
    }
}

// ---------------------------------------------------------------- v1 实现

/// CMSIS-DAP v1 HID：主机侧 65B（ReportID=0x00 + 64B 包，零填充），设备侧 64B 包
pub struct HidTransport {
    dev: hidapi::HidDevice,
    desc: String,
    cancel: Option<CancelFlag>,
    /// 本次传输是否已发过 ABORT（幂等：一次传输内只发一次）
    aborted: bool,
}

impl HidTransport {
    /// 按 hidapi path 打开 v1 HID 通道
    pub fn open(path: &str) -> Result<Self> {
        let api = hidapi::HidApi::new()
            .map_err(|e| ProgError::Transport(format!("hidapi init: {e}")))?;
        let cpath = std::ffi::CString::new(path)
            .map_err(|e| ProgError::Transport(format!("bad path: {e}")))?;
        let dev = api
            .open_path(&cpath)
            .map_err(|e| ProgError::Transport(format!("open hid: {e}")))?;
        Ok(Self { dev, desc: format!("HID {path}"), cancel: None, aborted: false })
    }

    fn write_frame(&mut self, request: &[u8]) -> Result<()> {
        let mut buf = [0u8; 65];
        buf[1..1 + request.len()].copy_from_slice(request);
        tracing::trace!(target: "dap", "TX {}", hex(request));
        self.dev
            .write(&buf)
            .map_err(|e| ProgError::Transport(format!("hid write: {e}")))?;
        Ok(())
    }
}

impl DapTransport for HidTransport {
    fn dap_transfer(&mut self, request: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        if request.len() > 64 {
            return Err(ProgError::BadParam(format!(
                "DAP v1 packet {} > 64 bytes",
                request.len()
            )));
        }
        self.aborted = false;
        self.write_frame(request)?;

        // 读到非空包或超时；每 100ms tick 检查取消令牌 → 发 ABORT
        let deadline = Instant::now() + timeout;
        let mut resp = [0u8; 64];
        loop {
            // 取消：立即发 ABORT 强制中止固件长操作（原命令将以 ABORTED 应答）
            if !self.aborted && self.cancel.as_ref().is_some_and(|f| f.load(Ordering::Relaxed)) {
                self.aborted = true;
                tracing::warn!(target: "dap", "cancel: send ABORT(0x12)");
                self.write_frame(&ABORT_FRAME)?;
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                return Err(ProgError::Timeout);
            }
            let tick = remain.min(CANCEL_TICK);
            let n = self
                .dev
                .read_timeout(&mut resp, tick.as_millis() as i32)
                .map_err(|e| ProgError::Transport(format!("hid read: {e}")))?;
            if n == 0 {
                continue; // 该 tick 无数据，继续等到 deadline
            }
            tracing::trace!(target: "dap", "RX {}", hex(&resp[..n]));
            return Ok(resp[..n].to_vec());
        }
    }

    fn describe(&self) -> String {
        self.desc.clone()
    }

    fn set_cancel(&mut self, flag: CancelFlag) {
        self.cancel = Some(flag);
    }

    fn drain(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let mut resp = [0u8; 64];
        let n = self
            .dev
            .read_timeout(&mut resp, timeout.as_millis() as i32)
            .map_err(|e| ProgError::Transport(format!("hid drain: {e}")))?;
        if n == 0 {
            Ok(None)
        } else {
            Ok(Some(resp[..n].to_vec()))
        }
    }
}

// ---------------------------------------------------------------- v2 实现

/// CMSIS-DAP v2 Bulk 传输（nusb）。FS 包 ≤64B。
use nusb::transfer::{EndpointType, Queue, RequestBuffer};

/// CMSIS-DAP v2 Bulk 通道：异步端点队列 + 同步超时包装
pub struct BulkTransport {
    out_q: Queue<Vec<u8>>,
    in_q: Queue<RequestBuffer>,
    desc: String,
    cancel: Option<CancelFlag>,
    aborted: bool,
}

impl BulkTransport {
    /// 打开 v2 Bulk 通道：定位设备 → claim 接口 → 找 bulk IN/OUT 端点
    ///
    /// @param info 枚举得到的 v2 通道（path 为 bus:address）
    pub fn open(info: &DapChannelInfo) -> Result<Self> {
        let dev_info = nusb::list_devices()
            .map_err(|e| ProgError::Transport(format!("nusb list: {e}")))?
            .find(|d| format!("{}:{}", d.bus_number(), d.device_address()) == info.path)
            .ok_or_else(|| ProgError::Transport("device gone".into()))?;
        let dev = dev_info
            .open()
            .map_err(|e| ProgError::Transport(format!("nusb open: {e}")))?;
        let iface = dev
            .claim_interface(info.interface)
            .map_err(|e| ProgError::Transport(format!("claim interface: {e}")))?;

        // 找 bulk 端点（descriptors() 直接产出 InterfaceAltSetting）
        let (mut out_addr, mut in_addr) = (None, None);
        for alt in iface.descriptors() {
            for e in alt.endpoints() {
                if e.transfer_type() == EndpointType::Bulk {
                    if e.address() & 0x80 == 0 {
                        out_addr = Some(e.address());
                    } else {
                        in_addr = Some(e.address());
                    }
                }
            }
        }
        let out_addr = out_addr.ok_or_else(|| ProgError::Transport("no bulk OUT ep".into()))?;
        let in_addr = in_addr.ok_or_else(|| ProgError::Transport("no bulk IN ep".into()))?;

        Ok(Self {
            out_q: iface.bulk_out_queue(out_addr),
            in_q: iface.bulk_in_queue(in_addr),
            desc: format!("Bulk {} if{}", info.path, info.interface),
            cancel: None,
            aborted: false,
        })
    }
}

impl DapTransport for BulkTransport {
    fn dap_transfer(&mut self, request: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        self.aborted = false;
        let cancel = self.cancel.clone();
        futures_lite::future::block_on(async {
            tracing::trace!(target: "dap", "TX {}", hex(request));
            self.out_q.submit(request.to_vec());
            with_timeout(timeout, self.out_q.next_complete())
                .await?
                .into_result()
                .map_err(|e| ProgError::Transport(format!("bulk out: {e}")))?;

            self.in_q.submit(RequestBuffer::new(64));
            let deadline = Instant::now() + timeout;
            loop {
                // 取消：带外发 ABORT(0x12)，原命令将以 ABORTED 应答
                if !self.aborted && cancel.as_ref().is_some_and(|f| f.load(Ordering::Relaxed)) {
                    self.aborted = true;
                    tracing::warn!(target: "dap", "cancel: send ABORT(0x12)");
                    self.out_q.submit(ABORT_FRAME.to_vec());
                    if let Ok(c) =
                        with_timeout(Duration::from_secs(1), self.out_q.next_complete()).await
                    {
                        let _ = c.into_result();
                    }
                }
                let remain = deadline.saturating_duration_since(Instant::now());
                if remain.is_zero() {
                    return Err(ProgError::Timeout);
                }
                // 100ms tick 轮询（next_complete 取消安全，丢弃不影响挂起的传输）
                match with_timeout(remain.min(CANCEL_TICK), self.in_q.next_complete()).await {
                    Ok(c) => {
                        let buf = c
                            .into_result()
                            .map_err(|e| ProgError::Transport(format!("bulk in: {e}")))?;
                        tracing::trace!(target: "dap", "RX {}", hex(&buf));
                        return Ok(buf);
                    }
                    Err(ProgError::Timeout) => continue,
                    Err(e) => return Err(e),
                }
            }
        })
    }

    fn describe(&self) -> String {
        self.desc.clone()
    }

    fn set_cancel(&mut self, flag: CancelFlag) {
        self.cancel = Some(flag);
    }

    fn drain(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let r = futures_lite::future::block_on(with_timeout(timeout, async {
            self.in_q.submit(RequestBuffer::new(64));
            let c = self.in_q.next_complete().await;
            c.into_result()
                .map_err(|e| ProgError::Transport(format!("bulk drain: {e}")))
        }));
        match r {
            Ok(Ok(buf)) => Ok(Some(buf)),
            Ok(Err(e)) => Err(e),
            Err(ProgError::Timeout) => {
                self.in_q.cancel_all(); // 取消未完成的读请求
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

/// 简单超时包装（不引入 tokio）
async fn with_timeout<F: std::future::Future>(dur: Duration, fut: F) -> Result<F::Output> {
    futures_lite::future::or(
        async { Ok(fut.await) },
        async {
            async_io::Timer::after(dur).await;
            Err(ProgError::Timeout)
        },
    )
    .await
}

// ---------------------------------------------------------------- Mock（测试用）

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

/// 脚本化 mock 传输层：按请求返回预置响应
#[derive(Default)]
pub struct MockTransport {
    /// (匹配请求前缀, 响应)；按序匹配第一条命中的规则
    pub rules: Vec<(Vec<u8>, Vec<u8>)>,
    /// 已收到的请求（断言用）
    pub log: Vec<Vec<u8>>,
    /// 通用默认响应（规则未命中时）：[0x80, 0x00] OK
    pub default_ok: bool,
}

impl MockTransport {
    pub fn new() -> Self {
        Self { rules: Vec::new(), log: Vec::new(), default_ok: true }
    }

    /// 规则：对以 `prefix` 开头的请求返回 `resp`
    pub fn on(mut self, prefix: &[u8], resp: &[u8]) -> Self {
        self.rules.push((prefix.to_vec(), resp.to_vec()));
        self
    }
}

impl DapTransport for MockTransport {
    fn dap_transfer(&mut self, request: &[u8], _timeout: Duration) -> Result<Vec<u8>> {
        self.log.push(request.to_vec());
        for (prefix, resp) in &self.rules {
            if request.starts_with(prefix) {
                return Ok(resp.clone());
            }
        }
        if self.default_ok {
            Ok(vec![0x80, 0x00])
        } else {
            Err(ProgError::Transport(format!("no rule for {request:02X?}")))
        }
    }

    fn describe(&self) -> String {
        "mock".into()
    }
}
