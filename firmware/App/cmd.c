/**
 * @file cmd.c
 * @brief L1 命令分发 + 编程器状态机（通信协议约定 §3.1/§3.2）
 *
 * 状态机：IDLE →(ENTER_PGM)→ SYNCED →(ERASE)→ ERASED →(FLASH_INIT)→ RW_MODE
 *         SYNCED →(FLASH_INIT)→ RW_MODE（仅校验/读取路径，不擦除）
 *         RW_MODE →(WRITE_x/READ_x/CR_TRIM_WRITE/ERASE)→ RW_MODE
 *         RW_MODE →(QUIT)→ SYNCED；任何状态 →(RESET_RUN/DISCONNECT)→ IDLE
 *
 * WRITE 三段：WRITE_BEGIN(Len≤512) → WRITE_DATA(≤56B/包 顺序追加) → WRITE_COMMIT；
 * READ  两段：READ_BEGIN(Len≤1024，立即执行 L2 读入缓冲) → READ_DATA(≤56B/包 取回)。
 */
#include "cmd.h"
#include "vendor.h"
#include "new8fx.h"
#include "pgmseq.h"
#include "pwr.h"
#include "wire.h"
#include "led.h"
#include "rtt_log.h"
#include <string.h>

#define PING_RSP_STR    "F2MC-LINK"                  /**< PING 产品串 */
#define PING_RSP_STRLEN (sizeof(PING_RSP_STR) - 1U) /**< 9，不含 NUL */

#define WR_BUF_SIZE     512U    /**< 通信协议约定 §3.1：WRITE Len ≤ 512 B */
#define RD_BUF_SIZE     1024U   /**< 通信协议约定 §3.1：READ  Len ≤ 1024 B */
#define SECURE_ADDR     0xFFFCU /**< 安全锁地址（固件使用说明 §2.9） */

static prog_state_t g_state = PROG_ST_IDLE;   /**< 状态机 */
static uint8_t      g_last_error = L1_ST_OK;  /**< 最近非 OK 错误码 */
static volatile uint8_t s_abort_req = 0U;     /**< ABORT(0x12) 中止标志 */

void cmd_request_abort(void) { s_abort_req = 1U; }
int  cmd_abort_pending(void) { return (s_abort_req != 0U) ? 1 : 0; }
void cmd_clear_abort(void)   { s_abort_req = 0U; }

/** @name WRITE/READ 缓冲与进度
 * @{ */
static uint8_t  s_wr_buf[WR_BUF_SIZE];
static uint16_t s_wr_addr, s_wr_len, s_wr_off;
static uint8_t  s_rd_buf[RD_BUF_SIZE];
static uint16_t s_rd_len, s_rd_off;
/** @} */

prog_state_t cmd_get_state(void)          { return g_state; }
void         cmd_set_state(prog_state_t st) { g_state = st; }
uint8_t      cmd_get_last_error(void)     { return g_last_error; }

/** @brief 记录非 OK 错误码并原样返回（set-and-return 惯用法） */
static uint8_t set_err(uint8_t err)
{
    if (err != L1_ST_OK)
        g_last_error = err;
    return err;
}

/** @brief 大端 16 位解码（L2 地址/长度为大端，固件使用说明 §2.1） */
static uint16_t be16(const uint8_t *p)
{
    return (uint16_t)(((uint16_t)p[0] << 8) | p[1]);
}

/**
 * @brief 判定引擎类命令（L2 操作）：执行期 LED 2Hz（led.h）
 * @return 1=引擎命令；0=即时命令（PING/GET_STATE/SET_POWER/RESET_RUN/DISCONNECT/ABORT）
 */
static int is_engine_cmd(uint8_t cmd)
{
    switch (cmd)
    {
    case L1_CMD_ERASE:
    case L1_CMD_FLASH_INIT:
    case L1_CMD_WRITE_BEGIN:
    case L1_CMD_WRITE_DATA:
    case L1_CMD_WRITE_COMMIT:
    case L1_CMD_READ_BEGIN:
    case L1_CMD_READ_DATA:
    case L1_CMD_CR_TRIM_WRITE:
    case L1_CMD_QUIT:
    case L1_CMD_WRITE_SECURE:
    case L1_CMD_SEND_BREAK:
        return 1;
    default:
        return 0;
    }
}

