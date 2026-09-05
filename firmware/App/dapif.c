/**
 * @file dapif.c
 * @brief CMSIS-DAP v1(HID) + v2(Bulk) 双接口复合 USB 类驱动
 *
 * 基于 ST USBD 内核（usbd_core/usbd_ctlreq/usbd_ioreq）之上自实现
 * USBD_ClassTypeDef，替代 CubeMX CustomHID 类：
 *   - GetFSConfigDescriptor：Config + If0(HID) + If1(vendor Bulk) 共 64 B；
 *   - Setup：HID 类请求（最小集）/ HID 报告描述符 / MS OS 1.0 & 2.0 vendor 请求；
 *   - DataOut：EP1/EP2 OUT 完成 → 投递 g_cmd_mq（ISR 安全）→ f2mc_task 执行；
 *   - 响应由 usb_task 调 dapif_tx_response() 经对应 EP IN 发回。
 *
 * @warning 免驱关键：MS OS 描述符请求走 EP0 vendor 控制传输，在此 Setup
 * 中应答（通信协议约定 §1 免驱注意点；漏掉则 Windows 下 v2 需 Zadig）。
 */
#include "dapif.h"
#include "dapif_desc.h"
#include "prog_tasks.h"
#include "vendor.h"
#include "cmd.h"
#include "usb_device.h"
#include "usbd_ctlreq.h"

extern USBD_HandleTypeDef hUsbDeviceFS;   /**< USB_DEVICE/App/usb_device.c */

/** @brief HID 报告描述符（CMSIS-DAP v1：64 B In + 64 B Out，厂商自定义页） */
static const uint8_t dap_hid_report_desc[] = {
    0x06, 0x00, 0xFF,   /* Usage Page (Vendor Defined 0xFF00) */
    0x09, 0x01,         /* Usage (1) */
    0xA1, 0x01,         /* Collection (Application) */
    0x15, 0x00,         /*   Logical Minimum (0) */
    0x26, 0xFF, 0x00,   /*   Logical Maximum (255) */
    0x75, 0x08,         /*   Report Size (8) */
    0x95, 0x40,         /*   Report Count (64) */
    0x09, 0x01,         /*   Usage (1) */
    0x81, 0x02,         /*   Input (Data,Var,Abs) */
    0x95, 0x40,         /*   Report Count (64) */
    0x09, 0x01,         /*   Usage (1) */
    0x91, 0x02,         /*   Output (Data,Var,Abs) */
    0xC0,               /* End Collection */
};
#define DAP_HID_REPORT_DESC_SIZE  ((uint16_t)sizeof(dap_hid_report_desc))  /**< 27 */

/** @brief HID 描述符（嵌入配置描述符，同时可响应 GET_DESCRIPTOR 0x21） */
#define DAP_HID_DESC                                                \
    0x09, 0x21,                     /* bLength, HID */              \
    0x11, 0x01,                     /* bcdHID 1.11 */               \
    0x00,                           /* bCountryCode */              \
    0x01,                           /* bNumDescriptors */           \
    0x22,                           /* bDescriptorType: Report */   \
    (uint8_t)(DAP_HID_REPORT_DESC_SIZE & 0xFF),                    \
    (uint8_t)(DAP_HID_REPORT_DESC_SIZE >> 8)

/**
 * @name 配置描述符（9 + If0 32 + If1 23 = 64 B）
 * @warning 必须在 RAM（非 const）：ST 库的 CONFIG/OTHER_SPEED 分支会回写
 * pbuf[1] 打类型字节（usbd_ctlreq.c），flash 里的 const 会直接 fault。
 * @{ */
#define DAP_CFG_DESC_SIZE   64U
#define DAP_STR_IF_V1       6U    /**< iInterface：CMSIS-DAP v1 */
#define DAP_STR_IF_V2       7U    /**< iInterface：CMSIS-DAP v2 */
/** @} */

