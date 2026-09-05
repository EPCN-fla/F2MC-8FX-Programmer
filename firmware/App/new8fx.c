/**
 * @file new8fx.c
 * @brief New8FX L2 协议引擎（固件使用说明 §2，依据 Spec New8FX-Serial_PGM-V1.3.0 §7）
 *
 * 帧约定：基本命令帧 4 字节 [功能码/AddrH | Data/AddrL | 参数 | Checksum]，
 * Checksum = 前 3 字节之和低 8 位；随后目标回 1 字节 ACK（0x00 正常，0xFD 安全锁）。
 * 读写功能命令为 5 字节头 [AddrH AddrL LenH LenL 0xFF/0x00] + 数据流。
 *
 * @warning 时序纪律：每次 wire_send 后必须立即 wire_recv——EXTI 只在接收
 * 窗口内 arm，若 ACK 在我们 arm 之前到达会被漏采（读成 0xFF）。
 * @warning 重试策略（固件使用说明 §2.8）：单命令最多 3 次，重试前 SEND_BREAK；
 * SECURITY_LOCKED 不重试（擦除解锁是上位机流程）；擦除末帧不可重发。
 */
#include "new8fx.h"
#include "wire.h"
#include "pgmseq.h"
#include "vendor.h"
#include "cmd.h"
#include "rtt_log.h"
#include "f2mc_hw.h"
#include "drv_pin.h"
#include "main.h"
#include <rtthread.h>

/** @name L2 ACK 值
 * @{ */
#define L2_ACK_OK           0x00U   /**< 正常 */
#define L2_ACK_SECURITY     0xFDU   /**< 安全锁 */
#define L2_ACK_HANDSHAKE    0x51U   /**< 握手应答 */
/** @} */

/** @name 字节间间隔（µs）
 * @{ */
#define GAP_CLOCKMOD_US     100U    /**< 时钟修改：字节间隔 <250µs（Spec 7.3） */
#define GAP_ERASE_US        1000U   /**< 擦除系列：字节间隔 ≥1ms（Spec 7.4） */
#define GAP_NONE_US         0U      /**< FLASH_INIT @62500：连续发（ACK 流控） */
#define GAP_RW_US           150U    /**< RW 期 @500K：DA 每字节处理 ~50-66µs
                                     * （Spec 7.14），写忙时会丢字节（字节计数失配
                                     * 会把后续命令吃成数据），150µs 留 3x 余量 */
/** @} */

/** @name ACK 等待超时（ms）
 * @{ */
#define ACK_TO_NORMAL_MS    10U     /**< 常规帧 */
#define ACK_TO_HANDSHAKE_MS 20U     /**< 握手：T2 ≥6.6ms + 目标处理余量（10ms 会卡边） */
#define ACK_TO_CLOCKMOD_MS  50U     /**< 时钟修改：目标可能先完成时钟切换才回 ACK */
#define ACK_TO_ERASE_MS     60000U  /**< 擦除末帧：Spec 仅要求 >35s，实测 ~1.3s 即回 */
/** @} */

#define HANDSHAKE_MAX_TRY   200U    /**< Spec 7.1 给 5000；实测典型 2 次，200 次≈5s
                                     * 足够，失败路径 ENTER_PGM 全程 ~6.5s < 上位机 10s 超时 */
#define L2_RETRY_MAX        3U      /**< 固件使用说明 §2.8 */

/**
 * @brief INIT DA.BIN（141 字节）
 * @note 抓取自 YM02 编程器对 MB95630H 的实际上传（sniff_62500.txt）；
 * 与 Spec Table 7-1 的 198B 版本不同——该版本在真机上 RW 阶段行为异常
 * （DA 对所有读沉默），YM02 版本实测可用。XX=0x02（RW 500K）已就位。
 */
