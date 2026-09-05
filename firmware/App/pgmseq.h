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

#endif /* PGMSEQ_H */
