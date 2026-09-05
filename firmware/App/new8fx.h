/**
 * @file new8fx.h
 * @brief New8FX 串行编程协议（L2）引擎（固件使用说明 §2）
 *
 * 全部函数返回 L1 状态码（vendor.h L1_ST_*），在 f2mc_task 上下文阻塞运行。
 * 波特率：enter_pgm/erase_preinit 阶段 62500；flash_init 成功后内部切 500K，
 * quit 时归位 62500（目标退模式后回到 bootloader 的 62500 世界）。
 */
#ifndef NEW8FX_H
#define NEW8FX_H

#include <stdint.h>

/**
 * @brief  进入编程模式完整序列（固件使用说明 §2.2：电气时序 + 握手 + 时钟修改）
 * @return L1_ST_OK / L1_ST_SECURITY_LOCKED（握手成功但加锁）/ 错误码
 * @note   返回 SECURITY_LOCKED 也算进入 SYNCED（可整片擦除解锁）
 */
uint8_t new8fx_enter_pgm(void);
/** @brief 预 init 擦除（固件使用说明 §2.4，62500；整片 Addr=0x0000，f3 单发等 ≤60s） */
uint8_t new8fx_erase_preinit(uint16_t addr);
/** @brief 下载 INIT_DA_BIN（固件使用说明 §2.5，YM02 141B 版本）并切 500K */
uint8_t new8fx_flash_init(void);
/** @brief 读写模式内擦除（固件使用说明 §2.4，500K；整片 Addr=0x0000） */
uint8_t new8fx_erase_post(uint16_t addr);
/** @brief 写闪存（固件使用说明 §2.6；150µs/字节节拍，写后等 10ms DA 忙期） */
uint8_t new8fx_write_mem(uint16_t addr, const uint8_t *data, uint16_t len);
/** @brief 读闪存（固件使用说明 §2.6；首字节窗 100ms，失败 break+重试 ×3） */
uint8_t new8fx_read_mem(uint16_t addr, uint8_t *buf, uint16_t len);
/** @brief CR 校准写（固件使用说明 §2.7；⚠ 真机 DA 不实现，恒 ACK_ERROR，保留不用） */
uint8_t new8fx_cr_trim_write(uint16_t addr, uint8_t data);
/** @brief 退出读写模式（固件使用说明 §2.7；归位 62500） */
uint8_t new8fx_quit(void);

#endif /* NEW8FX_H */
