/**
 * @file pgmseq.c
 * @brief 进入编程模式电气时序（固件使用说明 §3.3）
 *
 * @warning 时序核心：PB14 必须在目标上电"之前"拉低 DBG（目标无电时拉低
 * 安全——目标侧 2.2k 上拉接目标 VCC 侧，无倒灌路径）；若等 VCC 上升后再
 * 拉低，目标进用户模式，永远无法进入 PGM。
 *
 * 五步流程（F2MC-LINK v1.1 起上电/断电由固件经 PWR_EN 自动控制，零手动）：
 *   1. 目标仍带电（>2.5V）→ 自动断电并等放电（<0.5V），超时报错；
 *   2. PB14 推挽输出低；
 *   3. 自动上电（pwr_switch，3s 上升确认，失败自动关断）；
 *   3b. 等 VCC 稳定（连续 3 次采样变化 <100mV）；
 *   4. 精确定时 1.2 s（DBG 低覆盖 VCC 上升且 ≥1 s，留裕量）；
 *   5. PB14 释放为输入（目标侧上拉把 DBG 拉高）→ new8fx.c 进入握手。
 *
 * 在 f2mc_task 上下文运行，可阻塞（典型 ~3s）；失败时已把 PB14 恢复为输入。
 * 全程可响应 ABORT(0x12) 中止。
 */
#include "pgmseq.h"
#include "f2mc_hw.h"
#include "pwr.h"
#include "vendor.h"
#include "cmd.h"
#include "rtt_log.h"
#include <rtthread.h>

#define DBG_HOLD_MS             1200U   /**< Spec 要求 ≥1 s，取 1.2 s 裕量 */
#define DISCHARGE_TIMEOUT_MS    10000U  /**< 等目标电容放电 10 s */

uint8_t pgmseq_enter(void)
{
    LOGI("pgmseq: enter");

    /* 1. 目标必须处于断电状态：仍带电则自动断电并等放电（目标电容泄放需时间） */
    if (pwr_vcc_mv() >= VCC_ON_MV)
    {
        rt_err_t wrc;
        LOGI("target still powered (%lu mV), switching off",
             (unsigned long)pwr_vcc_mv());
        (void)pwr_switch(RT_FALSE);
        wrc = pwr_wait_off(DISCHARGE_TIMEOUT_MS);
        if (wrc == PWR_E_ABORT)
        {
            LOGW("pgmseq: aborted (wait off)");
            return L1_ST_ABORTED;
        }
        if (wrc != RT_EOK)
        {
            LOGE("target not discharged");
            return L1_ST_TIMEOUT;
        }
    }

    /* 2. 上电前拉低 DBG（PB14 推挽输出低） */
    rt_pin_mode(DBG_TX_PIN, PIN_MODE_OUTPUT);
    rt_pin_write(DBG_TX_PIN, PIN_LOW);

    /* 3. 自动上电（上升确认 3s，过载/未接自动关断） */
    {
        uint8_t prc = pwr_switch(RT_TRUE);
        if (prc != L1_ST_OK)
        {
            rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);   /* 失败也要释放 */
            LOGE("power on failed");
            return prc;
        }
    }
    LOGI("VCC up (%lu mV), wait stable",
         (unsigned long)pwr_vcc_mv());

    /* 3b. 等 VCC 稳定（连续 3 次采样变化 <100mV，50ms 间隔，上限 10 s）。
     * Spec 要求“VCC 稳定后 DBG 低 ≥1 s”——上升缓慢的电源（软启动/大电容）
     * 会从 2.5V 检测点吃掉保持时间，必须先等稳定再开始 1.2 s 计时。 */
    {
        uint32_t prev = 0U;
        rt_int32_t stable = 0;
        rt_tick_t deadline = rt_tick_get() + rt_tick_from_millisecond(10000);
        while (stable < 3)
        {
            uint32_t mv = pwr_vcc_mv();
            if ((mv > prev ? mv - prev : prev - mv) < 100U)
                stable++;
            else
                stable = 0;
            prev = mv;
            if (cmd_abort_pending() != 0)
            {
                rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);
                LOGW("pgmseq: aborted (wait stable)");
                return L1_ST_ABORTED;
            }
            if ((rt_int32_t)(rt_tick_get() - deadline) >= 0)
            {
                rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);
                LOGE("VCC not stable");
                return L1_ST_TIMEOUT;
            }
            rt_thread_mdelay(50);
        }
    }
    LOGI("VCC stable (%lu mV), holding DBG low %u ms",
         (unsigned long)pwr_vcc_mv(), DBG_HOLD_MS);

    /* 4. 精确定时 1.2 s（50ms 粒度轮询，可响应 ABORT 中止） */
    for (uint8_t i = 0U; i < (DBG_HOLD_MS / 50U); i++)
    {
        if (cmd_abort_pending() != 0)
        {
            rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);
            LOGW("pgmseq: aborted (hold)");
            return L1_ST_ABORTED;
        }
        rt_thread_mdelay(50);
    }

    /* 5. 释放 PB14 为输入，目标侧 2.2k 上拉把 DBG 拉高 */
    rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);
    LOGI("DBG released, enter-pgm sequence done");

    return L1_ST_OK;
}
