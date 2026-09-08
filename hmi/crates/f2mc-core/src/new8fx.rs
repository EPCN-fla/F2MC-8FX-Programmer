//! New8FX 串行编程协议常量（目标侧 L2 层关键地址与参数）。

/// FLASH_INIT(0x05) 参数：MB95630H 时钟配置（固件据此完成 500 Kbps 切换）
pub const FLASH_INIT_XX: u8 = 0x02;
pub const FLASH_INIT_YY: u8 = 0x7C;

/// NVR 校准区（CR Trimming 校准值存放地址，Blank Check / Verify 必须跳过）
pub const NVR_LOW: u32 = 0xFFBB;
pub const NVR_HIGH: u32 = 0xFFBF;
/// Blank Check / Verify 必须跳过的三个地址（回写未生效前读回值可能与写入不一致）
pub const VERIFY_SKIP: [u32; 3] = [0xFFBB, 0xFFBC, 0xFFBD];
/// 安全位地址（写 0x01 加锁，断电复位或 QUIT 后生效）
pub const SECURE_ADDR: u32 = 0xFFFC;

/// CR Trimming 比对地址表：NVR 与对应 RAM 镜像（Spec 7.11）
pub const CR_NVR_ADDRS: [u16; 3] = [0xFFBB, 0xFFBC, 0xFFBD];
pub const CR_RAM_ADDRS: [u16; 3] = [0x0FE7, 0x0FE4, 0x0FE5];

// INIT DA.BIN：固件内嵌，上位机经 SET_CHIP(0x13) 下发型号名，固件按系列匹配（docs/DA 结构解析.md）。
// ⚠ DA 与目标 boot ROM 常驻监视器布局严格配对（含绝对跳转地址），选错变体会导致 FLASH_INIT 后读写全灭。