static const uint8_t INIT_DA_BIN[141] = {
    0xF1,0xD4,0x01,0x1D,0x05,0x7C,0x61,0x01,0x1F,0xE5,0x01,0x60,0x41,0xF3,0x40,0x70,
    0x40,0xE4,0x70,0x30,0x71,0x04,0x00,0xEA,0x85,0x26,0x02,0xE9,0x10,0xE9,0xE3,0xE9,
    0x4F,0xE9,0x48,0xE9,0x49,0x99,0xFF,0xFD,0x29,0x99,0x88,0xFD,0x4E,0x05,0x7C,0x64,
    0xCF,0x45,0x7C,0x98,0x00,0xFD,0x0F,0xD8,0xE4,0x00,0xD1,0x40,0xE4,0xFF,0xD4,0x93,
    0xE0,0xF2,0xEA,0x21,0x00,0xC3,0x9F,0x00,0xFD,0x05,0xDF,0xD8,0x21,0x00,0xC8,0x21,
    0x00,0xAB,0x05,0x7C,0x64,0xCF,0x45,0x7C,0xAE,0x0C,0x98,0x00,0xFD,0x0F,0xD8,0xE9,
    0xE2,0xE4,0x00,0xFA,0x40,0xE4,0xFF,0xE0,0x93,0xE0,0x21,0x00,0xEA,0x9F,0x00,0xFD,
    0x05,0xDF,0xD8,0x21,0x00,0xEF,0xA6,0x0C,0x21,0x00,0xAB,0x50,0x71,0x50,0xE3,0x51,
    0xC4,0x01,0x1D,0xE1,0x60,0x01,0x1F,0x45,0x7C,0x85,0x26,0x02,0x20,
};

/* ------------------------------------------------------------------ */
/* 基础原语                                                             */
/* ------------------------------------------------------------------ */

/** @brief wire_recv 返回码 → L1 状态码（多处共用） */
static uint8_t wire_rc_to_l1(int rc)
{
    if (rc == WIRE_E_ABORT)
        return L1_ST_ABORTED;
    return (rc == WIRE_E_FRAMING) ? L1_ST_UART_ERROR : L1_ST_TIMEOUT;
}

/**
 * @brief  发 4 字节命令帧 + 读 1 字节 ACK
 * @param  f4       帧（含校验和）
 * @param  gap_us   字节间间隔
 * @param  ack_to_ms ACK 等待超时
 * @return L1 状态码（OK / SECURITY_LOCKED / UART_ERROR / TIMEOUT / ABORTED / ACK_ERROR）
 */
static uint8_t cmd_frame(const uint8_t *f4, uint32_t gap_us, uint32_t ack_to_ms)
{
    uint8_t ack = 0U;
    int rc;

    (void)wire_send(f4, 4U, gap_us);
    rc = wire_recv(&ack, 1U, ack_to_ms);
    if (rc < 0)
        return wire_rc_to_l1(rc);
    if (ack == L2_ACK_OK)
        return L1_ST_OK;
    if (ack == L2_ACK_SECURITY)
        return L1_ST_SECURITY_LOCKED;
    LOGE("cmd %02x ack=%02x\n", f4[0], ack);
    return L1_ST_ACK_ERROR;
}

/** @brief L2 操作函数签名（with_retry 包装用） */
typedef uint8_t (*l2_op_t)(void *arg);

/**
 * @brief  重试包装：最多 L2_RETRY_MAX 次，重试前 SEND_BREAK
 * @note   SECURITY_LOCKED / ABORTED 不重试直接返回（解锁/中止是上位机决策）
 */
static uint8_t with_retry(l2_op_t op, void *arg, const char *name)
{
    uint8_t st = L1_ST_OK;
    uint8_t attempt;

    for (attempt = 0U; attempt < L2_RETRY_MAX; attempt++)
    {
        st = op(arg);
        if ((st == L1_ST_OK) || (st == L1_ST_SECURITY_LOCKED))
            return st;
        LOGW("%s: st=%02x, break+retry %u/%u\n", name, st,
             (unsigned)(attempt + 1U), (unsigned)L2_RETRY_MAX);
        wire_send_break();
    }
    return st;
}

/* ------------------------------------------------------------------ */
/* 握手 / 时钟修改（固件使用说明 §2.3）                                          */
/* ------------------------------------------------------------------ */

/**
 * @brief 握手（固件使用说明 §2.3）：0x55 →(≥3ms)→ 0xAA → 读 ACK 0x51，最多 200 次
 * @note 每次发送后立即 wire_recv（窗口 20ms 覆盖 Spec T2 ≥6.6ms 的 ACK 延迟）
 */
