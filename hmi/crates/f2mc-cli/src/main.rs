//! f2mc-programmer-cli —— F2MC-8FX 烧录命令行（复用 f2mc-core 全流程）
//!
//! 两种用法：
//!
//! 1) 原生子命令（脚本/CI/VS Code 插件直接调用）：
//!    f2mc-programmer-cli probes
//!    f2mc-programmer-cli program <image.hex|.mhx> [--chip MB95F636H] [--secure] [--reset|--no-reset]
//!    f2mc-programmer-cli verify  <image> [--chip MB95F636H]
//!    f2mc-programmer-cli erase
//!    f2mc-programmer-cli readout <out.hex>
//!
//! 2) OpenOCD 兼容外壳（当 openocd 用）：
//!    f2mc-programmer-cli --openocd -s <dir> -f <cfg> -c "program app.hex verify reset exit"
//!    日志输出采用 OpenOCD 风格的 "Info :"/"Error :" 前缀，退出码 0=成功。
//!
//! ⚠ 纯 OpenOCD TCL 无法烧录 F2MC：`cmsis-dap cmd` 只把响应打进日志、不返回给
//! TCL（openocd v0.12.0 src/jtag/drivers/cmsis_dap.c cmsis_dap_handle_cmd_command），
//! 状态检查/校验/安全锁检测都做不到——因此采用本桥接方案。

use std::process::ExitCode;

use f2mc_core::chipdef;
use f2mc_core::flow::{self, FlowEvent, FlowOptions};
use f2mc_core::hexfile;
use f2mc_core::proto::F2mcClient;
use f2mc_core::transport::{self, DapTransport};
use f2mc_core::ProgError;

const DEFAULT_CHIP: &str = "MB95F636H";
/// readout 固定全地址空间（双 bank 布局，含 IO/RAM 区无妨）
const READOUT_START: u16 = 0x1000;
const READOUT_LEN: usize = 0xF000;

/// OpenOCD 风格信息日志
fn info(msg: &str) {
    println!("Info : {msg}");
}
/// OpenOCD 风格告警日志
fn warn(msg: &str) {
    println!("Warn : {msg}");
}
/// OpenOCD 风格错误日志（stderr）
fn err(msg: &str) {
    eprintln!("Error: {msg}");
}

/// 命令行参数（原生子命令模式）
struct Args {
    positional: Vec<String>,
    chip: String,
    secure: bool,
    reset: bool,
}

impl Args {
    /// 解析 argv（--chip/--secure/--reset/--no-reset + 位置参数）
    fn parse(argv: &[String]) -> Result<Self, String> {
        let mut a = Args {
            positional: Vec::new(),
            chip: std::env::var("F2MC_CHIP").unwrap_or_else(|_| DEFAULT_CHIP.into()),
            secure: false,
            reset: true,
        };
        let mut it = argv.iter();
        while let Some(s) = it.next() {
            match s.as_str() {
                "--chip" => {
                    a.chip = it.next().ok_or("--chip 缺参数")?.clone();
                }
                "--secure" => a.secure = true,
                "--reset" => a.reset = true,
                "--no-reset" => a.reset = false,
                other if other.starts_with("--") => return Err(format!("未知选项 {other}")),
                other => a.positional.push(other.to_string()),
            }
        }
        Ok(a)
    }
}

/// 打开编程器通道并构造 L1 客户端
fn open_client() -> Result<F2mcClient<Box<dyn DapTransport>>, String> {
    let channels = transport::enumerate();
    let ch = channels
        .first()
        .ok_or_else(|| "未找到 CMSIS-DAP 编程器（检查 USB 连接）".to_string())?;
    info(&format!("使用通道：{ch}"));
    let t = transport::open(ch).map_err(|e| format!("打开通道失败：{e}"))?;
    Ok(F2mcClient::new(t))
}

