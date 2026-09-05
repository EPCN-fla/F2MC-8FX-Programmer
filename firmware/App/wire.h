/**
 * @file wire.h
 * @brief 单线 UART bit-bang 驱动（固件使用说明 §3.5：PB14=TX / PB12=RX，DWT 精确定时）
 *
 * 链路：单线半双工 8N1，空闲高，LSB first，62500 / 500000 bps 两档运行时切换。
 *
 * @warning EXTI12（PB12）平时必须保持 IMR 屏蔽（wire_init 默认 disarm），
 *   仅 wire_recv 期间 arm——目标未上电时 DBG 浮空，噪声会引发最高优先级
 *   中断风暴饿死全系统（T3 实测踩坑）。
 * @warning TX 开漏（目标 5V 兼容）：发 1 = 释放（目标侧 2.2k 上拉），
 *   发 0 = 输出低；禁止推挽输出高（3.3V 低于 5V 目标 VIH≈3.5V）。
 */
#ifndef WIRE_H
#define WIRE_H

#include <stdint.h>
#include <rtthread.h>

/** @name 波特率档位
 * @{ */
#define WIRE_BAUD_62500     62500UL     /**< 进模式/握手/时钟修改/预 init 擦除 */
#define WIRE_BAUD_500K      500000UL    /**< FLASH_INIT 之后（RW 阶段） */
/** @} */

/** @name wire_recv 错误码（返回值 <0 时）
 * @{ */
#define WIRE_E_TIMEOUT      (-1)        /**< 起始位超时 */
#define WIRE_E_FRAMING      (-2)        /**< 停止位校验失败（帧错误） */
#define WIRE_E_ABORT        (-3)        /**< 上位机 ABORT(0x12) 中止 */
/** @} */

/** @brief 初始化：DWT 使能 + EXTI12 屏蔽 + PB14 释放输入（防倒灌） */
void     wire_init(void);
/** @brief 运行时切换波特率（仅改位定时常量） */
void     wire_set_baud(uint32_t baud);
/**
 * @brief  发送数据（开漏 TX，DWT 滚动位边界）
 * @param  data    数据缓冲
 * @param  len     字节数
 * @param  gap_us  字节间间隔（µs，0=背靠背）
 * @return 0=OK
 */
int      wire_send(const uint8_t *data, uint16_t len, uint32_t gap_us);
/**
 * @brief  接收数据（62500=EXTI 时间戳采样；500K=RamFunc 轮询采样）
 * @param  buf        接收缓冲
 * @param  len        期望字节数
 * @param  timeout_ms 首字节超时（后续字节固定 10ms）
 * @return >0=字节数；<0=WIRE_E_* 错误码
 */
int      wire_recv(uint8_t *buf, uint16_t len, uint32_t timeout_ms);
/** @brief 发送 UART Break（拉低 ≥2 帧时长，通信恢复，固件使用说明 §2.8） */
void     wire_send_break(void);
/** @brief EXTI 累计触发次数（排障用） */
uint32_t wire_exti_hit_count(void);
/** @brief 62500 单线程逐位回读自测（开环 PB14→PB12 物理环回） */
rt_err_t wire_loopback_test(void);

/** @name 内部接口（ISR/接收窗口成对调用，见 wire.c）
 * @{ */
void     wire_rx_arm(void);     /**< 使能 EXTI12（先清挂起再开 IMR） */
void     wire_rx_disarm(void);  /**< 屏蔽 EXTI12 */
/** @} */

#endif /* WIRE_H */
