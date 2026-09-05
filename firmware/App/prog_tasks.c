/**
 * @file prog_tasks.c
 * @brief RT-Thread 任务胶水（固件使用说明 §3.2）
 *
 * 全部线程静态创建（RT_USING_HEAP 未定义），栈/TCB/消息池均为静态内存。
 */
#include "prog_tasks.h"
#include "drv_pin.h"
#include "rtt_log.h"
#include "pwr.h"
#include "f2mc_hw.h"
#include "wire.h"
#include "dapif.h"
#include "led.h"
#include "DAP.h"
#include <rthw.h>
#include "main.h"

extern void rt_hw_board_init(void);   /**< CubeMX pack board.c 提供 */

/** @name 线程优先级（RT_THREAD_PRIORITY_MAX=8，数值越小越高）
 * @note 无 blink 线程：LED 由 TIM1_CH2 硬件 PWM 驱动（led.c）。历史教训——
 * blink 线程高于 f2mc 曾导致 flash_init 上传丢帧（2026-08-06 A/B 实证）；
 * 低于 f2mc 则在长自旋期被饿死。硬件 PWM 两全。
 * @{ */
#define PRIO_USB    3
#define PRIO_F2MC   4
/** @} */

/** @name 线程资源（静态）
 * @{ */
static struct rt_thread usb_tcb;
static struct rt_thread f2mc_tcb;
ALIGN(RT_ALIGN_SIZE) static rt_uint8_t usb_stack[768];
ALIGN(RT_ALIGN_SIZE) static rt_uint8_t f2mc_stack[2048];  /**< 含 ADC/日志/引擎调用链，余量放大 */
/** @} */

/** @name IPC 对象（静态）
 * @{ */
static struct rt_messagequeue cmd_mq;
static struct rt_semaphore    rsp_sem;
ALIGN(RT_ALIGN_SIZE) static rt_uint8_t cmd_mq_pool[CMD_MSG_SIZE * CMD_MSG_MAX];
/** @} */

rt_mq_t  g_cmd_mq = RT_NULL;
rt_sem_t g_rsp_sem = RT_NULL;
rt_uint8_t g_rsp_buf[RSP_BUF_SIZE];

/**
 * @brief USB 任务：等 f2mc_task 的响应信号量 → 经对应接口 EP IN 发回
 */
static void usb_entry(void *parameter)
{
    RT_UNUSED(parameter);

    LOGI("usb_task started\n");

    while (1)
    {
        rt_sem_take(g_rsp_sem, RT_WAITING_FOREVER);
        dapif_tx_response(g_rsp_buf[0], g_rsp_buf + 2, g_rsp_buf[1]);
    }
}

/**
 * @brief F2MC 任务：取 DAP 请求包 → DAP_ExecuteCommand → 投递响应
 * @note  vendor 命令（0x80）由 vendor.c → cmd.c 分发；L2 长操作在此上下文
 *        阻塞执行。启动时先做一次 wire 环回自测。
 */
static void f2mc_entry(void *parameter)
{
    rt_uint8_t msg[CMD_MSG_SIZE];
    rt_uint32_t ret;

    RT_UNUSED(parameter);

    LOGI("f2mc_task started\n");

    /* 上电 wire 自测（62500 loopback 逐位回读）；目标带电则跳过，失败不阻塞。
     * 目标接着但未上电时也要跳过：目标侧上拉接到无电 VCC 轨+ESD 钳位会把
     * DBG 拉低，自测靠的 40k 弱上拉形不成高电平（实测 FAIL）。 */
    if (pwr_vcc_mv() < VCC_OFF_MV)
    {
        if (rt_pin_read(DBG_RX_PIN) == PIN_HIGH)
        {
            if (wire_loopback_test() == RT_EOK)
                LOGI("wire loopback: PASS\n");
            else
                LOGE("wire loopback: FAIL\n");
        }
        else
        {
            LOGW("wire loopback: skipped (target attached)\n");
        }
    }
    else
    {
        LOGW("wire loopback: skipped (target powered)\n");
    }

    while (1)
    {
        /* 返回值为 rt_err_t；消息定长 CMD_MSG_SIZE */
        if (rt_mq_recv(g_cmd_mq, msg, sizeof(msg), RT_WAITING_FOREVER) != RT_EOK)
            continue;

        /* DAP 请求-响应模型：一包完成（通信协议约定 §1）。msg[0]=接口, msg[1]=长度 */
        ret = DAP_ExecuteCommand(msg + 2, g_rsp_buf + 2);

        g_rsp_buf[0] = msg[0];
        g_rsp_buf[1] = (rt_uint8_t)(ret & 0xFFFFU);   /* 响应字节数（低 16 位） */

        rt_sem_release(g_rsp_sem);
    }
}