static uint8_t dap_cfg_desc[] = {
    /* Configuration */
    0x09, 0x02,
    (uint8_t)(DAP_CFG_DESC_SIZE & 0xFF), (uint8_t)(DAP_CFG_DESC_SIZE >> 8),
    0x02,                           /* bNumInterfaces */
    0x01,                           /* bConfigurationValue */
    0x00,                           /* iConfiguration */
    0x80,                           /* bmAttributes: Bus Powered */
    0x32,                           /* bMaxPower: 100 mA */

    /* Interface 0: CMSIS-DAP v1 (HID) */
    0x09, 0x04,
    0x00,                           /* bInterfaceNumber */
    0x00,                           /* bAlternateSetting */
    0x02,                           /* bNumEndpoints */
    0x03, 0x00, 0x00,               /* HID, no boot protocol */
    DAP_STR_IF_V1,                  /* iInterface = "CMSIS-DAP v1" */

    DAP_HID_DESC,                   /* HID descriptor (9 B) */

    /* EP1 IN (Interrupt) */
    0x07, 0x05, DAP_HID_EPIN_ADDR, 0x03,
    DAP_EP_SIZE, 0x00,
    0x01,                           /* bInterval 1 ms */
    /* EP1 OUT (Interrupt) */
    0x07, 0x05, DAP_HID_EPOUT_ADDR, 0x03,
    DAP_EP_SIZE, 0x00,
    0x01,

    /* Interface 1: CMSIS-DAP v2 (vendor Bulk, WinUSB) */
    0x09, 0x04,
    0x01,                           /* bInterfaceNumber */
    0x00,
    0x02,
    0xFF, 0x00, 0x00,               /* Vendor Specific */
    DAP_STR_IF_V2,                  /* iInterface = "CMSIS-DAP v2" */

    /* EP2 IN (Bulk) */
    0x07, 0x05, DAP_BULK_EPIN_ADDR, 0x02,
    DAP_EP_SIZE, 0x00,
    0x00,
    /* EP2 OUT (Bulk) */
    0x07, 0x05, DAP_BULK_EPOUT_ADDR, 0x02,
    DAP_EP_SIZE, 0x00,
    0x00,
};

/** @name OUT 端点接收缓冲（USBD 内核直接写入）
 * @{ */
static uint8_t hid_out_buf[DAP_EP_SIZE];
static uint8_t bulk_out_buf[DAP_EP_SIZE];
/** @} */

/* ================= 类回调 ================= */

/** @brief SET_CONFIGURATION：打开 4 个端点并两臂 OUT 接收 */
static uint8_t dap_init(USBD_HandleTypeDef *pdev, uint8_t cfgidx)
{
    (void)cfgidx;

    USBD_LL_OpenEP(pdev, DAP_HID_EPIN_ADDR, USBD_EP_TYPE_INTR, DAP_EP_SIZE);
    pdev->ep_in[DAP_HID_EPIN_ADDR & 0xFU].is_used = 1U;
    USBD_LL_OpenEP(pdev, DAP_HID_EPOUT_ADDR, USBD_EP_TYPE_INTR, DAP_EP_SIZE);
    pdev->ep_out[DAP_HID_EPOUT_ADDR & 0xFU].is_used = 1U;

    USBD_LL_OpenEP(pdev, DAP_BULK_EPIN_ADDR, USBD_EP_TYPE_BULK, DAP_EP_SIZE);
    pdev->ep_in[DAP_BULK_EPIN_ADDR & 0xFU].is_used = 1U;
    USBD_LL_OpenEP(pdev, DAP_BULK_EPOUT_ADDR, USBD_EP_TYPE_BULK, DAP_EP_SIZE);
    pdev->ep_out[DAP_BULK_EPOUT_ADDR & 0xFU].is_used = 1U;

    /* 两臂 OUT 接收 */
    USBD_LL_PrepareReceive(pdev, DAP_HID_EPOUT_ADDR, hid_out_buf, DAP_EP_SIZE);
    USBD_LL_PrepareReceive(pdev, DAP_BULK_EPOUT_ADDR, bulk_out_buf, DAP_EP_SIZE);

    return (uint8_t)USBD_OK;
}