static uint8_t handshake(void)
{
    uint8_t b, ack = 0U;
    uint16_t try_cnt, no_rsp = 0U;
    int rc;

    for (try_cnt = 0U; try_cnt < HANDSHAKE_MAX_TRY; try_cnt++)
    {
        if (cmd_abort_pending() != 0)   /* ABORT(0x12)：握手重试可被中止 */
            return L1_ST_ABORTED;
        b = 0x55U;
        (void)wire_send(&b, 1U, GAP_NONE_US);
        rt_thread_mdelay(3);                    /* 0x55→0xAA 间隔 ≥3 ms */
        b = 0xAAU;
        (void)wire_send(&b, 1U, GAP_NONE_US);
        /* 立即 arm 接收；目标 ACK（≥6.6 ms 后）由 recv 超时覆盖。
         * 窗口 20ms：Spec T2 仅保证 ≥6.6ms，10ms 恰好卡边导致首试必超时。 */
        rc = wire_recv(&ack, 1U, ACK_TO_HANDSHAKE_MS);
        if ((rc > 0) && (ack == L2_ACK_HANDSHAKE))
        {
            if (try_cnt > 0U)
                LOGI("handshake ok after %u tries\n", (unsigned)(try_cnt + 1U));
            rt_thread_mdelay(2);                /* Spec 7.3：握手后 ≥1ms 再发 clock_mod
                                                 *（mdelay(1) 可能不足 1ms，用 2 保底） */
            return L1_ST_OK;
        }
        if (rc < 0)
        {
            no_rsp++;
            if (no_rsp == 20U)
                LOGW("handshake: no response x20, exti_hits=%lu\n",
                     (unsigned long)wire_exti_hit_count());
        }
        else
        {
            LOGE("handshake: bad ack %02x\n", ack);
        }
    }
    LOGE("handshake failed (%u tries)\n", (unsigned)HANDSHAKE_MAX_TRY);
    return L1_ST_TIMEOUT;
}

/**
 * @brief 时钟修改（固件使用说明 §2.3）：'00 00 07 07' + '03 00 D8 DB'（MB95630H）
 * @return L1_ST_OK / L1_ST_SECURITY_LOCKED（f2 回 0xFD）/ 错误码
 */
static uint8_t clock_mod(void)
{
    static const uint8_t f1[4] = {0x00, 0x00, 0x07, 0x07};
    static const uint8_t f2[4] = {0x03, 0x00, 0xD8, 0xDB};   /* MB95630H */
    uint8_t st;

    st = cmd_frame(f1, GAP_CLOCKMOD_US, ACK_TO_CLOCKMOD_MS);
    if (st != L1_ST_OK)
    {
        LOGW("clock_mod f1: st=%02x\n", st);
        return st;
    }
    st = cmd_frame(f2, GAP_CLOCKMOD_US, ACK_TO_CLOCKMOD_MS);   /* 0xFD → 安全锁 */
    if (st == L1_ST_OK)
        LOGI("clock_mod ok\n");
    else
        LOGW("clock_mod f2: st=%02x\n", st);
    return st;
}

/* ------------------------------------------------------------------ */
/* 各命令操作实体（with_retry 包装）                                      */
/* ------------------------------------------------------------------ */

/** @brief 预 init 擦除头两帧（固件使用说明 §2.4）：'00 00 0C 0C' + '05 00 60 65' */
static uint8_t op_erase_preinit_hdr(void *arg)
{
    static const uint8_t f1[4] = {0x00, 0x00, 0x0C, 0x0C};
    static const uint8_t f2[4] = {0x05, 0x00, 0x60, 0x65};
    uint8_t st;

    RT_UNUSED(arg);
    st = cmd_frame(f1, GAP_ERASE_US, ACK_TO_NORMAL_MS);
    if (st != L1_ST_OK) return st;
    return cmd_frame(f2, GAP_ERASE_US, ACK_TO_NORMAL_MS);
}

/**
 * @brief 擦除末帧（06 Addr Sum）：目标收到后开始物理擦除，ACK 要等很久
 * @warning 不可套用"超时→break→重发"：超时多半只是还没擦完，重发会撞上忙
 * 中的目标（实测：attempt1 启动了擦除，attempt2/3 的 f1 全部无响应）。
 * 故 f3 独立单发，超时后直接报错，由上位机决定重试。
 */
static uint8_t erase_fire(uint16_t addr)
{
    uint8_t f3[4] = {0x06, (uint8_t)(addr >> 8), (uint8_t)addr, 0U};
    rt_tick_t t0;
    uint8_t st;

    f3[3] = (uint8_t)(f3[0] + f3[1] + f3[2]);
    LOGI("erasing (addr=%04x)...\n", (unsigned)addr);
    t0 = rt_tick_get();
    st = cmd_frame(f3, GAP_ERASE_US, ACK_TO_ERASE_MS);
    LOGI("erase f3 done: st=%02x, %lu ms\n", st,
         (unsigned long)rt_tick_get() - (unsigned long)t0);
    return st;
}

