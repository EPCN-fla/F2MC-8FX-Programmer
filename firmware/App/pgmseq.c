/**
 * @file pgmseq.c
 * @brief 进入编程模式电气时序（固件使用说明 §3.3）
 *
 * @warning 时序核心：PB14 必须在目标上电"之前"拉低 DBG（目标无电时拉低
 * 安全——目标侧 2.2k 上拉接目标 VCC 侧，无倒灌路径）；若等 VCC 上升后再
 * 拉低，目标进用户模式，永远无法进入 PGM。
 *
 * 五步流程（F2MC-LINK v1.1 起上电/断电由固件经 PWR_EN 自动控制，零手动）：
 *   1. 目标仍带电（>2.5V）→ PB14 推挽拉低（经目标侧 2.2k 上拉主动泄放目标轨，
 *      防大 die 目标放电不净 POR 失败）→ 自动断电并等放电（<0.5V）；
 *   2. PB14 推挽输出低（步骤 1 已拉低时为幂等）；
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
#define DISCHARGE_DWELL_MS      300U    /**< 放电达标后继续保持断电的时长：
                                         *   外部轨 ≤0.5V 时大 die 目标内部轨可能
                                         *   仍高于 VPOR（实测 F698K 曾因此 POR 不净、
                                         *   DA 残留 500K 态致握手 200 次超时），
                                         *   保持 300ms 确保内部放电深度 */

uint8_t pgmseq_enter(void)
{
    LOGI("pgmseq: enter");

    /* 1. 目标必须处于断电状态：仍带电则自动断电并等放电（目标电容泄放需时间）。
     *    ⚠ 放电期间主动把 DBG 推挽拉低：DBG 经目标侧 2.2k 上拉接目标 VCC
     *    （开关后），拉低即以 ~2.3mA@5V 主动泄放目标轨——大 die/大电容目标
     *    仅靠芯片漏电 + 9.4k 分压（~0.5mA）放电又慢又浅，实测 10s 放不到
	 *    0.5V 以下 → 放电超时，或掉电不深 POR 不干净 → 握手 200 次重试后
	 *    TIMEOUT。小 die 放电快故无此问题。DBG 在断电前拉低同时满足 Spec
	 *    “上电前 DBG 已为低”，无副作用。 */
    if (pwr_vcc_mv() >= VCC_ON_MV)
    {
        rt_err_t wrc;
        LOGI("target still powered (%lu mV), switching off",
             (unsigned long)pwr_vcc_mv());
        rt_pin_mode(DBG_TX_PIN, PIN_MODE_OUTPUT);   /* 推挽拉低：主动泄放目标轨 */
        rt_pin_write(DBG_TX_PIN, PIN_LOW);
        (void)pwr_switch(RT_FALSE);
        wrc = pwr_wait_off(DISCHARGE_TIMEOUT_MS);
        if (wrc == PWR_E_ABORT)
        {
            rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);   /* 已驱动 PB14，须释放 */
            LOGW("pgmseq: aborted (wait off)");
            return L1_ST_ABORTED;
        }
        if (wrc != RT_EOK)
        {
            rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);
            LOGE("target not discharged (VCC=%lu mV)",
                 (unsigned long)pwr_vcc_mv());
            return L1_ST_TIMEOUT;
        }
        rt_thread_mdelay(DISCHARGE_DWELL_MS);   /* 保持断电 300ms，保证内部 POR 深度 */
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

/**
 * @brief 复位运行（无 RST 引脚，用目标电源开关实现）：断电 → 主动放电 → 上电
 * @note  ⚠ 不可只断电几百 ms：大 die/大电容目标放电慢，浅掉电不触发 POR，
 *        目标根本没复位（boot/DA 态继续跑，用户程序不运行）。
 *        放电措施与 pgmseq_enter 步骤 1 相同（DBG 拉低经 2.2k 主动泄放）。
 * @return L1_ST_OK / L1_ST_TIMEOUT（放电超时）/ L1_ST_PWR_FAULT（上电失败）
 */
uint8_t pgmseq_power_cycle_run(void)
{
    rt_err_t wrc;

    LOGI("pgmseq: power-cycle run");
    rt_pin_mode(DBG_TX_PIN, PIN_MODE_OUTPUT);   /* 推挽拉低：经 2.2k 主动泄放目标轨 */
    rt_pin_write(DBG_TX_PIN, PIN_LOW);          /*（仅断电期拉低；上电前须释放，
                                                * 否则目标会进 PGM 而非运行用户程序）*/
    (void)pwr_switch(RT_FALSE);
    wrc = pwr_wait_off(DISCHARGE_TIMEOUT_MS);       /* 等 VCC <0.5V */
    if (wrc == PWR_E_ABORT)
    {
        rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);
        LOGW("pgmseq: aborted (power-cycle)");
        return L1_ST_ABORTED;
    }
    if (wrc != RT_EOK)
    {
        rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);
        LOGE("reset_run: target not discharged (VCC=%lu mV)",
             (unsigned long)pwr_vcc_mv());
        return L1_ST_TIMEOUT;
    }
    rt_thread_mdelay(DISCHARGE_DWELL_MS);           /* 保持断电 300ms，保证 POR 深度 */
    rt_pin_mode(DBG_TX_PIN, PIN_MODE_INPUT);        /* 释放 DBG（上拉成形高） */
    return pwr_switch(RT_TRUE);                     /* 上电（3s 上升确认） */
}
