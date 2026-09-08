/**
 * @file cmd.h
 * @brief L1 命令分发 + 编程器状态机（通信协议约定 §3.1/§3.2）
 *
 * 状态机：
 * @code
 *   IDLE →(ENTER_PGM)→ SYNCED →(ERASE)→ ERASED →(FLASH_INIT)→ RW_MODE
 *                    SYNCED →(FLASH_INIT)→ RW_MODE（仅校验/读取路径，不擦除）
 *   RW_MODE →(WRITE_x/READ_x/CR_TRIM_WRITE/ERASE)→ RW_MODE
 *   RW_MODE →(QUIT)→ SYNCED；任何状态 →(RESET_RUN/DISCONNECT)→ IDLE
 * @endcode
 */
#ifndef __CMD_H__
#define __CMD_H__

#include <stdint.h>

/** @brief 编程器状态（GET_STATE 返回值，通信协议约定 §3.2） */
typedef enum
{
    PROG_ST_IDLE   = 0,   /**< 空闲（等待 ENTER_PGM） */
    PROG_ST_SYNCED = 1,   /**< 已握手/时钟修改（可擦除/解锁/直接 FLASH_INIT） */
    PROG_ST_ERASED = 2,   /**< 已擦除（可 FLASH_INIT） */
    PROG_ST_RW     = 3,   /**< 读写模式（500K，可写/读/模式内擦除/QUIT） */
} prog_state_t;

/** @name 固件版本（PING 响应，与 DAP_config.h 的 Product FW Ver 同步）
 * @{ */
#define FW_VER_MAJOR    0U
#define FW_VER_MINOR    2U
#define FW_VER_PATCH    0U
/** @} */

/**
 * @brief  执行一条 L1 命令（含 LED 状态钩子与 ABORT 标志清除）
 * @param  cmd      命令字（vendor.h L1_CMD_*）
 * @param  payload  请求载荷（≤ L1_MAX_PAYLOAD）
 * @param  len      载荷长度
 * @param  rsp      响应数据缓冲（≥ L1_MAX_PAYLOAD+2）
 * @param  rsp_len  输出：响应数据长度
 * @return L1 状态码（vendor.h L1_ST_*）
 */
uint8_t cmd_execute(uint8_t cmd, const uint8_t *payload, uint16_t len,
                    uint8_t *rsp, uint16_t *rsp_len);

/** @brief  读取状态机当前状态 */
prog_state_t cmd_get_state(void);
/** @brief  设置状态机状态（引擎执行结果反馈） */
void         cmd_set_state(prog_state_t st);
/** @brief  读取最近一次非 OK 错误码（GET_STATE 返回） */
uint8_t      cmd_get_last_error(void);

/**
 * @name ABORT(0x12) 中止标志
 * dapif 在 USB ISR 收到 ABORT 包时置位（带外生效，不等 f2mc_task 分发）；
 * 长操作（pgmseq 等待/handshake/wire_recv）轮询退出。
 * 每条命令分发入口（cmd_execute）清除一次。
 * @{ */
void cmd_request_abort(void);   /**< 置位（ISR 安全） */
int  cmd_abort_pending(void);   /**< 查询：1=有未消耗的中止请求 */
void cmd_clear_abort(void);     /**< 清除（cmd_execute 入口调用） */
/** @} */

#endif /* __CMD_H__ */