/// 读取并清洗烧录镜像，打印摘要与告警
fn load_image(path: &str, chip_name: &str) -> Result<hexfile::HexImage, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读取 {path} 失败：{e}"))?;
    let chip = chipdef::by_name(chip_name).ok_or_else(|| format!("未知型号 {chip_name}"))?;
    let img = hexfile::parse(&text, chip).map_err(|e| format!("解析 {path} 失败：{e}"))?;
    info(&format!(
        "镜像：{}，{} 字节，{} 个区间，型号 {}",
        img.format.label(),
        img.total_bytes(),
        img.segments.len(),
        chip.name
    ));
    for w in &img.warnings {
        warn(w);
    }
    Ok(img)
}

/// FlowEvent → OpenOCD 风格日志
fn on_event(e: FlowEvent) {
    match e {
        FlowEvent::StageStart(s) => info(&format!("=== {} ===", s.label())),
        FlowEvent::Progress(_, done, total) => {
            if total > 0 && done % 4096 < 1024 {
                info(&format!("进度 {:.0}%", done as f32 * 100.0 / total as f32));
            }
        }
        FlowEvent::Log(m) => info(&m),
        FlowEvent::Warn(m) => warn(&m),
    }
}

/// 核心错误 → 用户可读中文提示
fn map_err(e: ProgError) -> String {
    match e {
        ProgError::Cancelled => "已取消".into(),
        ProgError::SecurityLocked => "目标已加安全锁（需整片擦除解锁）".into(),
        ProgError::DeviceStatus(f2mc_core::error::StatusCode::Unsupported) => {
            "编程器无复位硬件，请人工断电重启目标板".into()
        }
        other => format!("{other}"),
    }
}

/// `program` 子命令：烧录（擦除+写入+校验）
fn cmd_program(image: &str, a: &Args) -> Result<(), String> {
    let img = load_image(image, &a.chip)?;
    let mut client = open_client()?;
    apply_chip(&mut client, a)?;
    let opts = FlowOptions { write_secure: a.secure, reset_after: a.reset };
    let r = flow::program(&mut client, &img, &opts, &mut on_event, &flow::default_cancel())
        .map_err(map_err)?;
    info(&format!(
        "烧录完成：写入 {} 字节，校验 {} 字节，用时 {:.1} s",
        r.bytes_written, r.bytes_verified, r.elapsed_secs
    ));
    if r.unlocked {
        warn("目标曾加安全锁，已整片擦除解锁");
    }
    let _ = client.disconnect();
    Ok(())
}

/// 把型号名下发给编程器（固件按系列匹配内嵌 DA，docs/DA 结构解析.md）
fn apply_chip(client: &mut F2mcClient<Box<dyn DapTransport>>, a: &Args) -> Result<(), String> {
    client
        .set_chip(&a.chip)
        .map_err(|e| format!("SET_CHIP 失败：{e}"))
}

/// `verify` 子命令：仅校验
fn cmd_verify(image: &str, a: &Args) -> Result<(), String> {
    let img = load_image(image, &a.chip)?;
    let mut client = open_client()?;
    apply_chip(&mut client, a)?;
    flow::verify_only(&mut client, &img, &mut on_event, &flow::default_cancel())
        .map_err(map_err)?;
    info("校验通过");
    let _ = client.disconnect();
    Ok(())
}

/// `erase` 子命令：仅整片擦除
fn cmd_erase(_a: &Args) -> Result<(), String> {
    let mut client = open_client()?;
    flow::erase_only(&mut client, &mut on_event, &flow::default_cancel()).map_err(map_err)?;
    info("擦除完成");
    let _ = client.disconnect();
    Ok(())
}

/// `recover` 子命令：烧录恢复（强制进入含整循环重试 + 整片擦除）
///
/// 用于上次烧录异常（如错配 DA 写坏目标 Flash）导致无法正常烧录时；
/// 参照 YM02 行为：能进编程模式就能整片擦除重来。
fn cmd_recover(_a: &Args) -> Result<(), String> {
    let mut client = open_client()?;
    flow::recover(&mut client, &mut on_event, &flow::default_cancel()).map_err(map_err)?;
    info("恢复完成");
    let _ = client.disconnect();
    Ok(())
}