/**
 * @brief 命令分发主体（不含钩子；钩子见 cmd_execute）
 * @copydetails cmd_execute
 */
static uint8_t cmd_execute_inner(uint8_t cmd, const uint8_t *payload,
                                 uint16_t len, uint8_t *rsp, uint16_t *rsp_len)
{
    *rsp_len = 0U;

    switch (cmd)
    {
    case L1_CMD_PING:
        /* "F2MC-LINK" + FW 版本 3B（docs/通信协议约定.md §3.1），任何状态可用 */
        for (uint8_t i = 0U; i < PING_RSP_STRLEN; i++)
            rsp[i] = (uint8_t)PING_RSP_STR[i];
        rsp[9]  = FW_VER_MAJOR;
        rsp[10] = FW_VER_MINOR;
        rsp[11] = FW_VER_PATCH;
        *rsp_len = 12U;
        return L1_ST_OK;

    case L1_CMD_GET_STATE:
        rsp[0] = (uint8_t)g_state;
        rsp[1] = g_last_error;
        *rsp_len = 2U;
        return L1_ST_OK;

    case L1_CMD_DISCONNECT:
        /* 上位机断开通知（通信协议约定 §3.1）：LED 熄灭 + 状态机复位 IDLE。
         * 目标侧状态不变；重连后从 ENTER_PGM 重新开始。DA 型号匹配复位到
         * 默认（避免上个会话的型号数据误用；重连后上位机重新 SET_CHIP）。 */
        g_state = PROG_ST_IDLE;
        new8fx_da_clear();
        led_notify_host_disconnect();
        return L1_ST_OK;

    case L1_CMD_ABORT:
        /* 中止标志已由 dapif 带外置位（长操作轮询退出）；
         * 走到这里说明无在执行操作或操作已退出，直接 OK。 */
        return L1_ST_OK;

    case L1_CMD_SET_POWER:
        /* F2MC-LINK v1.1 电源开关（EL817+SS8050，PA8=PWR_EN）：payload[0] 0=断电/1=上电。
         * 上电带 3s 上升确认，过载/未接自动关断回 PWR_FAULT；
         * 断电时目标已失电，状态机复位 IDLE（通信协议约定 §3.2）。 */
        if (len < 1U)
            return L1_ST_BAD_PARAM;
        {
            uint8_t prc = pwr_switch(payload[0] != 0U);
            if (prc == L1_ST_OK && payload[0] == 0U)
                g_state = PROG_ST_IDLE;
            return prc;
        }

    case L1_CMD_RESET_RUN:
        /* 无 RST 引脚（PA1 已舍弃）：用目标电源开关复位运行——断电→主动放电
         *（DBG 拉低经 2.2k 泄放，等 VCC<0.5V 保证 POR 深度）→上电。
         * ⚠ 不可只断电几百 ms：大 die 目标（F698K）放电慢，浅掉电不触发
         * POR，目标根本没复位（boot/DA 态继续跑，用户程序不运行）。
         * 响应 DATA[0] = 复位能力：0=原生（复位引脚）/ 1=模拟（断电+上电，
         * 兼容模式）——本板恒为模拟；更旧固件恒回 UNSUPPORTED（上位机
         * 自行 set_power 断电上电兜底）。 */
        g_state = PROG_ST_IDLE;
        {
            uint8_t st = pgmseq_power_cycle_run();
            if (st == L1_ST_OK)
            {
                rsp[0] = PGMSEQ_RST_CAP;
                *rsp_len = 1U;
            }
            return set_err(st);
        }

    case L1_CMD_SEND_BREAK:
        /* 通信恢复手段（固件使用说明 §2.8），任何状态可用 */
        wire_send_break();
        return L1_ST_OK;

    /* ---- 型号下发（任何状态可用的配置命令，不入引擎 LED 快闪） ---- */

    case L1_CMD_SET_CHIP:
        /* [型号名 ASCII ≤24B]（如 "MB95F698K"）：固件按系列匹配内嵌 DA
         *（new8fx_set_chip，docs/DA 结构解析.md）；FLASH_INIT 用匹配结果 */
        if ((len == 0U) || (len > 24U))
            return L1_ST_BAD_PARAM;
        new8fx_set_chip(payload, len);
        return L1_ST_OK;

    /* ---- 编程流程 ---- */

    case L1_CMD_ENTER_PGM:
        /* 任何状态可进：pgmseq 内含完整断电→放电→上电循环（电气上即完整重进），
         * 无需上位机先 RESET_RUN/QUIT 归位——带电重进路径因此不再依赖
         * QUIT 帧（DA 退出）与多余电源循环，规避"保持编程模式后再烧录卡死"。 */
        {
            uint8_t st = new8fx_enter_pgm();
            /* 握手成功即 SYNCED——安全锁目标（clock_mod 回 0xFD）也允许
             * 进入 SYNCED 做整片擦除解锁（固件使用说明 §2.9 流程） */
            if ((st == L1_ST_OK) || (st == L1_ST_SECURITY_LOCKED))
                g_state = PROG_ST_SYNCED;
            return set_err(st);
        }

    case L1_CMD_ERASE:
        if (len != 2U)
            return set_err(L1_ST_BAD_PARAM);
        if (g_state == PROG_ST_SYNCED)
        {
            /* pre-init 擦除（62500，固件使用说明 §2.4） */
            uint8_t st = new8fx_erase_preinit(be16(payload));
            if (st == L1_ST_OK)
                g_state = PROG_ST_ERASED;
            return set_err(st);
        }
        if (g_state == PROG_ST_RW)
            /* 读写模式内擦除（500K，固件使用说明 §2.4）；状态保持 RW_MODE——
             * 烧录完成后上位机可直接再擦（通信协议约定 §3.2） */
            return set_err(new8fx_erase_post(be16(payload)));
        return set_err(L1_ST_STATE_ERROR);

    case L1_CMD_FLASH_INIT:
        /* SYNCED 也允许：仅校验/读取流程不能先擦除（会毁数据），
         * L2 上 DA 加载不依赖先擦除（Spec 7.5 无此前提，通信协议约定 §3.2） */
        if ((g_state != PROG_ST_ERASED) && (g_state != PROG_ST_SYNCED))
            return set_err(L1_ST_STATE_ERROR);
        /* payload [XX, YY]：MB95630H = 0x02/0x7C，已固化在 INIT_DA_BIN 内 */
        if (len == 2U && ((payload[0] != 0x02U) || (payload[1] != 0x7CU)))
            LOGW("flash_init: XX/YY=%02x/%02x (expect 02/7C for MB95630H)\n",
                 payload[0], payload[1]);
        {
            uint8_t st = new8fx_flash_init();
            if (st == L1_ST_OK)
                g_state = PROG_ST_RW;
            return set_err(st);
        }

    /* ---- 写三段 ---- */

    case L1_CMD_WRITE_BEGIN:
        if (g_state != PROG_ST_RW)
            return set_err(L1_ST_STATE_ERROR);
        if (len != 4U)
            return set_err(L1_ST_BAD_PARAM);
        s_wr_len = be16(payload + 2);
        if ((s_wr_len == 0U) || (s_wr_len > WR_BUF_SIZE))
            return set_err(L1_ST_BAD_PARAM);
        s_wr_addr = be16(payload);
        s_wr_off = 0U;
        return L1_ST_OK;

    case L1_CMD_WRITE_DATA:
        if (g_state != PROG_ST_RW)
            return set_err(L1_ST_STATE_ERROR);
        /* 必须紧跟 WRITE_BEGIN 且未达 Len（通信协议约定 §3.2） */
        if ((s_wr_len == 0U) || (s_wr_off >= s_wr_len))
            return set_err(L1_ST_STATE_ERROR);
        if ((len == 0U) || ((uint32_t)s_wr_off + len > s_wr_len))
            return set_err(L1_ST_BAD_PARAM);
        memcpy(s_wr_buf + s_wr_off, payload, len);
        s_wr_off = (uint16_t)(s_wr_off + len);
        return L1_ST_OK;

    case L1_CMD_WRITE_COMMIT:
        if (g_state != PROG_ST_RW)
            return set_err(L1_ST_STATE_ERROR);
        if ((s_wr_len == 0U) || (s_wr_off != s_wr_len))
            return set_err(L1_ST_STATE_ERROR);   /* 数据未收满 */
        {
            uint8_t st = new8fx_write_mem(s_wr_addr, s_wr_buf, s_wr_len);
            s_wr_len = s_wr_off = 0U;
            return set_err(st);
        }

    /* ---- 读两段 ---- */

    case L1_CMD_READ_BEGIN:
        if (g_state != PROG_ST_RW)
            return set_err(L1_ST_STATE_ERROR);
        if (len != 4U)
            return set_err(L1_ST_BAD_PARAM);
        {
            uint16_t rlen = be16(payload + 2);
            uint8_t st;
            if ((rlen == 0U) || (rlen > RD_BUF_SIZE))
                return set_err(L1_ST_BAD_PARAM);
            st = new8fx_read_mem(be16(payload), s_rd_buf, rlen);
            if (st != L1_ST_OK)
                return set_err(st);
            s_rd_len = rlen;
            s_rd_off = 0U;
        }
        return L1_ST_OK;

    case L1_CMD_READ_DATA:
        if (g_state != PROG_ST_RW)
            return set_err(L1_ST_STATE_ERROR);
        /* 必须紧跟 READ_BEGIN 且未取满（通信协议约定 §3.2） */
        if ((s_rd_len == 0U) || (s_rd_off >= s_rd_len))
            return set_err(L1_ST_STATE_ERROR);
        {
            uint16_t n = (uint16_t)(s_rd_len - s_rd_off);
            if (n > L1_MAX_PAYLOAD)
                n = L1_MAX_PAYLOAD;
            memcpy(rsp, s_rd_buf + s_rd_off, n);
            s_rd_off = (uint16_t)(s_rd_off + n);
            if (s_rd_off >= s_rd_len)
                s_rd_len = 0U;                 /* 取满，允许下一 READ_BEGIN */
            *rsp_len = n;
        }
        return L1_ST_OK;

    /* ---- 其他 RW 命令 ---- */

    case L1_CMD_CR_TRIM_WRITE:
        if (g_state != PROG_ST_RW)
            return set_err(L1_ST_STATE_ERROR);
        if (len != 3U)
            return set_err(L1_ST_BAD_PARAM);
        return set_err(new8fx_cr_trim_write(be16(payload), payload[2]));

    case L1_CMD_QUIT:
        if (g_state != PROG_ST_RW)
            return set_err(L1_ST_STATE_ERROR);
        {
            uint8_t st = new8fx_quit();
            if (st == L1_ST_OK)
                g_state = PROG_ST_SYNCED;
            return set_err(st);
        }

    case L1_CMD_WRITE_SECURE:
        if (g_state != PROG_ST_RW)
            return set_err(L1_ST_STATE_ERROR);
        {
            /* 向 0xFFFC 写 0x01（固件使用说明 §2.9）；断电或 QUIT 后生效 */
            uint8_t one = 0x01U;
            return set_err(new8fx_write_mem(SECURE_ADDR, &one, 1U));
        }

    default:
        return set_err(L1_ST_BAD_PARAM);
    }
}

uint8_t cmd_execute(uint8_t cmd, const uint8_t *payload, uint16_t len,
                    uint8_t *rsp, uint16_t *rsp_len)
{
    uint8_t rc;

    /* LED 状态钩子（led.h）：任何 vendor 命令=上位机在线；
     * ENTER_PGM 执行期 0.5Hz（等目标上电/握手），引擎命令执行期 2Hz，
     * 命令结束回 SOLID。TIM1 硬件 PWM 驱动，模式切换即时生效。 */
    cmd_clear_abort();   /* 消耗本次 ABORT；运行中的命令不受影响（已过入口） */
    led_notify_host_cmd();
    if (cmd == L1_CMD_ENTER_PGM)
        led_set_mode(LED_MODE_SLOW);
    else if (is_engine_cmd(cmd) != 0)
        led_set_mode(LED_MODE_FAST);

    rc = cmd_execute_inner(cmd, payload, len, rsp, rsp_len);

    if ((cmd == L1_CMD_ENTER_PGM) || (is_engine_cmd(cmd) != 0))
        led_set_mode(LED_MODE_SOLID);
    return rc;
}
