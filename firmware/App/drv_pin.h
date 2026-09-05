/**
 * @file drv_pin.h
 * @brief RT-Thread 风格最小 pin 驱动层（Nano 无官方 pin 框架）
 *
 * API 语义与 RT-Thread 完整版 rt_pin_* 一致（引脚编号 = 端口号*16 + 引脚号，
 * PA0=0, PB12=28…），便于将来迁移。仅实现所需子集：
 * rt_pin_get / rt_pin_mode / rt_pin_write / rt_pin_read。
 *
 * @warning 时序关键路径（wire.c 的 500K bit-bang、EXTI 捕获）不经过本层，
 * 直接操作寄存器；本层用于 LED、模式切换等非关键路径。
 */
#ifndef __DRV_PIN_H__
#define __DRV_PIN_H__

#include <rtthread.h>

/** @name 电平
 * @{ */
#define PIN_LOW                 0x00
#define PIN_HIGH                0x01
/** @} */

/** @name 引脚模式（映射 STM32F1 CRL/CRH 编码）
 * @{ */
#define PIN_MODE_OUTPUT         0x00    /**< 推挽输出 */
#define PIN_MODE_OUTPUT_OD      0x01    /**< 开漏输出 */
#define PIN_MODE_INPUT          0x02    /**< 浮空输入 */
#define PIN_MODE_INPUT_PULLUP   0x03    /**< 上拉输入 */
#define PIN_MODE_INPUT_PULLDOWN 0x04    /**< 下拉输入 */
/** @} */

/** @name 中断模式（预留，未实现）
 * @{ */
#define PIN_IRQ_MODE_RISING             0x00
#define PIN_IRQ_MODE_FALLING            0x01
#define PIN_IRQ_MODE_RISING_FALLING     0x02
#define PIN_IRQ_DISABLE                 0x00
#define PIN_IRQ_ENABLE                  0x01
/** @} */

#define PIN_NONE                (-1)    /**< 无效引脚 */

/** @brief 引脚编号合成：PA0=0 … PA15=15，PB0=16 … PB15=31 */
#define DRV_PIN(port, pin)      ((rt_base_t)(((port) - 'A') * 16 + (pin)))

/**
 * @brief  按名字取引脚编号（"PA.9"/"PB12"/"PA9" 均可）
 * @return 引脚编号；非法返回 PIN_NONE
 */
rt_base_t   rt_pin_get(const char *name);
/** @brief 设置引脚模式（PIN_MODE_*；直写 CRL/CRH，临界区保护） */
void        rt_pin_mode(rt_base_t pin, rt_uint8_t mode);
/** @brief 写输出电平（BSRR/BRR 原子写） */
void        rt_pin_write(rt_base_t pin, rt_uint8_t value);
/** @brief 读输入电平（IDR） */
rt_int8_t   rt_pin_read(rt_base_t pin);

#endif /* __DRV_PIN_H__ */