/// `readout` 子命令：全地址空间读回并保存为 Intel HEX
fn cmd_readout(out: &str, a: &Args) -> Result<(), String> {
    let mut client = open_client()?;
    apply_chip(&mut client, a)?;
    let data = flow::read_out(
        &mut client,
        READOUT_START,
        READOUT_LEN,
        &mut on_event,
        &flow::default_cancel(),
    )
    .map_err(map_err)?;
    write_hex(out, READOUT_START as u32, &data)?;
    info(&format!("已保存 {out}（{} 字节）", data.len()));
    let _ = client.disconnect();
    Ok(())
}

/// 将读回数据写成 Intel HEX 文件（16 B/记录，含扩展线性地址记录）
fn write_hex(path: &str, base: u32, data: &[u8]) -> Result<(), String> {
    let mut s = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        let addr = base + (i * 16) as u32;
        let ext = (addr >> 16) as u8;
        let mut rec = vec![16u8, (addr >> 8) as u8, addr as u8, 0];
        rec.extend_from_slice(chunk);
        let mut line = String::new();
        if ext > 0 {
            let r = [2u8, 0, 0, 4, 0, ext];
            let sum: u8 = r.iter().fold(0, |a, b| a.wrapping_add(*b));
            line.push_str(&format!(":02000004{:04X}{:02X}\n", ext as u16, (!sum).wrapping_add(1)));
        }
        let sum: u8 = rec.iter().fold(0, |a, b| a.wrapping_add(*b));
        let chk = (!sum).wrapping_add(1);
        line.push(':');
        for b in &rec {
            line.push_str(&format!("{b:02X}"));
        }
        line.push_str(&format!("{chk:02X}\n"));
        s.push_str(&line);
    }
    s.push_str(":00000001FF\n");
    std::fs::write(path, s).map_err(|e| format!("写入 {path} 失败：{e}"))
}

/// `probes` 子命令：枚举 CMSIS-DAP 通道
fn cmd_probes() -> Result<(), String> {
    let list = transport::enumerate();
    if list.is_empty() {
        info("未发现 CMSIS-DAP 编程器");
    }
    for c in list {
        info(&format!("{c}"));
    }
    Ok(())
}

// ---------------------------------------------------------------- OpenOCD shim

/// 解析 openocd 风格参数：-s 忽略；-f 指定 F2MC 目标 cfg（实际解析，见 load_cfg）；
/// -c 收集命令串
/// 支持：program <file> [verify] [reset] exit、verify_image <file>、shutdown、init
fn run_openocd_shim(argv: &[String]) -> Result<(), String> {
    let mut cmds: Vec<String> = Vec::new();
    let mut cfg_path: Option<String> = None;
    let mut a = Args {
        positional: Vec::new(),
        chip: std::env::var("F2MC_CHIP").unwrap_or_else(|_| DEFAULT_CHIP.into()),
        secure: false,
        reset: false,
    };
    let mut it = argv.iter().peekable();
    while let Some(s) = it.next() {
        match s.as_str() {
            "-s" | "--search" => {
                let _ = it.next(); // 路径参数忽略
            }
            "-f" | "--file" => {
                cfg_path = Some(it.next().ok_or("-f 缺参数")?.clone());
            }
            "-c" | "--command" => {
                cmds.push(it.next().ok_or("-c 缺参数")?.clone());
            }
            _ => {}
        }
    }
    info("f2mc-programmer-cli OpenOCD 兼容模式（F2MC vendor 协议桥接）");

    // -f cfg：解析 set F2MC_CHIP / F2MC_SECURE / F2MC_RESET（环境变量已作默认值）
    if let Some(p) = &cfg_path {
        load_cfg(p, &mut a)?;
    }

    for c in &cmds {
        let tokens: Vec<&str> = c.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        match tokens[0] {
            "program" => {
                // program <file> [verify] [reset] [exit]；reset 关键字优先于 cfg 默认
                let file = tokens.get(1).ok_or("program 缺文件名")?;
                if tokens.iter().any(|t| *t == "reset") {
                    a.reset = true;
                }
                cmd_program(file, &a)?;
            }
            "verify_image" => {
                let file = tokens.get(1).ok_or("verify_image 缺文件名")?;
                cmd_verify(file, &a)?;
            }
            "init" | "shutdown" | "exit" | "targets" | "halt" | "reset" => {
                info(&format!("（桥接忽略）{c}"));
            }
            other => return Err(format!("不支持的 openocd 命令：{other}")),
        }
    }
    Ok(())
}

