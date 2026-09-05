/**
 * @file pwr.c
 * @brief 目标电源控制（PA8 → EL817+SS8050 开关）+ VCC 检测（PB0 ADC1_IN8）
 */
#include "pwr.h"
#include "f2mc_hw.h"
#include "vendor.h"
#include "cmd.h"
#include "drv_pin.h"
#include "rtt_log.h"
#include "adc.h"

#define ADC_VREF_MV         3300U   /**< ADC 参考电压（mV） */
#define ADC_FULL_SCALE      4095U   /**< 12 位满量程 */
#define ADC_CONV_TIMEOUT_MS 10U     /**< 单次转换超时 */
#define VCC_POLL_MS         50U     /**< pwr_wait_* 轮询间隔 */
#define PWR_RISE_TIMEOUT_MS 3000U   /**< 上电上升确认超时（开关为电子通路，应 <<1s） */

void pwr_init(void)
{
    /* 防倒灌前提：固件启动先把 PWR_EN 归位低（CubeMX 默认 RESET，双保险） */
    rt_pin_mode(PWR_EN_PIN, PIN_MODE_OUTPUT);
    rt_pin_write(PWR_EN_PIN, PIN_LOW);

    /* F1 上电后建议做一次 ADC 自校准（ADC 时钟 12 MHz，CubeMX 已配好） */
    if (HAL_ADCEx_Calibration_Start(&hadc1) != HAL_OK)
        LOGW("ADC calibration failed");
}

uint8_t pwr_switch(rt_bool_t on)
{
    if (on)
    {
        rt_pin_write(PWR_EN_PIN, PIN_HIGH);
        /* 上升确认：超时=过载/短路/未接目标 → 立即自动关断（保险丝之外的
         * 唯一过流保护；光电管通流上限 ~50mA，不能靠硬扛） */
        if (pwr_wait_on(PWR_RISE_TIMEOUT_MS) != RT_EOK)
        {
            rt_pin_write(PWR_EN_PIN, PIN_LOW);
            LOGE("pwr: switch on failed (VCC=%lu mV), auto off",
                 (unsigned long)pwr_vcc_mv());
            return L1_ST_PWR_FAULT;
        }
        LOGI("pwr: on (%lu mV)", (unsigned long)pwr_vcc_mv());
    }
    else
    {
        rt_pin_write(PWR_EN_PIN, PIN_LOW);
        LOGI("pwr: off");
    }
    return L1_ST_OK;
}

uint32_t pwr_vcc_mv(void)
{
    uint32_t raw;

    (void)HAL_ADC_Start(&hadc1);
    if (HAL_ADC_PollForConversion(&hadc1, ADC_CONV_TIMEOUT_MS) != HAL_OK)
    {
        (void)HAL_ADC_Stop(&hadc1);
        LOGE("ADC poll timeout");
        return 0U;
    }
    raw = HAL_ADC_GetValue(&hadc1);
    (void)HAL_ADC_Stop(&hadc1);

    /* 引脚 mV = raw × Vref / 4095；目标 VCC = 引脚 × 分压比 */
    return raw * ADC_VREF_MV * VCC_DIVIDER_NUM / ADC_FULL_SCALE;
}

/**
 * @brief  轮询等待 VCC 越过阈值（50ms 间隔，可响应 ABORT 中止）
 * @param  threshold_mv 阈值
 * @param  wait_above   RT_TRUE=等上升；RT_FALSE=等跌落
 * @param  timeout_ms  超时
 * @return RT_EOK / -RT_ETIMEOUT / PWR_E_ABORT
 */
static rt_err_t pwr_wait(uint32_t threshold_mv, rt_bool_t wait_above,
                         uint32_t timeout_ms)
{
    rt_tick_t deadline = rt_tick_get() + rt_tick_from_millisecond(timeout_ms);

    for (;;)
    {
        uint32_t mv = pwr_vcc_mv();
        rt_bool_t hit = wait_above ? (mv >= threshold_mv) : (mv <= threshold_mv);
        if (hit)
            return RT_EOK;
        if (cmd_abort_pending() != 0)          /* ABORT(0x12)：等待可被中止 */
            return PWR_E_ABORT;
        if ((rt_int32_t)(rt_tick_get() - deadline) >= 0)   /* 有符号差抗回绕 */
            return -RT_ETIMEOUT;
        rt_thread_mdelay(VCC_POLL_MS);
    }
}

rt_err_t pwr_wait_on(uint32_t timeout_ms)
{
    return pwr_wait(VCC_ON_MV, RT_TRUE, timeout_ms);
}

rt_err_t pwr_wait_off(uint32_t timeout_ms)
{
    return pwr_wait(VCC_OFF_MV, RT_FALSE, timeout_ms);
}
