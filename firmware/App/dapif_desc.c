/**
 * @file dapif_desc.c
 * @brief USB 设备/字符串/BOS/MS OS 1.0 & 2.0 描述符
 *
 * 免驱设计（通信协议约定 §1）：
 *   - MS OS 2.0（BOS "MSFT20" 平台能力 + vendor 0xA1 取描述符集）
 *     → Win8.1+/Win10/11 自动给 Interface 1 绑 WinUSB；
 *   - MS OS 1.0（0xEE "MSFT100" 串 + vendor 0xA0 取 Compat ID）
 *     → Win7 等老系统同理（v2 不可用也无所谓，v1/HID 保底）；
 *   - DeviceInterfaceGUIDs 注册属性使用 CMSIS-DAP v2 标准 GUID
 *     {CDB3B5AD-293B-4663-AA36-1AAE46463776}（OpenOCD/probe-rs 按此识别）。
 *
 * VID/PID：0x1209 = pid.codes 社区 VID，0xF2A1 为开发期自取 PID。
 * @warning 产品化前需在 pid.codes 注册正式 PID。
 */

#include "dapif_desc.h"
#include "dapif.h"

#define DAP_VID         0x1209U   /**< pid.codes 社区 VID */
#define DAP_PID         0xF2A1U   /**< 开发期自取 PID（产品化前需注册） */

/* ================= 设备描述符（18 B） ================= */
static const uint8_t dap_device_desc[] = {
    0x12, 0x01,                 /* bLength, DEVICE */
    0x10, 0x02,                 /* bcdUSB 2.10（BOS 需要 ≥2.01） */
    0x00, 0x00, 0x00,           /* bDeviceClass/Sub/Protocol: per-interface */
    0x40,                       /* bMaxPacketSize0 = 64 */
    (uint8_t)(DAP_VID & 0xFF), (uint8_t)(DAP_VID >> 8),
    (uint8_t)(DAP_PID & 0xFF), (uint8_t)(DAP_PID >> 8),
    0x00, 0x01,                 /* bcdDevice 1.00 */
    0x01,                       /* iManufacturer */
    0x02,                       /* iProduct */
    0x03,                       /* iSerialNumber */
    0x01,                       /* bNumConfigurations */
};

/* ================= 字符串 ================= */
static const uint8_t dap_langid[] = { 0x04, 0x03, 0x09, 0x04 };   /**< en-US */

static uint8_t str_buf[64];     /**< UTF-16 字符串转换缓冲（最长 28 字符） */

/**
 * @brief ASCII 字符串 → USB UTF-16LE 描述符（复用 str_buf，单次控制传输期有效）
 * @param s      输入 ASCII 串（最长取 28 字符）
 * @param length 输出描述符长度（字节）
 * @return str_buf
 */
uint8_t *dap_str_to_utf16(const char *s, uint16_t *length)
{
    uint16_t n = 0U;

    while ((s[n] != '\0') && (n < 28U))
    {
        str_buf[2U + n * 2U] = (uint8_t)s[n];
        str_buf[2U + n * 2U + 1U] = 0U;
        n++;
    }
    str_buf[0] = (uint8_t)(2U + n * 2U);
    str_buf[1] = 0x03U;
    *length = (uint16_t)(2U + n * 2U);
    return str_buf;
}

/**
 * @brief MS OS 1.0 签名串（索引 0xEE）："MSFT100" + vendor 码 + 0x00
 * @note Windows 枚举期通过 GET_DESCRIPTOR(STRING, 0xEE) 索取
 */
uint8_t *dap_msft100_str(uint16_t *length)
{
    static const uint8_t msft100[] = {
        0x12, 0x03,
        'M', 0, 'S', 0, 'F', 0, 'T', 0, '1', 0, '0', 0, '0', 0,
        DAP_MSOS1_VENDOR_CODE, 0x00,
    };
    *length = (uint16_t)sizeof(msft100);
    return (uint8_t *)msft100;
}

/**
 * @brief MS OS 1.0 Extended Compat ID Feature Descriptor（56 B）
 * @note vendor 请求 bRequest=0xA0、wIndex=0x0004；绑定 Interface 1 = WinUSB
 */