/// 解析 F2MC 目标 cfg（`set <键> <值>` 行，# 注释），应用到烧录参数
fn load_cfg(path: &str, a: &mut Args) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读取 cfg {path} 失败：{e}"))?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        if tok.next() != Some("set") {
            continue; // 非 set 行忽略（兼容混入的 TCL 注释等）
        }
        let (k, v) = match (tok.next(), tok.next()) {
            (Some(k), Some(v)) => (k, v),
            _ => continue,
        };
        match k {
            "F2MC_CHIP" => a.chip = v.to_string(),
            "F2MC_SECURE" => a.secure = v == "1",
            "F2MC_RESET" => a.reset = v == "1",
            _ => warn(&format!("cfg 未知键 {k}（忽略）")),
        }
    }
    info(&format!("cfg {path}：chip={} secure={} reset={}", a.chip, a.secure, a.reset));
    Ok(())
}

// ---------------------------------------------------------------- main

fn usage() {
    eprintln!(
        "f2mc-programmer-cli —— F2MC-8FX 烧录命令行（OpenOCD 兼容）

用法：
  f2mc-programmer-cli probes
  f2mc-programmer-cli program <image.hex|.mhx> [--chip MB95F636H] [--secure] [--reset|--no-reset]
  f2mc-programmer-cli verify  <image> [--chip MB95F636H]
  f2mc-programmer-cli erase
  f2mc-programmer-cli recover
  f2mc-programmer-cli readout <out.hex>

OpenOCD 兼容外壳（把本程序当 openocd 调用）：
  f2mc-programmer-cli --openocd -s <dir> -f <cfg> -c \"program app.hex verify reset exit\"

环境变量 F2MC_CHIP 指定默认型号（默认 {DEFAULT_CHIP}）；型号经 SET_CHIP 下发编程器，
固件按系列匹配内嵌 DA（docs/DA 结构解析.md）。"
    );
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || argv[0] == "-h" || argv[0] == "--help" {
        usage();
        return ExitCode::FAILURE;
    }

    // Ctrl+C：直接退出（进程被杀，固件侧命令超时自恢复）
    let r = if argv.iter().any(|a| a == "--openocd") {
        run_openocd_shim(&argv)
    } else {
        match Args::parse(&argv).and_then(|a| {
            let cmd = a.positional.first().cloned().unwrap_or_default();
            match cmd.as_str() {
                "probes" => cmd_probes(),
                "program" => {
                    let f = a.positional.get(1).ok_or("program 缺文件名".to_string())?;
                    cmd_program(f, &a)
                }
                "verify" => {
                    let f = a.positional.get(1).ok_or("verify 缺文件名".to_string())?;
                    cmd_verify(f, &a)
                }
                "erase" => cmd_erase(&a),
                "recover" => cmd_recover(&a),
                "readout" => {
                    let f = a.positional.get(1).ok_or("readout 缺输出文件名".to_string())?;
                    cmd_readout(f, &a)
                }
                other => Err(format!("未知命令 {other}")),
            }
        }) {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    };

    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            err(&e);
            ExitCode::FAILURE
        }
    }
}
