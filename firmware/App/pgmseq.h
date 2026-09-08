/**
 * @file pgmseq.h
 * @brief 进入编程模式电气时序（固件使用说明 §3.3）
 *
 * 完整 ENTER_PGM = 本模块电气时序 + handshake + clock_mod（后两者在 new8fx.c）。
 */
#ifndef PGMSEQ_H
#define PGMSEQ_H

#include <stdint.h>

/**
 * @brief  进模式电气时序：等断电 → 上电前拉低 DBG → 等 VCC 上升/稳定 →
 *         保持 1.2s → 释放 DBG
 * @return L1 状态码（vendor.h L1_ST_*）；失败时已把 PB14 恢复为输入
 * @note   在 f2mc_task 上下文运行，可阻塞（最长 ~22s）；可被 ABORT(0x12) 中止
 */
uint8_t pgmseq_enter(void);

/**
 * @brief  复位运行（RESET_RUN，无 RST 引脚用电源开关实现）：
 *         DBG 拉低主动放电（等 VCC<0.5V 并保持 300ms，保证 POR 深度）→
 *         释放 DBG → 上电
 * @return L1 状态码；可被 ABORT(0x12) 中止
 * @note   大 die 目标自然放电慢，浅掉电不触发 POR——不能省放电等待
 */
uint8_t pgmseq_power_cycle_run(void);

/** @name 复位能力（RESET_RUN 响应 DATA[0] 上报给上位机）
 *  本板（F2MC-LINK v1.1）无 RST 引脚 → 恒为“模拟（断电+上电）”；
 *  未来带 RST 引脚的硬件改 PGMSEQ_RST_CAP 并在 pgmseq_power_cycle_run
 *  中改走引脚复位。 @{ */
#define PGMSEQ_RST_CAP_NATIVE       0x00U   /**< 原生：复位引脚直接实现 */
#define PGMSEQ_RST_CAP_SIMULATED    0x01U   /**< 模拟：断电+上电（兼容模式） */
#define PGMSEQ_RST_CAP              PGMSEQ_RST_CAP_SIMULATED
/** @} */

#endif /* PGMSEQ_H */