uint8_t *dap_msos1_compat_id(uint16_t *length)
{
    static const uint8_t desc[] = {
        /* Header */
        0x38, 0x00, 0x00, 0x00, /* dwLength = 56 */
        0x00, 0x01,             /* bcdVersion 1.00 */
        0x04, 0x00,             /* wIndex = 4 (Extended Compat ID) */
        0x01,                   /* bCount = 1 */
        0, 0, 0, 0, 0, 0, 0,    /* RESERVED */
        /* Function（绑定 Interface 1 = CMSIS-DAP v2） */
        0x01,                   /* bFirstInterfaceNumber = 1 */
        0x01,                   /* bReserved = 1 */
        'W', 'I', 'N', 'U', 'S', 'B', 0, 0,   /* compatibleID = WINUSB */
        0, 0, 0, 0, 0, 0, 0, 0,                 /* subCompatibleID */
        0, 0, 0, 0, 0, 0,                       /* reserved */
    };
    *length = (uint16_t)sizeof(desc);
    return (uint8_t *)desc;
}

/* ================= BOS（含 MS OS 2.0 平台能力） ================= */
#define MSOS2_SET_TOTAL_LEN     178U    /**< 与下方描述符集总长相符 */

static const uint8_t dap_bos_desc[] = {
    /* BOS header */
    0x05, 0x0F,
    (uint8_t)(5 + 7 + 28), 0x00,        /* wTotalLength = 40 */
    0x02,                               /* bNumDeviceCaps */

    /* USB 2.0 Extension */
    0x07, 0x10, 0x02,
    0x00, 0x00, 0x00, 0x00,             /* bmAttributes */

    /* MS OS 2.0 Platform Capability（UUID DF60DDD8-8945-C74C-9CD2-659D9E648A9F） */
    0x1C, 0x10, 0x05, 0x00,             /* bLength=28, Device Capability, Platform */
    0xDF, 0x60, 0xDD, 0xD8, 0x89, 0x45, 0xC7, 0x4C,
    0x9C, 0xD2, 0x65, 0x9D, 0x9E, 0x64, 0x8A, 0x9F,
    0x00, 0x00, 0x03, 0x06,             /* dwWindowsVersion = 0x06030000 (Win8.1) */
    (uint8_t)(MSOS2_SET_TOTAL_LEN & 0xFF), (uint8_t)(MSOS2_SET_TOTAL_LEN >> 8),
    DAP_MSOS2_VENDOR_CODE,              /* bMS_VendorCode */
    0x00,                               /* bAltEnumCode */
};

/**
 * @brief MS OS 2.0 Descriptor Set（178 B）
 * @details 布局：Set Header(10) + Cfg Subset(8) + Func Subset(8) + Compat ID(20)
 * + Registry Property(132) = 178 B。
 * Registry Property: "DeviceInterfaceGUIDs" = {CDB3B5AD-...}（CMSIS-DAP v2 标准 GUID）。
 * @note vendor 请求 bRequest=0xA1、wIndex=0x0007
 */
