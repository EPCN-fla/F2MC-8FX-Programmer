/**
 * @file pwr.h
 * @brief 目标电源控制 + VCC 检测
 *
 * 控制：PA8=PWR_EN 驱动 EL817+SS8050 光电管开关（F2MC-LINK v1.1，通流上限 ~50mA）；
 * 检测：PB0=ADC1_IN8（4.7k:4.7k 分压，采样点在开关后，HAL ADC 轮询）。
 */
#ifndef PWR_H
#define PWR_H

#include <stdint.h>
#include <rtthread.h>

/** @brief ADC 校准 + PWR_EN 归位低（rt_application_init 调用一次） */
void     pwr_init(void);
/** @brief 读取当前目标 VCC（mV，已乘分压比 ×2） */
uint32_t pwr_vcc_mv(void);
/**
 * @brief  目标电源开关
 * @param  on RT_TRUE=上电（等 VCC 上升 >2.5V 确认，3s 未达判过载/未接→自动关断）
 *            RT_FALSE=断电
 * @return L1_ST_OK / L1_ST_PWR_FAULT（上电失败，已自动关断）
 */
uint8_t  pwr_switch(rt_bool_t on);
/**
 * @brief  等 VCC 上升到 > VCC_ON_MV
 * @param  timeout_ms 超时
 * @return RT_EOK / -RT_ETIMEOUT / PWR_E_ABORT（被 ABORT(0x12) 中止）
 */
rt_err_t pwr_wait_on(uint32_t timeout_ms);
/** @brief 等 VCC 跌落到 < VCC_OFF_MV（返回值同 pwr_wait_on） */
rt_err_t pwr_wait_off(uint32_t timeout_ms);

/** @brief pwr_wait_* 被 ABORT 中止的返回码 */
#define PWR_E_ABORT     ((rt_err_t)(-2))

#endif /* PWR_H */
