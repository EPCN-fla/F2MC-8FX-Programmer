/*
 * DAP_config.h — CMSIS-DAP 移植配置（基于 ARM 官方模板适配，Apache-2.0）
 *
 * SWD 引擎启用（官方 SW_DP.c 位流 + 本文件引脚端口层）。
 *   SWCLK = PB13（推挽输出）；SWDIO_OUT = PB14、SWDIO_IN = PB12（220Ω 并联上针座），
 *   目标 3.3V 电平（STM32F103CBT6），推挽直接驱动无需开漏/上拉。
 * F2MC 操作全部走 ID_DAP_Vendor0(0x80) → App/vendor.c → App/cmd.c。
 *
 * ⚠ 引脚仲裁：SWD 与 F2MC wire.c 共享 PB12/PB14（及 PB13）。
 *   PORT_SWD_SETUP() 把三脚切到 SWD 角色；PORT_OFF() 全部释放为浮空输入
 *   （与 wire_init 防倒灌状态一致）。F2MC 侧 wire_send 每次自建开漏模式，
 *   天然兼容；但同一时刻只允许一类目标会话（固件使用说明 §3.6）。
 */

#ifndef __DAP_CONFIG_H__
#define __DAP_CONFIG_H__

#include <stdint.h>
#include <string.h>
#include "stm32f1xx.h"          /* GPIOB 寄存器 */
#include "led.h"                /* LED_CONNECTED_OUT → led_set_mode */

//**************************************************************************************************
// 硬件/时钟配置
#define CPU_CLOCK               72000000U   ///< CPU Clock: STM32F103 @ 72 MHz
#define IO_PORT_WRITE_CYCLES    2U          ///< I/O 写周期（BSRR/BRR 单次写 ~2 周期）

// 调试端口能力
#define DAP_SWD                 1           ///< SWD: 启用（官方 SW_DP.c）
#define DAP_JTAG                0           ///< JTAG: 不支持
#define DAP_JTAG_DEV_CNT        0U          ///< JTAG 链设备数
#define DAP_DEFAULT_PORT        1U          ///< 默认端口: 1 = SWD
#define DAP_DEFAULT_SWJ_CLOCK   4000000U    ///< 默认 SWD 时钟 4 MHz（名义值；bit-bang 实际 ~1.7-2MHz，可用 adapter speed 调整）

// DAP 包缓冲（F103 单包 64 B，严格请求-响应模型）
#define DAP_PACKET_SIZE         64U         ///< 单包大小
#define DAP_PACKET_COUNT        1U          ///< 缓冲包数

// SWO / UART / 时间戳：均不可用
#define SWO_UART                0
#define SWO_UART_DRIVER         0
#define SWO_UART_MAX_BAUDRATE   0U
#define SWO_MANCHESTER          0
#define SWO_BUFFER_SIZE         0U
#define SWO_STREAM              0
#define TIMESTAMP_CLOCK         0U
#define DAP_UART                0
#define DAP_UART_DRIVER         0
#define DAP_UART_RX_BUFFER_SIZE 0U
#define DAP_UART_TX_BUFFER_SIZE 0U
#define DAP_UART_USB_COM_PORT   0

// 目标信息（TARGET_FIXED=0：probe-rs/OpenOCD 自选目标）
#define TARGET_FIXED            0
#define TARGET_DEVICE_VENDOR    ""
#define TARGET_DEVICE_NAME      ""
#define TARGET_BOARD_VENDOR     ""
#define TARGET_BOARD_NAME       ""

#include "cmsis_compiler.h"

//**************************************************************************************************
// 信息字符串（DAP_Info 命令）
__STATIC_INLINE uint8_t DAP_GetVendorString (char *str) {
  const char *s = "f2mc";
  (void)strcpy(str, s);
  return (uint8_t)(sizeof("f2mc"));
}

__STATIC_INLINE uint8_t DAP_GetProductString (char *str) {
  const char *s = "F2MC-LINK CMSIS-DAP";
  (void)strcpy(str, s);
  return (uint8_t)(sizeof("F2MC-LINK CMSIS-DAP"));
}

__STATIC_INLINE uint8_t DAP_GetSerNumString (char *str) {
  /* TODO: 从 STM32 UID96 生成唯一序列号 */
  const char *s = "0001A";
  (void)strcpy(str, s);
  return (uint8_t)(sizeof("0001A"));
}

__STATIC_INLINE uint8_t DAP_GetTargetDeviceVendorString (char *str) {
  (void)str; return 0U;
}
__STATIC_INLINE uint8_t DAP_GetTargetDeviceNameString (char *str) {
  (void)str; return 0U;
}
__STATIC_INLINE uint8_t DAP_GetTargetBoardVendorString (char *str) {
  (void)str; return 0U;
}
__STATIC_INLINE uint8_t DAP_GetTargetBoardNameString (char *str) {
  (void)str; return 0U;
}
__STATIC_INLINE uint8_t DAP_GetProductFirmwareVersionString (char *str) {
  const char *s = "0.1.0";   /* 与 App/cmd.h 的 FW_VER_* 保持一致 */
  (void)strcpy(str, s);
  return (uint8_t)(sizeof("0.1.0"));
}

