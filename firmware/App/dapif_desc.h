/**
 * @file dapif_desc.h
 * @brief USB 设备/字符串/BOS/MS OS 1.0&2.0 描述符（T2 双接口 + WinUSB 免驱）
 */
#ifndef __DAPIF_DESC_H__
#define __DAPIF_DESC_H__

#include "usbd_def.h"

/** @name MS OS vendor 请求码（嵌在 0xEE 'MSFT100' 串与 BOS 平台能力中）
 * @{ */
#define DAP_MSOS1_VENDOR_CODE   0xA0U   /**< MS OS 1.0 Extended Compat ID */
#define DAP_MSOS2_VENDOR_CODE   0xA1U   /**< MS OS 2.0 Descriptor Set */
/** @} */

/** @brief 设备描述符集（usb_device.c 注册，USBD_RegisterDescription） */
extern USBD_DescriptorsTypeDef DAP_FS_Desc;

/** @name 供 dapif.c 类驱动复用的描述符生成函数
 * @{ */
uint8_t *dap_str_to_utf16(const char *s, uint16_t *length);   /**< ASCII→UTF16LE 字符串描述符 */
uint8_t *dap_msft100_str(uint16_t *length);                   /**< 0xEE 'MSFT100' 串（MS OS 1.0 锚点） */
uint8_t *dap_msos1_compat_id(uint16_t *length);               /**< MS OS 1.0 Extended Compat ID（56B） */
uint8_t *dap_msos2_desc_set(uint16_t *length);                /**< MS OS 2.0 集（178B，WinUSB GUID） */
/** @} */

#endif /* __DAPIF_DESC_H__ */
