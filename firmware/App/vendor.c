/**
 * @file vendor.c
 * @brief L1 vendor 命令封装解析（通信协议约定 §2）
 *
 * DAP.c 对 0x80~0x9F 的命令统一调用 DAP_ProcessVendorCommand（__WEAK），
 * 此处提供强符号实现：解包 L1 帧 → cmd.c 分发 → 打包响应。
 */
#include "vendor.h"
#include "cmd.h"

uint32_t DAP_ProcessVendorCommand(const uint8_t *request, uint8_t *response)
{
    uint8_t  cmd;
    uint16_t len, rsp_len = 0U;
    uint8_t  status;

    if (*request != ID_DAP_Vendor0)
    {
        /* 0x81~0x9F 未定义：按帧格式回 BAD_PARAM */
        response[0] = ID_DAP_Vendor0;
        response[1] = L1_ST_BAD_PARAM;
        return ((1U << 16) | 2U);
    }

    cmd = request[1];
    len = (uint16_t)request[2] | ((uint16_t)request[3] << 8);

    if (len > L1_MAX_PAYLOAD)
    {
        response[0] = ID_DAP_Vendor0;
        response[1] = L1_ST_BAD_PARAM;
        return ((4U << 16) | 2U);
    }

    status = cmd_execute(cmd, request + 4, len, response + 2, &rsp_len);

    response[0] = ID_DAP_Vendor0;
    response[1] = status;

    return (((uint32_t)(4U + len) << 16) | (uint32_t)(2U + rsp_len));
}
