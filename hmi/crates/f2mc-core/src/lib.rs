//! F2MC-8FX 编程核心库（UI 无关）。
//!
//! 覆盖 CMSIS-DAP vendor 命令 L1 层（帧收发/重试/取消）、hex/mhx 烧录文件解析清洗、
//! 烧录流程状态机（擦除/写入/校验/安全位）、编程器模拟器（集成测试与 GUI mock 共用）。
//! 协议细节（L1 帧格式、16 条命令、状态机 IDLE→SYNCED→ERASED→RW_MODE、状态码表）
//! 见 docs/上位机使用说明.md「通信协议」章节与 docs/通信协议约定.md。

pub mod chipdef;
pub mod error;
pub mod flow;
pub mod hexfile;
pub mod new8fx;
pub mod proto;
pub mod sim;
pub mod transport;
pub mod vendor;

pub use chipdef::{chips, search, ChipDef};
pub use error::{ProgError, Result};
pub use flow::{default_cancel, CancelToken, FlowEvent, FlowOptions, FlowReport, Stage};
pub use hexfile::HexImage;
pub use proto::F2mcClient;
pub use sim::SimProgrammer;
pub use transport::{DapChannelInfo, DapTransport, MockTransport};
