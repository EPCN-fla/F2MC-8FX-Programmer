/**
 * @file rtt_log.h
 * @brief 极简 SEGGER RTT 日志（上行通道 0）
 *
 * 自实现，不含 SEGGER 版权代码；兼容 J-Link RTT Viewer / OpenOCD 的 RTT 协议。
 * 模式 = SKIP（缓冲满则丢弃），任何上下文（含 ISR）都可安全调用、不阻塞。
 * 查看方式：Test/rtt.bat + Test/rtt_dump.py。
 */
#ifndef __RTT_LOG_H__
#define __RTT_LOG_H__

/** @brief 初始化控制块（"SEGGER RTT" ID + 上行缓冲），rt_application_init 调用 */
void rtt_log_init(void);
/** @brief 格式化输出（vsnprintf 到栈缓冲后写入；满则丢弃） */
void rtt_log_printf(const char *fmt, ...);
/** @brief 原始写入（len 字节；满则丢弃） */
void rtt_log_write(const char *s, unsigned len);

/** @name 分级日志宏
 * @{ */
#define LOGI(...)  rtt_log_printf("[I] " __VA_ARGS__)   /**< 信息 */
#define LOGW(...)  rtt_log_printf("[W] " __VA_ARGS__)   /**< 警告 */
#define LOGE(...)  rtt_log_printf("[E] " __VA_ARGS__)   /**< 错误 */
/** @} */

#endif /* __RTT_LOG_H__ */
