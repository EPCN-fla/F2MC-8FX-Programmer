/**
 * @file dapif.h
 * @brief CMSIS-DAP v1(HID) + v2(Bulk) 双接口 USB 复合类驱动
 *
 * 同一 USB 配置中提供两个接口（通信协议约定 §1）：
 *   Interface 0 = CMSIS-DAP v1（HID，EP1 IN/OUT 中断 64 B）
 *   Interface 1 = CMSIS-DAP v2（vendor Bulk，EP2 IN/OUT 64 B）
 * 应用层统一单包载荷 ≤ 56 B（通信协议约定 §2）。
 */
#ifndef __DAPIF_H__
#define __DAPIF_H__

#include "usbd_core.h"

/** @name 接口号（g_cmd_mq/g_rsp_buf 消息首字节）
 * @{ */
#define DAP_IF_V1   0U    /**< CMSIS-DAP v1（HID） */
#define DAP_IF_V2   1U    /**< CMSIS-DAP v2（Bulk） */
/** @} */

/** @name 端点分配（EP0 + 4 = 5 ≤ F103 上限 8）
 * @{ */
#define DAP_HID_EPIN_ADDR    0x81U   /**< v1 HID 中断 IN */
#define DAP_HID_EPOUT_ADDR   0x01U   /**< v1 HID 中断 OUT */
#define DAP_BULK_EPIN_ADDR   0x82U   /**< v2 Bulk IN */
#define DAP_BULK_EPOUT_ADDR  0x02U   /**< v2 Bulk OUT */
#define DAP_EP_SIZE          64U     /**< 端点包大小 */
/** @} */

/** @brief USBD 类驱动实例（usb_device.c 注册） */
extern USBD_ClassTypeDef USBD_DAP;

/**
 * @brief  发送 DAP 响应（usb_task 上下文调用）
 * @param  iface  接口号（DAP_IF_V1/V2）
 * @param  data   响应数据
 * @param  len    长度（v1 始终补满 64 B 报告；v2 发实际长度）
 */
void dapif_tx_response(uint8_t iface, const uint8_t *data, uint16_t len);

#endif /* __DAPIF_H__ */