/** @brief 去初始化：关闭全部端点 */
static uint8_t dap_deinit(USBD_HandleTypeDef *pdev, uint8_t cfgidx)
{
    (void)cfgidx;

    USBD_LL_CloseEP(pdev, DAP_HID_EPIN_ADDR);
    USBD_LL_CloseEP(pdev, DAP_HID_EPOUT_ADDR);
    USBD_LL_CloseEP(pdev, DAP_BULK_EPIN_ADDR);
    USBD_LL_CloseEP(pdev, DAP_BULK_EPOUT_ADDR);
    pdev->ep_in[DAP_HID_EPIN_ADDR & 0xFU].is_used = 0U;
    pdev->ep_out[DAP_HID_EPOUT_ADDR & 0xFU].is_used = 0U;
    pdev->ep_in[DAP_BULK_EPIN_ADDR & 0xFU].is_used = 0U;
    pdev->ep_out[DAP_BULK_EPOUT_ADDR & 0xFU].is_used = 0U;

    return (uint8_t)USBD_OK;
}

/**
 * @brief EP0 请求处理
 *  - 标准请求(接口)：GET_DESCRIPTOR 0x21(HID) / 0x22(Report)
 *  - 类请求(接口)：HID SET_IDLE/GET_IDLE/SET_PROTOCOL/GET_PROTOCOL（最小集）
 *  - 厂商请求(设备)：MS OS 1.0(0xA0) / MS OS 2.0(0xA1) 描述符 ← 免驱关键
 */
static uint8_t dap_setup(USBD_HandleTypeDef *pdev, USBD_SetupReqTypedef *req)
{
    uint8_t  *pbuf;
    uint16_t len;

    switch (req->bmRequest & USB_REQ_TYPE_MASK)
    {
    case USB_REQ_TYPE_STANDARD:
        /* 接口收件人：HID 类描述符 */
        if ((req->wValue >> 8) == 0x21U)            /* HID Descriptor */
        {
            /* 配置描述符中 HID 描述符段（偏移 18 起 9 B） */
            USBD_CtlSendData(pdev, (uint8_t *)&dap_cfg_desc[18], 9U);
        }
        else if ((req->wValue >> 8) == 0x22U)       /* Report Descriptor */
        {
            USBD_CtlSendData(pdev, (uint8_t *)dap_hid_report_desc,
                             DAP_HID_REPORT_DESC_SIZE);
        }
        else
        {
            USBD_CtlError(pdev, req);
        }
        break;

    case USB_REQ_TYPE_CLASS:
        switch (req->bRequest)
        {
        case 0x0A:  /* HID_SET_IDLE */
        case 0x0B:  /* HID_SET_PROTOCOL */
            USBD_CtlSendStatus(pdev);
            break;
        case 0x02:  /* HID_GET_IDLE */
        case 0x03:  /* HID_GET_PROTOCOL */
            pbuf = bulk_out_buf;    /* 借用缓冲，返回 0 */
            pbuf[0] = 0U;
            USBD_CtlSendData(pdev, pbuf, 1U);
            break;
        default:    /* GET_REPORT/SET_REPORT 等：STALL（hidapi 走中断 EP） */
            USBD_CtlError(pdev, req);
            break;
        }
        break;

    case USB_REQ_TYPE_VENDOR:
        if ((req->bRequest == DAP_MSOS1_VENDOR_CODE) && (req->wIndex == 0x0004U))
        {
            /* MS OS 1.0 Extended Compat ID Feature Descriptor */
            len = 0U;
            pbuf = dap_msos1_compat_id(&len);
            USBD_CtlSendData(pdev, pbuf, len);
        }
        else if ((req->bRequest == DAP_MSOS2_VENDOR_CODE) && (req->wIndex == 0x0007U))
        {
            /* MS OS 2.0 Descriptor Set */
            len = 0U;
            pbuf = dap_msos2_desc_set(&len);
            USBD_CtlSendData(pdev, pbuf, len);
        }
        else
        {
            USBD_CtlError(pdev, req);
        }
        break;

    default:
        USBD_CtlError(pdev, req);
        break;
    }

    return (uint8_t)USBD_OK;
}

