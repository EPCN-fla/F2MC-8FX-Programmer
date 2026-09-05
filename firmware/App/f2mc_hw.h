/**
 * @file f2mc_hw.h
 * @brief 目标接口硬件引脚/阈值统一定义（F3~F5 共享）
 */
#ifndef F2MC_HW_H
#define F2MC_HW_H

#include "drv_pin.h"

/**
 * @name 目标 DBG 单线（分脚结构：PB14 经 33Ω 到 DBG 端子，PB14 经 220Ω 到 PB12）
 * @warning 目标 3.3V/5V 均支持；5V 时目标 VIH≈3.5V > PB14 推挽高 3.3V，
 *   故 TX 一律开漏（STM32F1 原生 OD 模式）：发 1=释放（目标侧 2.2k 上拉），
 *   发 0=输出低；空闲=输入。PB12/PB14 为 FT 脚，输入可直收 5V。
 * @{ */
#define DBG_TX_PIN          DRV_PIN('B', 14)  /**< PB14 → DBG（TX，开漏） */
#define DBG_RX_PIN          DRV_PIN('B', 12)  /**< PB12 ← DBG（RX，EXTI 下降沿） */
#define SWCLK_PIN           DRV_PIN('B', 13)  /**< PB13 SWCLK，恒输入（防误短接互顶） */
/** @} */

/**
 * @name 目标 VCC 检测（F2MC-LINK v1.1：PB0 = ADC1_IN8；v1.0 为 PA0/IN0——PA0 被原设计 WKUP 占用。
 *       4.7k:4.7k 分压 → 引脚电压 = VCC/2，采样点在电源开关之后）
 * @{ */
#define VCC_DIVIDER_NUM     2U      /**< 分压比（VCC = 引脚电压 × 2） */
#define VCC_ON_MV           2500U   /**< >2.5V 视为目标已上电 */
#define VCC_OFF_MV          500U    /**< <0.5V 视为目标已断电 */

/** @name 目标电源开关（F2MC-LINK v1.1：PA8=PWR_EN → EL817 光电管通路 + SS8050 驱动）
 * @{ */
#define PWR_EN_PIN          DRV_PIN('A', 8)   /**< 高有效；默认低=目标不上电 */
/** @} */
/** @} */

#endif /* F2MC_HW_H */