//**************************************************************************************************
// SWD 引脚宏（SW_DP.c 调用）
//   CRH 位域：PB12=nibble4(shift16) PB13=nibble5(shift20) PB14=nibble6(shift24)
//   模式值：0x3 = 推挽输出 50MHz；0x4 = 浮空输入
#define DAP_CRH_PP_OUT(pin_shift)   (0x3U << (pin_shift))
#define DAP_CRH_FLOAT_IN(pin_shift) (0x4U << (pin_shift))

/** SWCLK 时钟线（PB13） */
#define PIN_SWCLK_TCK_SET()     (GPIOB->BSRR = (1U << 13))
#define PIN_SWCLK_TCK_CLR()     (GPIOB->BRR  = (1U << 13))

/** SWDIO 数据输出（PB14；TMS 复用同一宏）
 *  ⚠ 语义=输出参数的 bit0（官方 SW_DP.c 的 SW_WRITE_BIT 传整个 val 再 >>=1，
 *     高位不参与；写成 (bit)? 判非零会把 0b10 错驱成 1，导致 IDR 读取失败） */
#define PIN_SWDIO_TMS_SET()     (GPIOB->BSRR = (1U << 14))
#define PIN_SWDIO_TMS_CLR()     (GPIOB->BRR  = (1U << 14))
#define PIN_SWDIO_OUT(bit)      (((bit) & 1U) ? (GPIOB->BSRR = (1U << 14)) \
                                               : (GPIOB->BRR  = (1U << 14)))

/** SWDIO 数据输入（PB12，220Ω 并联点回读） */
#define PIN_SWDIO_IN()          ((GPIOB->IDR >> 12) & 1U)

/** SWDIO 方向：输出使能=PB14 推挽；禁止=PB14 释放浮空（由目标驱动，PB12 读） */
#define PIN_SWDIO_OUT_ENABLE()  (GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 24)) | (0x3U << 24))
#define PIN_SWDIO_OUT_DISABLE() (GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 24)) | (0x4U << 24))

/** nRESET：无 RST 硬件（PA1 已舍弃）。IN 恒读 1（未复位），OUT 忽略。 */
#define PIN_nRESET_IN()         (1U)
#define PIN_nRESET_OUT(bit)     ((void)(bit))

/** 回读与 JTAG 侧引脚（DAP_SWJ_Pins 命令用；JTAG 无硬件，输入给无害值） */
#define PIN_SWCLK_TCK_IN()      ((GPIOB->IDR >> 13) & 1U)
#define PIN_SWDIO_TMS_IN()      PIN_SWDIO_IN()
#define PIN_TDO_IN()            PIN_SWDIO_IN()
#define PIN_TDI_OUT(bit)        ((void)(bit))
#define PIN_TDI_IN()            (0U)
#define PIN_nTRST_OUT(bit)      ((void)(bit))
#define PIN_nTRST_IN()          (1U)

//**************************************************************************************************
// 端口控制

/** JTAG：不支持（DAP_JTAG=0，不会被调用） */
__STATIC_INLINE void PORT_JTAG_SETUP (void) {}

/**
 * SWD 端口接管：PB13/PB14 推挽输出（空闲高），PB12 浮空输入。
 * 先全释放再置角色，避免从 F2MC 开漏状态直接切换产生毛刺。
 */
__STATIC_INLINE void PORT_SWD_SETUP (void) {
  GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 24)) | (0x4U << 24);   /* PB14 释放 */
  GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 20)) | (0x4U << 20);   /* PB13 释放 */
  GPIOB->BSRR = (1U << 13) | (1U << 14);                      /* 空闲高 */
  GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 20)) | (0x3U << 20);   /* PB13 PP 输出 */
  GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 24)) | (0x3U << 24);   /* PB14 PP 输出 */
}

/** 端口释放：三脚全部浮空输入（与 wire_init 防倒灌状态一致） */
__STATIC_INLINE void PORT_OFF (void) {
  GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 24)) | (0x4U << 24);   /* PB14 in */
  GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 20)) | (0x4U << 20);   /* PB13 in */
  GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 16)) | (0x4U << 16);   /* PB12 in */
}

// 状态 LED
/** 连接灯：DAP_Connect→常亮，DAP_Disconnect→熄灭（TIM1 PWM，见 led.c）。
 *  ⚠ 与 F2MC 上位机在线语义共用一颗 LED：若两侧会话交替，后到的操作覆盖灯效。 */
__STATIC_INLINE void LED_CONNECTED_OUT (uint32_t bit) {
  led_set_mode(bit ? LED_MODE_SOLID : LED_MODE_OFF);
}
/** 运行灯：传输活动脉冲过于频繁（TIM1 PWM 按位翻转不划算），不联动。 */
__STATIC_INLINE void LED_RUNNING_OUT (uint32_t bit) { (void)bit; }

// 时间戳（TIMESTAMP_CLOCK=0，不会被调用）
__STATIC_INLINE uint32_t TIMESTAMP_GET (void) { return 0U; }

//**************************************************************************************************
/** DAP 初始化（rt_application_init 调用一次）：端口归位释放。 */
__STATIC_INLINE void DAP_SETUP (void) {
  PORT_OFF();
}

/** 复位目标（标准 DAP ResetTarget 命令）。
 *  无 RST 引脚（v2 引脚表 PA1 已舍弃），返回 0 = 不支持。 */
__STATIC_INLINE uint8_t RESET_TARGET (void) { return 0U; }

#endif /* __DAP_CONFIG_H__ */
