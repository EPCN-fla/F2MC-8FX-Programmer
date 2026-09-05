/**
 * @file prog_tasks.h
 * @brief RT-Thread 任务胶水层对外接口
 *
 * 线程模型（固件使用说明 §3.2）：
 *   usb_task  (768 B 栈，prio 3)  等响应信号量 → dapif_tx_response 回 USB
 *   f2mc_task (2048 B 栈，prio 4) 收 g_cmd_mq → DAP_ExecuteCommand →
 *                                 vendor/cmd 分发 → new8fx 引擎（长操作）
 *   （无 blink 线程：LED 由 TIM1_CH2 硬件 PWM 驱动，见 led.c）
 *
 * 通信：DAP 请求包 ISR→rt_mq（g_cmd_mq）；响应 f2mc→usb 用 rt_sem + g_rsp_buf。
 */
#ifndef __PROG_TASKS_H__
#define __PROG_TASKS_H__

#include <rtthread.h>

/** @brief DAP 请求消息：1 B 接口号 + 1 B 长度 + 64 B DAP 包（v1/v2 统一 64 B） */
#define CMD_MSG_SIZE   66
/** @brief 请求消息队列深度 */
#define CMD_MSG_MAX    4
/** @brief 响应缓冲：1 B 接口号 + 1 B 长度 + 64 B DAP 包 */
#define RSP_BUF_SIZE   66

extern rt_mq_t  g_cmd_mq;   /**< dapif(ISR) → f2mc_task：DAP 请求包 */
extern rt_sem_t g_rsp_sem;  /**< f2mc_task → usb_task：响应就绪 */
extern rt_uint8_t g_rsp_buf[RSP_BUF_SIZE]; /**< 响应包（接口号+长度+数据） */

/** @brief 应用线程/IPC 创建（rtthread_startup 调用） */
void rt_application_init(void);

#endif /* __PROG_TASKS_H__ */