/**
 * @brief FLASH_INIT 实体（固件使用说明 §2.5）：3 头帧 + 141 帧 DA.BIN + 尾帧，随后切 500K
 * @note 失败日志带 exti_delta/lvl 诊断：delta=0=DA 沉默；delta>0=RX 丢帧
 */
static uint8_t op_flash_init(void *arg)
{
    static const uint8_t f1[4] = {0x00, 0x00, 0x0C, 0x0C};
    static const uint8_t f2[4] = {0x05, 0x00, 0x60, 0x65};
    static const uint8_t f3[4] = {0x00, 0x00, 0x90, 0x90};
    static const uint8_t ft[4] = {0x0A, 0x00, 0x00, 0x0A};
    uint8_t st;
    uint16_t i;

    RT_UNUSED(arg);

    st = cmd_frame(f1, GAP_NONE_US, ACK_TO_NORMAL_MS);
    if (st != L1_ST_OK) return st;
    st = cmd_frame(f2, GAP_NONE_US, ACK_TO_NORMAL_MS);
    if (st != L1_ST_OK) return st;
    st = cmd_frame(f3, GAP_NONE_US, ACK_TO_NORMAL_MS);
    if (st != L1_ST_OK) return st;

    for (i = 0U; i < (uint16_t)sizeof(INIT_DA_BIN); i++)
    {
        uint8_t fr[4] = {0x03, 0x00, INIT_DA_BIN[i],
                         (uint8_t)(0x03 + 0x00 + INIT_DA_BIN[i])};
        rt_uint32_t ex0 = wire_exti_hit_count();   /* 诊断：失败时分辨 DA 沉默/RX 丢帧 */
        st = cmd_frame(fr, GAP_NONE_US, ACK_TO_NORMAL_MS);
        if (st != L1_ST_OK)
        {
            LOGE("flash_init: bin[%u]=%02x st=%02x exti_delta=%lu lvl=%d\n",
                 (unsigned)i, (unsigned)INIT_DA_BIN[i], (unsigned)st,
                 (unsigned long)(wire_exti_hit_count() - ex0),
                 (int)rt_pin_read(DBG_RX_PIN));
            return st;
        }
    }

    st = cmd_frame(ft, GAP_NONE_US, ACK_TO_NORMAL_MS);
    if (st != L1_ST_OK)
        return st;

    /* 尾帧后目标进入 RW 模式并切到 500K（BIN 内 XX=0x02）。
     * 留 10ms 让目标完成自身 UART 重配置。 */
    wire_set_baud(WIRE_BAUD_500K);
    rt_thread_mdelay(10);
    LOGI("flash_init ok, baud -> 500K\n");
    return L1_ST_OK;
}

/** @brief 读写模式内擦除（固件使用说明 §2.4，500K，5 字节帧 'AddrH AddrL 00 00 AA'） */
static uint8_t op_erase_post(void *arg)
{
    uint16_t addr = *(const uint16_t *)arg;
    uint8_t f[5] = {(uint8_t)(addr >> 8), (uint8_t)addr, 0x00, 0x00, 0xAA};
    uint8_t ack = 0U;
    int rc;

    (void)wire_send(f, 5U, GAP_RW_US);
    rc = wire_recv(&ack, 1U, ACK_TO_ERASE_MS);
    if (rc < 0)
        return wire_rc_to_l1(rc);
    if (ack == L2_ACK_OK)       return L1_ST_OK;
    if (ack == L2_ACK_SECURITY) return L1_ST_SECURITY_LOCKED;
    LOGE("erase_post ack=%02x\n", ack);
    return L1_ST_ACK_ERROR;
}

/** @brief CR 校准写（固件使用说明 §2.7，'AddrH AddrL 00 Data 55'；⚠ 真机 DA 不实现） */
static uint8_t op_cr_trim(void *arg)
{
    const uint8_t *p = (const uint8_t *)arg;   /* [addrH, addrL, data] */
    uint8_t f[5] = {p[0], p[1], 0x00, p[2], 0x55};
    uint8_t ack = 0U;
    int rc;

    (void)wire_send(f, 5U, GAP_RW_US);
    rc = wire_recv(&ack, 1U, ACK_TO_NORMAL_MS);
    if (rc < 0)
        return wire_rc_to_l1(rc);
    if (ack == L2_ACK_OK)       return L1_ST_OK;
    if (ack == L2_ACK_SECURITY) return L1_ST_SECURITY_LOCKED;
    LOGE("cr_trim ack=%02x\n", ack);
    return L1_ST_ACK_ERROR;
}