uint8_t *dap_msos2_desc_set(uint16_t *length)
{
    static const uint8_t desc[] = {
        /* Set Header (10) */
        0x0A, 0x00,                     /* wLength */
        0x00, 0x00,                     /* wDescriptorType = MS OS 2.0 Set Header */
        0x00, 0x00, 0x03, 0x06,         /* dwWindowsVersion */
        0xB2, 0x00,                     /* wTotalLength = 178 */

        /* Configuration Subset Header (8) */
        0x08, 0x00,
        0x01, 0x00,                     /* wDescriptorType = Config Subset */
        0x00,                           /* bConfigurationValue = 0 */
        0x00,                           /* bReserved */
        0xA8, 0x00,                     /* wTotalLength = 168 */

        /* Function Subset Header (8) */
        0x08, 0x00,
        0x02, 0x00,                     /* wDescriptorType = Function Subset */
        0x01,                           /* bFirstInterface = 1 (v2) */
        0x00,
        0xA0, 0x00,                     /* wSubsetLength = 160 */

        /* Compatible ID (20) */
        0x14, 0x00,
        0x03, 0x00,                     /* wDescriptorType = Compatible ID */
        'W', 'I', 'N', 'U', 'S', 'B', 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

        /* Registry Property (132)：DeviceInterfaceGUIDs（REG_MULTI_SZ） */
        0x84, 0x00,                     /* wLength = 132 */
        0x04, 0x00,                     /* wDescriptorType = Registry Property */
        0x07, 0x00,                     /* wPropertyDataType = REG_MULTI_SZ */
        0x2A, 0x00,                     /* wPropertyNameLength = 42 */
        /* "DeviceInterfaceGUIDs" UTF-16LE（含尾零，21 字符 = 42 B） */
        'D',0,'e',0,'v',0,'i',0,'c',0,'e',0,'I',0,'n',0,'t',0,'e',0,
        'r',0,'f',0,'a',0,'c',0,'e',0,'G',0,'U',0,'I',0,'D',0,'s',0, 0,0,
        0x50, 0x00,                     /* wPropertyDataLength = 80 */
        /* "{CDB3B5AD-293B-4663-AA36-1AAE46463776}" UTF-16LE（38 字符 76 B
         *  + 串尾零 2 B + REG_MULTI_SZ 附加零 2 B = 80 B） */
        '{',0,'C',0,'D',0,'B',0,'3',0,'B',0,'5',0,'A',0,'D',0,'-',0,
        '2',0,'9',0,'3',0,'B',0,'-',0,'4',0,'6',0,'6',0,'3',0,'-',0,
        'A',0,'A',0,'3',0,'6',0,'-',0,'1',0,'A',0,'A',0,'E',0,'4',0,
        '6',0,'4',0,'6',0,'3',0,'7',0,'7',0,'6',0,'}',0, 0,0, 0,0,
    };
    /* 编译期长度自检：10+8+8+20+132 = 178 */
    _Static_assert(sizeof(desc) == MSOS2_SET_TOTAL_LEN, "MS OS 2.0 set length");

    *length = (uint16_t)sizeof(desc);
    return (uint8_t *)desc;
}

/* ================= 描述符回调 ================= */
static uint8_t *get_dev_desc(USBD_SpeedTypeDef speed, uint16_t *length)
{
    (void)speed;
    *length = (uint16_t)sizeof(dap_device_desc);
    return (uint8_t *)dap_device_desc;
}

static uint8_t *get_langid(USBD_SpeedTypeDef speed, uint16_t *length)
{
    (void)speed;
    *length = (uint16_t)sizeof(dap_langid);
    return (uint8_t *)dap_langid;
}

static uint8_t *get_mfr(USBD_SpeedTypeDef speed, uint16_t *length)
{
    (void)speed;
    return dap_str_to_utf16("f2mc", length);
}

static uint8_t *get_product(USBD_SpeedTypeDef speed, uint16_t *length)
{
    (void)speed;
    return dap_str_to_utf16("F2MC-LINK CMSIS-DAP", length);
}

static uint8_t *get_serial(USBD_SpeedTypeDef speed, uint16_t *length)
{
    /* TODO: 从 STM32 UID96 生成；当前固定值 */
    (void)speed;
    return dap_str_to_utf16("F2MC0001", length);
}

#if (USBD_LPM_ENABLED == 1U)
static uint8_t *get_bos(USBD_SpeedTypeDef speed, uint16_t *length)
{
    (void)speed;
    *length = (uint16_t)sizeof(dap_bos_desc);
    return (uint8_t *)dap_bos_desc;
}
#endif

/** @brief USBD 描述符表（usb_device.c 的 USBD_RegisterDescriptors 注册） */
USBD_DescriptorsTypeDef DAP_FS_Desc = {
    get_dev_desc,
    get_langid,
    get_mfr,
    get_product,
    get_serial,
    NULL,       /* GetConfigurationStrDescriptor（配置无字符串，iConfiguration=0） */
    NULL,       /* GetInterfaceStrDescriptor（接口串走 class GetUsrStrDescriptor 6/7） */
#if (USBD_LPM_ENABLED == 1U)
    get_bos,
#endif
};
