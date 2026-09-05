/**
 * @file vendor.h
 * @brief L1 vendor 命令封装接口（协议见 通信协议约定 §2/§3）
 *
 * 帧格式（CMSIS-DAP vendor 命令 ID_DAP_Vendor0 通道）：
 *   请求:  [0x80] [CMD] [LEN_L] [LEN_H] [PAYLOAD(LEN ≤ 56 B)]
 *   响应:  [0x80] [STATUS] [DATA...]
 * 多字节整数一律小端。
 */
#ifndef __VENDOR_H__
#define __VENDOR_H__

#include <stdint.h>

/** @brief DAP vendor 命令起始 ID（ARM 预留 0x80~0x9F） */
#define ID_DAP_Vendor0          0x80U

/** @name L1 命令字（通信协议约定 §3.1）
 * @{ */
#define L1_CMD_PING             0x01U   /**< 连通性/版本 */
#define L1_CMD_SET_POWER        0x02U   /**< 目标电源控制（无 MOSFET，恒 UNSUPPORTED） */
#define L1_CMD_ENTER_PGM        0x03U   /**< 进入编程模式（电气时序+握手+时钟修改） */
#define L1_CMD_ERASE            0x04U   /**< 擦除（SYNCED=pre-init 式；RW=读写模式内） */
#define L1_CMD_FLASH_INIT       0x05U   /**< 下载 INIT_DA_BIN 并切 500K */
#define L1_CMD_WRITE_BEGIN      0x06U   /**< 写开始（Len ≤ 512B） */
#define L1_CMD_WRITE_DATA       0x07U   /**< 写数据（≤56B/包） */
#define L1_CMD_WRITE_COMMIT     0x08U   /**< 写提交（执行 L2 写） */
#define L1_CMD_READ_BEGIN       0x09U   /**< 读开始（Len ≤ 1024B，立即执行 L2 读） */
#define L1_CMD_READ_DATA        0x0AU   /**< 读数据（≤56B/包） */
#define L1_CMD_CR_TRIM_WRITE    0x0BU   /**< CR 校准写（真机 DA 不实现，回 ACK_ERROR） */
#define L1_CMD_QUIT             0x0CU   /**< 退出读写模式 */
#define L1_CMD_RESET_RUN        0x0DU   /**< 复位运行（无 RST 引脚，恒 UNSUPPORTED+回 IDLE） */
#define L1_CMD_GET_STATE        0x0EU   /**< 查询状态机/最后错误 */
#define L1_CMD_WRITE_SECURE     0x0FU   /**< 写安全锁（0xFFFC=0x01） */
#define L1_CMD_SEND_BREAK       0x10U   /**< 发送 UART Break（通信恢复） */
#define L1_CMD_DISCONNECT       0x11U   /**< 上位机断开通知（LED 灭+回 IDLE） */
#define L1_CMD_ABORT            0x12U   /**< 强制中止当前长操作（带外生效） */
/** @} */

/** @name L1 状态码（通信协议约定 §3.3）
 * @{ */
#define L1_ST_OK                0x00U   /**< 成功 */
#define L1_ST_TIMEOUT           0x01U   /**< 目标应答超时（含擦除 60s 等待） */
#define L1_ST_SECURITY_LOCKED   0x02U   /**< 收到 0xFD：目标已加安全锁 */
#define L1_ST_ACK_ERROR         0x03U   /**< 收到非预期 ACK 值 */
#define L1_ST_TARGET_CHK_ERROR  0x04U   /**< 目标侧校验和错误 */
#define L1_ST_BAD_PARAM         0x05U   /**< 请求参数非法 */
#define L1_ST_UART_ERROR        0x06U   /**< 帧错误/底层错误 */
#define L1_ST_STATE_ERROR       0x07U   /**< 当前状态不允许该命令 */
#define L1_ST_FRAME_CRC_ERROR   0x08U   /**< L1 帧 CRC 错误（保留，未用） */
#define L1_ST_BUSY              0x09U   /**< 上一条命令未执行完（保留） */
#define L1_ST_UNSUPPORTED       0x0AU   /**< 硬件/固件不支持 */
#define L1_ST_ABORTED           0x0BU   /**< 操作被 ABORT(0x12) 中止 */
#define L1_ST_PWR_FAULT         0x0CU   /**< 目标电源异常（过载/短路/未接），已自动关断 */
/** @} */

/** @brief 单包应用载荷上限（v1/v2 两通道取最小余量，通信协议约定 §1） */
#define L1_MAX_PAYLOAD          56U

/**
 * @brief  CMSIS-DAP vendor 命令入口（强符号覆盖 DAP.c 的 __WEAK 实现）
 * @param  request   请求帧（含首字节 0x80）
 * @param  response  响应帧缓冲（含首字节 0x80）
 * @return (请求消费字节数 << 16) | 响应字节数（DAP_ExecuteCommand 约定）
 */
uint32_t DAP_ProcessVendorCommand(const uint8_t *request, uint8_t *response);

#endif /* __VENDOR_H__ */
