/**
 * @file led.h
 * @brief 状态指示 LED（PA9 = TIM1_CH2 硬件 PWM，不占线程/CPU）
 *
 * 四态：
 *   OFF   —— 未连接上位机（上电后从未收到 L1 vendor 命令，或收到 DISCONNECT，
 *            或 SWD 侧 DAP_Disconnect）
 *   SOLID —— 上位机已连接（收到过任意 vendor 命令，或 SWD 侧 DAP_Connect），闲置
 *   SLOW  —— 0.5Hz：ENTER_PGM 执行中（等待目标上电/握手）
 *   FAST  —— 5Hz：已进入编程模式，引擎命令执行中
 *            （ERASE/FLASH_INIT/WRITE/READ/CR_TRIM/QUIT/SECURE/BREAK）
 *
 * SWD 侧联动：DAP_Connect→SOLID / DAP_Disconnect→OFF（DAP_config.h 的
 * LED_CONNECTED_OUT）。SWD 传输活动灯（LED_RUNNING_OUT）不联动。
 * 注意 F2MC 与 SWD 会话共用一灯，交替操作时后到者覆盖灯效。
 *
 * 已知限制：STM32F1 USB 设备无 VBUS 感知，上位机直接拔线检测不到，
 * LED 保持 SOLID；正常断开走 DISCONNECT(0x11) 命令。
 */
#ifndef APP_LED_H
#define APP_LED_H

#include <stdint.h>

/** @brief LED 模式 */
typedef enum
{
    LED_MODE_OFF = 0,   /**< 熄灭：未连接 */
    LED_MODE_SOLID,     /**< 常亮：已连接闲置 */
    LED_MODE_SLOW,      /**< 0.5Hz：ENTER_PGM 等待进模式 */
    LED_MODE_FAST       /**< 2Hz：引擎命令执行中 */
} led_mode_t;

/** @brief 初始化 TIM1_CH2 PWM 时基 + PA9（默认 OFF），rt_application_init 调用 */
void       led_init(void);
/** @brief 切换模式（幂等；模式切换即时生效，无毛刺） */
void       led_set_mode(led_mode_t m);
/** @brief 读取当前模式 */
led_mode_t led_get_mode(void);
/** @brief 任何 L1 vendor 命令到达时调用：标记上位机在线（OFF→SOLID） */
void       led_notify_host_cmd(void);
/** @brief DISCONNECT(0x11) 命令时调用：LED 熄灭 + 清除在线标记 */
void       led_notify_host_disconnect(void);

#endif /* APP_LED_H */
