# F2MC-8FX-Programmer

富士通 **F2MC-8FX** 系列（MB95630H 等全部 216 款）开源烧录/仿真编程器——自制编程器固件 + Rust 上位机，**同一硬件兼作 STM32 SWD 调试器/烧录器**。

## 特性

**F2MC-8FX 烧录**（自研 vendor 协议 over CMSIS-DAP）
- 全部 216 款型号（内置型号库 + 关键词检索），自动识别双 bank Flash 布局
- 擦除/烧录/校验/读取，36 KB 全片约 25~40 s；Intel HEX 与 S-record 自动嗅探
- 安全锁（0xFFFC）写入与自动解锁；NVR 区自动剔除与校验跳过；CR Trimming 失配自动回写
- 会话保持（连续烧录/校验/读取）、ABORT 秒级强制取消

**STM32 烧录/仿真**（标准 CMSIS-DAP）
- 固件内置 ARM 官方 SWD 引擎：probe-rs / OpenOCD / pyOCD 直接识别使用
- 上位机 STM32 路径由 probe-rs 驱动，支持全系 STM32 与芯片自动识别

**USB 全平台免驱**
- CMSIS-DAP **v1(HID) + v2(Bulk) 双接口**同一设备：Win8.1+ 走 v2（MS OS 2.0 自动绑 WinUSB），**Win7 走 v1（HID 天生免驱）**，Linux/macOS 免驱

**上位机三形态**（Rust workspace，`hmi/`）
- `f2mc-gui`：egui 工业风格 GUI（双目标家族切换、拖放烧录、进度/取消/日志）
- `f2mc-cli`：命令行（OpenOCD 风格日志，退出码语义）
- **OpenOCD 兼容桥**：`f2mc-programmer-cli --openocd` 实现像调 OpenOCD 一样烧录 F2MC

## 快速上手

1. **硬件**：按 `diagram/F2MC-LINK_v1.1.pdf` 打样 F2MC-LINK v1.1 编程器（STM32F103C8T6，自动上电）；
2. **固件**：STM32CubeMX 打开 `firmware/F2MC-LINK.ioc` 生成 STM32CubeIDE 工程（ARM-GCC），编译得 `F2MC-LINK.hex`，经板子自身 SWD 口（PA13/PA14）烧录；
3. **上位机**：`cd hmi && cargo build --release -p f2mc-gui -p f2mc-cli`（产物 `f2mc-programmer-gui.exe` / `f2mc-programmer-cli.exe`）；
4. **烧录 F2MC**：GUI 选型号 → 拖入 .mhx/.hex → 烧录；或 `f2mc-programmer-cli program app.mhx --chip MB95F636H`；
5. **烧录 STM32**：probe-rs / OpenOCD 直接选本设备（CMSIS-DAP）。

详细步骤见 **[docs/固件使用说明.md](docs/固件使用说明.md)** 与 **[docs/上位机使用说明.md](docs/上位机使用说明.md)**。

## 原理速览

```
上位机(Rust) ──USB──► 编程器固件(F103) ──DBG 单线 UART──► F2MC-8FX 目标
   │                     │ CMSIS-DAP v1/v2 双接口
   │                     ├─ vendor 命令(0x80) → New8FX 引擎（握手/擦除/INIT DA.BIN/读写）
   │                     └─ 标准 DAP 命令 → SWD 引擎 ──SWD──► STM32 目标
   └─ probe-rs / OpenOCD 兼容桥
```

- F2MC 串行编程协议（L2）依据：富士通《New 8FX Serial PGM Spec V1.3.0》（见致谢节获取途径）；
- 上位机↔编程器协议（L1）约定：`docs/通信协议约定.md`；
- 硬件手册：`docs/references/New 8FX MB95630H Series HARDWARE MANUAL V2.pdf`（第 25 章串行编程连接、第 26 章 Flash 算法、第 27 章 NVR）。

## 项目结构

```
F2MC-8FX-Programmer/
├── diagram/              # F2MC-LINK v1.1 原理图
├── docs/
│   ├── references/       # 参考资料
│   ├── 固件使用说明.md
│   ├── 上位机使用说明.md
│   ├── 通信协议约定.md
│   └── DA 结构解析.md
├── firmware/             # 编程器固件（STM32F103C8T6，CubeMX HAL + RT-Thread Nano）
│                         #   CMSIS-DAP v1/v2 双接口 + vendor(0x80) 命令路由 + New8FX L2 引擎 + SWD 引擎
│                         #   说明文档：docs/固件使用说明.md
├── hmi/                  # Rust 上位机（f2mc-core / stm32-flash / f2mc-gui / f2mc-cli + openocd 配置）
│                         #   说明文档：docs/上位机使用说明.md
├── LICENSE
└── README.md
```

## 致谢与参考

- [BruceSuen/8FX-MCU](https://github.com/BruceSuen/8FX-MCU) —— New8FX 串行编程规范 PDF 与官方 BGM 适配器固件源码
- [mnaberez/f2mc8dasm](https://github.com/mnaberez/f2mc8dasm) —— F2MC-8FX DA 线性反汇编
- [CMSIS-DAP](https://github.com/ARM-software/CMSIS-DAP) / [DAPLink](https://github.com/ARMmbed/DAPLink) —— 参考固件
- [probe-rs](https://probe.rs) —— 上位机 STM32 路径
- [RT-Thread Nano](https://www.rt-thread.io/) —— 固件 RTOS
- 富士通 SOFTUNE 随附 FGM Monitor —— 在线调试功能参考

## 许可证

[GPL-3.0](LICENSE)