/** @brief EP IN 发送完成（响应为一包发完，无需链式处理） */
static uint8_t dap_data_in(USBD_HandleTypeDef *pdev, uint8_t epnum)
{
    (void)pdev;
    (void)epnum;
    return (uint8_t)USBD_OK;
}

/**
 * @brief ABORT(0x12) 带外侦听（ISR 上下文）
 * @note f2mc_task 正忙时本消息在 mq 排队，但中止标志立即生效——长操作
 * （pgmseq 等待/handshake/wire_recv）轮询退出。ABORT 消息本身仍入 mq，
 * 按序回 OK（通信协议约定 §3.1）。
 */
static void peek_abort(const uint8_t *dap_pkt, uint32_t len)
{
    if ((len >= 2U) && (dap_pkt[0] == ID_DAP_Vendor0) &&
        (dap_pkt[1] == L1_CMD_ABORT))
        cmd_request_abort();
}

/**
 * @brief EP OUT 接收完成（ISR 上下文）
 * @note 数据已入 hid/bulk_out_buf → 组消息 {iface, len, payload} 投 g_cmd_mq，
 * 立即重臂接收。f2mc_task 取消息执行（通信协议约定 §1：单命令执行期间不响应新命令，
 * 由上位机串行发命令保证）。
 */
static uint8_t dap_data_out(USBD_HandleTypeDef *pdev, uint8_t epnum)
{
    uint8_t msg[CMD_MSG_SIZE];
    uint32_t rxlen;

    rxlen = USBD_LL_GetRxDataSize(pdev, epnum);
    if (rxlen > DAP_EP_SIZE)
        rxlen = DAP_EP_SIZE;

    if (epnum == DAP_HID_EPOUT_ADDR)
    {
        msg[0] = DAP_IF_V1;
        msg[1] = (uint8_t)rxlen;
        for (uint32_t i = 0U; i < rxlen; i++)
            msg[2U + i] = hid_out_buf[i];
        peek_abort(msg + 2, rxlen);
        (void)rt_mq_send(g_cmd_mq, msg, CMD_MSG_SIZE);
        USBD_LL_PrepareReceive(pdev, DAP_HID_EPOUT_ADDR, hid_out_buf, DAP_EP_SIZE);
    }
    else if (epnum == DAP_BULK_EPOUT_ADDR)
    {
        msg[0] = DAP_IF_V2;
        msg[1] = (uint8_t)rxlen;
        for (uint32_t i = 0U; i < rxlen; i++)
            msg[2U + i] = bulk_out_buf[i];
        peek_abort(msg + 2, rxlen);
        (void)rt_mq_send(g_cmd_mq, msg, CMD_MSG_SIZE);
        USBD_LL_PrepareReceive(pdev, DAP_BULK_EPOUT_ADDR, bulk_out_buf, DAP_EP_SIZE);
    }

    return (uint8_t)USBD_OK;
}

/** @brief 配置描述符回调（F1 仅 FS，HS 回调同返 FS 描述符） */
static uint8_t *dap_get_cfg_desc(uint16_t *length)
{
    *length = (uint16_t)sizeof(dap_cfg_desc);
    return dap_cfg_desc;
}