/* ------------------------------------------------------------------ */
/* 对外 API                                                              */
/* ------------------------------------------------------------------ */

uint8_t new8fx_enter_pgm(void)
{
    uint8_t st;

    /* QUIT/上次会话可能把 wire 停在 500K；目标重新上电后只说 62500，
     * 必须显式归位，否则握手 200 次重试死等 ~5 秒（实测）。 */
    wire_set_baud(WIRE_BAUD_62500);

    st = pgmseq_enter();          /* F3 电气时序（62500 已就位） */
    if (st != L1_ST_OK)
        return st;

    st = handshake();
    if (st != L1_ST_OK)
        return st;

    /* 0xFD（安全锁）由 clock_mod 帧2返回，透传给上位机；
     * 握手已成功，上位机可继续整片擦除解锁（固件使用说明 §2.9） */
    st = clock_mod();
    if (st == L1_ST_OK)
        LOGI("enter_pgm ok\n");
    return st;
}

uint8_t new8fx_erase_preinit(uint16_t addr)
{
    uint8_t st = with_retry(op_erase_preinit_hdr, RT_NULL, "erase_preinit hdr");
    if (st != L1_ST_OK)
        return st;
    return erase_fire(addr);
}

uint8_t new8fx_flash_init(void)
{
    /* YM02 嗅探证实：erase 后直接进 flash_init，无需重做 clock_mod。 */
    return with_retry(op_flash_init, RT_NULL, "flash_init");
}

uint8_t new8fx_erase_post(uint16_t addr)
{
    /* 与 erase_fire 同理：擦除 ACK 要等很久，超时≠失败，禁用 break+retry */
    return op_erase_post(&addr);
}

uint8_t new8fx_write_mem(uint16_t addr, const uint8_t *data, uint16_t len)
{
    uint8_t hdr[5] = {(uint8_t)(addr >> 8), (uint8_t)addr,
                      (uint8_t)(len >> 8), (uint8_t)len, 0xFF};

    (void)wire_send(hdr, 5U, GAP_RW_US);
    if (len > 0U)
        (void)wire_send(data, len, GAP_RW_US);
    /* Spec 7.8 说写完等 ~1 ms，但真机实测：闪存编程（charge pump）后 DA
     * 会忙相当长一段时间，立刻发读命令会超时/读到错位垃圾。保守等 10ms。 */
    rt_thread_mdelay(10);
    return L1_ST_OK;
}

uint8_t new8fx_read_mem(uint16_t addr, uint8_t *buf, uint16_t len)
{
    uint8_t hdr[5] = {(uint8_t)(addr >> 8), (uint8_t)addr,
                      (uint8_t)(len >> 8), (uint8_t)len, 0x00};
    int rc = 0;

    /* 读是幂等操作，允许固件内 break+retry（500K 链路首命令可能撞目标
     * UART 重配窗口；固件使用说明 §2.8 的恢复语义）。首字节窗口 100ms，
     * 覆盖写后 DA 忙于闪存编程的响应延迟（Spec 只给 ~1ms，真机远超）。 */
    for (rt_uint32_t attempt = 0U; attempt < 3U; attempt++)
    {
        (void)wire_send(hdr, 5U, GAP_RW_US);
        rc = wire_recv(buf, len, 100U);
        if (rc >= 0)
            return L1_ST_OK;
        if (rc == WIRE_E_ABORT)
            return L1_ST_ABORTED;   /* 不重试，立即上报 */
        if (attempt < 2U)
        {
            LOGW("read_mem @%04x rc=%d, break+retry %lu/2\n", (unsigned)addr,
                 rc, (unsigned long)(attempt + 1U));
            wire_send_break();
        }
    }
    return wire_rc_to_l1(rc);
}

uint8_t new8fx_cr_trim_write(uint16_t addr, uint8_t data)
{
    uint8_t arg[3] = {(uint8_t)(addr >> 8), (uint8_t)addr, data};
    return with_retry(op_cr_trim, arg, "cr_trim");
}

uint8_t new8fx_quit(void)
{
    static const uint8_t f[5] = {0x00, 0x00, 0x00, 0x00, 0x88};

    (void)wire_send(f, 5U, GAP_RW_US);
    rt_thread_mdelay(1);
    wire_set_baud(WIRE_BAUD_62500); /* 目标退回 bootloader 世界（62500） */
    return L1_ST_OK;                  /* 固件使用说明 §2.7：退出无 ACK */
}