/**
 * @brief RT-Thread 应用线程/IPC 创建（rtthread_startup 调用）
 */
void rt_application_init(void)
{
    rt_err_t rc;

    rtt_log_init();
    LOGI("F2MC-LINK boot\n");

    DAP_Setup();   /* CMSIS-DAP DAP_Data 默认值（DAP.c） */

    pwr_init();    /* ADC 自校准（F3 VCC 检测） */

    wire_init();   /* EXTI12 屏蔽（防浮空中断风暴） + PB14 释放输入（防倒灌） */

    led_init();    /* 状态 LED：TIM1_CH2 硬件 PWM，上电默认 OFF（未连接上位机） */

    /* IPC 对象 */
    rc = rt_mq_init(&cmd_mq, "cmd", cmd_mq_pool, CMD_MSG_SIZE,
                    sizeof(cmd_mq_pool), RT_IPC_FLAG_FIFO);
    RT_ASSERT(rc == RT_EOK);
    g_cmd_mq = &cmd_mq;

    rc = rt_sem_init(&rsp_sem, "rsp", 0, RT_IPC_FLAG_FIFO);
    RT_ASSERT(rc == RT_EOK);
    g_rsp_sem = &rsp_sem;

    /* 线程（静态创建） */
    rc = rt_thread_init(&usb_tcb, "usb", usb_entry, RT_NULL,
                        usb_stack, sizeof(usb_stack), PRIO_USB, 20);
    RT_ASSERT(rc == RT_EOK);
    rt_thread_startup(&usb_tcb);

    rc = rt_thread_init(&f2mc_tcb, "f2mc", f2mc_entry, RT_NULL,
                        f2mc_stack, sizeof(f2mc_stack), PRIO_F2MC, 20);
    RT_ASSERT(rc == RT_EOK);
    rt_thread_startup(&f2mc_tcb);

    LOGI("tasks up: usb/f2mc\n");
    RT_UNUSED(rc);   /* RT_ASSERT 在未开 RT_DEBUG 时为空，避免告警 */
}

/**
 * @brief RT-Thread 启动序列（Nano 精简模型；rtconfig.h 未定义
 * RT_USING_USER_MAIN，内核不自带）
 *
 * board 层函数（rt_hw_board_init/SysTick_Handler）以 CubeMX pack 生成的
 * Middlewares/.../bsp/_template/cubemx_config/board.c 为准。
 * main.c USER CODE 2 中调用，不返回。
 */
int rtthread_startup(void)
{
    rt_hw_interrupt_disable();

    /* board level initialization（CubeMX pack board.c：重配 SysTick 1 kHz） */
    rt_hw_board_init();

    /* 模板 board.c 的 SysTick_Handler 只走 rt_tick_increase，
     * HAL 时基经 tick hook 补齐（不动 CubeMX 生成文件） */
    rt_tick_sethook(HAL_IncTick);

    /* show RT-Thread version */
    rt_show_version();

    /* timer system initialization */
    rt_system_timer_init();

    /* scheduler system initialization */
    rt_system_scheduler_init();

    /* create application threads */
    rt_application_init();

    /* timer thread initialization（RT_USING_TIMER_SOFT 未定义，空操作） */
    rt_system_timer_thread_init();

    /* idle thread initialization */
    rt_thread_idle_init();

    /* start scheduler */
    rt_system_scheduler_start();

    /* never reach here */
    return 0;
}