/**
 * @brief Other-Speed 配置描述符
 * @note 独立 RAM 副本（lib 会回写 pbuf[1]=7，若与 FS 配置共用会把类型字节
 * 改坏）。dev_speed 强制 FULL 后本回调不会被执行，仅作纵深防御。
 */
static uint8_t *dap_get_other_speed_desc(uint16_t *length)
{
    static uint8_t other_speed_buf[DAP_CFG_DESC_SIZE];
    uint16_t i;

    for (i = 0U; i < DAP_CFG_DESC_SIZE; i++)
        other_speed_buf[i] = dap_cfg_desc[i];
    other_speed_buf[1] = 0x07U;   /* OTHER_SPEED_CONFIGURATION */

    *length = DAP_CFG_DESC_SIZE;
    return other_speed_buf;
}

/** @brief Device Qualifier 描述符（10 B，RAM：lib 同样可能回写） */
static uint8_t *dap_get_qualifier_desc(uint16_t *length)
{
    static uint8_t qualifier[] = {
        0x0A, 0x06,             /* bLength, DEVICE_QUALIFIER */
        0x00, 0x02,             /* bcdUSB 2.00 */
        0x00, 0x00, 0x00,       /* class/sub/proto */
        0x40,                   /* bMaxPacketSize0 */
        0x01,                   /* bNumConfigurations */
        0x00,                   /* bReserved */
    };
    *length = (uint16_t)sizeof(qualifier);
    return qualifier;
}

/** @brief 用户字符串（接口字符串 6/7、MSFT100 0xEE） */
static uint8_t *dap_get_usr_str(USBD_HandleTypeDef *pdev, uint8_t index, uint16_t *length)
{
    (void)pdev;

    switch (index)
    {
    case DAP_STR_IF_V1:
        return dap_str_to_utf16("CMSIS-DAP v1", length);
    case DAP_STR_IF_V2:
        return dap_str_to_utf16("CMSIS-DAP v2", length);
    case 0xEEU:     /* MS OS 1.0 签名串 */
        return dap_msft100_str(length);
    default:
        *length = 0U;
        return (uint8_t *)dap_hid_report_desc;  /* 不应到达；返回无害指针 */
    }
}

/** @brief USBD 类驱动表（usb_device.c 的 USBD_RegisterClass 注册） */
USBD_ClassTypeDef USBD_DAP = {
    dap_init,
    dap_deinit,
    dap_setup,
    NULL,           /* EP0_TxSent */
    NULL,           /* EP0_RxReady */
    dap_data_in,
    dap_data_out,
    NULL,           /* SOF */
    NULL,           /* IsoINIncomplete */
    NULL,           /* IsoOUTIncomplete */
    dap_get_cfg_desc,   /* GetHSConfigDescriptor（F1 不会用到） */
    dap_get_cfg_desc,   /* GetFSConfigDescriptor */
    dap_get_other_speed_desc,   /* GetOtherSpeedConfigDescriptor */
    dap_get_qualifier_desc,     /* GetDeviceQualifierDescriptor */
#if (USBD_SUPPORT_USER_STRING_DESC == 1U)
    dap_get_usr_str,
#endif
};

void dapif_tx_response(uint8_t iface, const uint8_t *data, uint16_t len)
{
    static uint8_t tx_buf[DAP_EP_SIZE];

    if (iface == DAP_IF_V1)
    {
        /* HID 报告恒 64 B（不足补零） */
        uint16_t i;
        for (i = 0U; i < DAP_EP_SIZE; i++)
            tx_buf[i] = (i < len) ? data[i] : 0U;
        USBD_LL_Transmit(&hUsbDeviceFS, DAP_HID_EPIN_ADDR, tx_buf, DAP_EP_SIZE);
    }
    else
    {
        if (len > DAP_EP_SIZE)
            len = DAP_EP_SIZE;
        USBD_LL_Transmit(&hUsbDeviceFS, DAP_BULK_EPIN_ADDR, (uint8_t *)data, len);
    }
}
