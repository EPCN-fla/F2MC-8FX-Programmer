/**
 * @file wire.c
 * @brief 单线 UART bit-bang 驱动实现（固件使用说明 §3.5）
 *
 * 定时基准：DWT->CYCCNT @72MHz（62500=1152 cyc/bit，500K=144 cyc/bit）。
 *
 * TX（PB14，STM32F1 原生开漏输出模式 CRH=0x7）：
 *   - 逐位边界用 CYCCNT 滚动对齐（t += CPB），消除代码路径抖动累积；
 *   - 每位仅需单次 BSRR/BRR 写（~5 周期），500K（2µs/位）波形相位稳定；
 *   - 整帧期间 BASEPRI=0x10 屏蔽优先级≥1 中断（USB/SysTick/PendSV），
 *     放行优先级 0 的 EXTI；500K 一帧 10 字节约 200 µs，62500 约 1.6 ms。
 *
 * RX 两级（PB12）：
 *   - 62500（位宽 16µs）：EXTI 下降沿捕获 CYCCNT 时间戳 + 任务上下文按
 *     t0+1.5T…8.5T 轮询采样 8 位 + 9.5T 校验停止位，采样期 BASEPRI 屏蔽；
 *   - ≥250K（位宽 2µs）：EXTI ISR 路径固有延迟不可接受，改为 BASEPRI
 *     屏蔽下紧轮询 IDR 沿检测 + rx8_500k（.RamFunc + O2）三点多数表决采样。
 *
 * @warning 本文件内所有旋转等待均不得调用 RTOS API（临界区/调度会破坏
 * 采样对齐）；忙等自旋也不可让出 CPU（睡醒时字节早已错过）。LED 闪烁不
 * 依赖线程（TIM1 硬件 PWM，见 led.c），故自旋不会冻结 LED。
 */
#include "wire.h"
#include "f2mc_hw.h"
#include "cmd.h"
#include "rtt_log.h"
#include "main.h"

#define CPU_HZ              72000000UL
#define CYC_PER_US          (CPU_HZ / 1000000UL)        /**< 72 */
#define CYC_PER_MS          (CPU_HZ / 1000UL)           /**< 72000 */
#define WIRE_BASEPRI_MASK   0x10U    /**< 屏蔽优先级≥1，放行 EXTI(0) */

static uint32_t s_cpb = CPU_HZ / WIRE_BAUD_62500;   /**< cycles per bit，当前波特率 */

/** @name EXTI 起始沿捕获邮箱（ISR 写，任务读）
 * @{ */
static volatile rt_bool_t s_start_flag;
static volatile uint32_t  s_start_t0;
/** @} */

static volatile uint32_t  s_exti_hits;  /**< EXTI 触发计数（排障：区分"线上无活动"与"采样解析失败"） */
uint32_t wire_exti_hit_count(void) { return s_exti_hits; }

/* ------------------------------------------------------------------ */
/* 底层原语                                                             */
/* ------------------------------------------------------------------ */

/** @brief 使能 DWT CYCCNT（µs 级定时基准） */
static void dwt_init(void)
{
    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    DWT->CYCCNT = 0U;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
}

/** @brief 自旋等待 CYCCNT 到达 target（仅用于 <200µs 窗口，防 32 位回绕） */
static void wait_cyc(uint32_t target)
{
    while ((int32_t)(DWT->CYCCNT - target) < 0)
        ;
}

/**
 * @brief 确保 PB14 为开漏输出并释放（ODR=1）
 * @note  500K TX 相位关键修复：PB14 固定开漏输出模式（CRH nibble6 = 0x7：
 * MODE=11 50MHz + CNF=01 开漏），TX 仅需单次写 BSRR/BRR（~5 周期）。
 * 旧实现逐位调用 rt_pin_write+rt_pin_mode（~50-75 周期 = 0.7-1.0µs），
 * 500K（2µs/位）下波形相位推后 35-50%，接收端采样全错（CH340 嗅探实锤）。
 * 先写 ODR 再切模式，无毛刺。
 */
static inline void dbg_tx_ensure_od(void)
{
    GPIOB->BSRR = (1U << 14);   /* 先释放（ODR=1），再配置，无毛刺 */
    GPIOB->CRH = (GPIOB->CRH & ~(0xFU << 24)) | (0x7U << 24);
}

/** @brief 驱动一位：1=开漏释放（外部上拉成形高电平），0=开漏驱动低 */
static inline void tx_bit(rt_bool_t one)
{
    if (one)
        GPIOB->BSRR = (1U << 14);
    else
        GPIOB->BRR  = (1U << 14);
}

void wire_rx_arm(void)
{
    EXTI->PR = EXTI_PR_PR12;   /* 清掉屏蔽期（含自身 TX 耦合）锁存的挂起 */
    EXTI->IMR |= EXTI_IMR_MR12;
}

void wire_rx_disarm(void)
{
    EXTI->IMR &= ~EXTI_IMR_MR12;
    EXTI->PR = EXTI_PR_PR12;   /* 清屏蔽期积累的挂起，防 arm 后误触发 */
}

/**
 * @brief EXTI15_10 快速路径（stm32f1xx_it.c USER CODE 区直接调用，绕过
 * HAL_GPIO_EXTI_IRQHandler → HAL 回调链）
 * @note 第一条语句即读 CYCCNT 取时间戳。HAL 路径从 flash 冷执行要 3~6µs，
 * 500K（2µs/bit）下采样点后移约 3 个位（实测 DA 的 0x00 被读成 0xC0）；
 * 本路径延迟 ~0.3µs。
 */
void wire_exti_fastpath(void)
{
    uint32_t t = DWT->CYCCNT;

    if ((EXTI->PR & EXTI_PR_PR12) == 0U)
        return;                     /* 非 PB12 挂起（本工程无其它 EXTI） */
    EXTI->PR = EXTI_PR_PR12;        /* 清挂起 */

    s_start_t0 = t;
    s_start_flag = RT_TRUE;
    s_exti_hits++;
    EXTI->IMR &= ~EXTI_IMR_MR12;    /* disarm，等任务进入采样窗口 */
}

/* ------------------------------------------------------------------ */
/* TX                                                                   */
/* ------------------------------------------------------------------ */

/**
 * @brief 发送一字节（起始+8 数据+停止）
 * @param b   数据（LSB first）
 * @param t   滚动位边界（输入=起始沿时刻，输出=停止位末）
 */
static void send_byte(uint8_t b, uint32_t *t)
{
    uint8_t i;

    tx_bit(RT_FALSE);                       /* start */
    *t += s_cpb;
    wait_cyc(*t);

    for (i = 0U; i < 8U; i++)               /* LSB first */
    {
        tx_bit((rt_bool_t)((b >> i) & 1U));
        *t += s_cpb;
        wait_cyc(*t);
    }

    tx_bit(RT_TRUE);                        /* stop */
    *t += s_cpb;
    wait_cyc(*t);
}

int wire_send(const uint8_t *data, uint16_t len, uint32_t gap_us)
{
    uint16_t i;
    uint32_t t;
    uint32_t gap_cyc = gap_us * CYC_PER_US;
    uint32_t old = __get_BASEPRI();

    /* 先完成全部建立工作再捕获 t：从 t 捕获到驱动起始位只剩 send_byte
     * 调用开销（~0.2µs，可忽略），起始位保持完整位宽。若先捕获 t 再建立
     * （旧代码），建立开销 ~1µs+ 会截短起始位，500K 下接收器每个采样点
     * 晚约 1 个位，帧首字节 MSB 恒读 1（实测 10->88, 07->83）。 */
    __set_BASEPRI(WIRE_BASEPRI_MASK);
    dbg_tx_ensure_od();   /* pgmseq 可能把 PB14 切成了推挽/输入 */
    t = DWT->CYCCNT;
    for (i = 0U; i < len; i++)
    {
        send_byte(data[i], &t);
        if ((i + 1U < len) && (gap_cyc != 0U))
        {
            t += gap_cyc;                   /* 字节间隔（空闲高） */
            wait_cyc(t);
        }
    }
    __set_BASEPRI(old);
    return 0;
}

void wire_send_break(void)
{
    uint32_t old = __get_BASEPRI();

    dbg_tx_ensure_od();
    __set_BASEPRI(WIRE_BASEPRI_MASK);
    tx_bit(RT_FALSE);
    wait_cyc(DWT->CYCCNT + 25U * s_cpb);    /* 2.5 帧时长（1 帧=10 位） */
    tx_bit(RT_TRUE);
    __set_BASEPRI(old);
}

/**
 * @brief 500K 采样核心：RAM 中执行（0 等待态）+ O2，预计算递增截止期
 * @param t0   起始沿 CYCCNT 时刻
 * @param qtr  1/4 位周期（cycle）
 * @return byte | (stop_ok<<8)
 * @note  教训：-Og + flash 2 等待态下每次投票 ~150-190 周期（≈2.2µs > 1 位时），
 * 采样点被代码速度拖成匀速漂移，位 5-7 全落在停止位（vtdbg 实测）。
 * 本函数每票 ~15 周期：自旋等待 + IDR 读 + 截止期递增，纯寄存器操作。
 * 投票点 1.25T/1.5T/1.75T 多数表决；停止位 9.25/9.5/9.75T。
 */
__attribute__((section(".RamFunc"), optimize("O2"), noinline))
static uint32_t rx8_500k(uint32_t t0, uint32_t qtr)
{
    volatile uint32_t *cyccnt = &DWT->CYCCNT;
    volatile uint32_t *idr    = &GPIOB->IDR;
    uint32_t dl = t0 + 5U * qtr;    /* 首票 1.25T */
    uint32_t b = 0U;
    uint32_t i;
    uint32_t k;
    uint32_t v;

    for (i = 0U; i < 8U; i++)
    {
        v = 0U;
        for (k = 0U; k < 3U; k++)
        {
            while ((int32_t)(*cyccnt - dl) < 0) { }
            v += (*idr >> 12) & 1U;
            dl += qtr;
        }
        b >>= 1;
        if (v >= 2U)
            b |= 0x80U;
        dl += qtr;                  /* 跨位补齐 1 个 quarter */
    }
    /* 停止位 9.25/9.5/9.75T */
    v = 0U;
    for (k = 0U; k < 3U; k++)
    {
        while ((int32_t)(*cyccnt - dl) < 0) { }
        v += (*idr >> 12) & 1U;
        dl += qtr;
    }
    if (v >= 2U)
        b |= 0x100U;
    return b;
}

/**
 * @brief 轮询模式 RX 单字节（≥250K 高速波特率）
 * @note EXTI ISR 路径的固有延迟（NVIC 进入 + prologue + flash 冷执行，
 * 实测等效数 µs）相对 2µs 位宽不可接受（DA 的 0x00 被采成 0xC0）。
 * 改为 BASEPRI 屏蔽下紧轮询 PB12 IDR（迭代 ~0.15µs），沿检测当场读
 * CYCCNT，采样精度 ±0.2µs。
 * @warning 轮询窗口内 SysTick 冻结，不得调用 RTOS API；截止期用 CYCCNT 算。
 */
static int recv_byte_poll(uint8_t *out, uint32_t timeout_ms)
{
    uint8_t b = 0U;
    uint32_t t0;
    uint32_t t_start = DWT->CYCCNT;
    uint32_t budget = timeout_ms * CYC_PER_MS;

    /* 等下降沿（线空闲为高；进来即低说明边沿已错过，等下一位回升又会误判，
     * 故直接等超时由上层重试） */
    while ((GPIOB->IDR & (1U << 12)) != 0U)
    {
        if ((uint32_t)(DWT->CYCCNT - t_start) >= budget)
            return WIRE_E_TIMEOUT;
    }
    t0 = DWT->CYCCNT;

    {
        uint32_t r = rx8_500k(t0, s_cpb / 4U);

        if ((r & 0x100U) == 0U)
            return WIRE_E_FRAMING;
        b = (uint8_t)(r & 0xFFU);
    }
    *out = b;
    return 0;
}

/* ------------------------------------------------------------------ */
/* RX                                                                   */
/* ------------------------------------------------------------------ */

/**
 * @brief 接收一字节（EXTI 时间戳路径，62500 用）
 * @return 0=OK；<0=WIRE_E_* 错误码
 * @warning 起始沿等待是毫秒级业务，deadline 用 rt tick（ms 域）；CYCCNT 是
 * 32 位计数器，>29.8s 的超时用周期差比较会溢出误判（实测：60s 被回绕
 * 成 349ms，40s 则立即超时——擦除 ACK 因此从未被真正等待过）。
 * CYCCNT 只留给出沿后的 µs 级采样窗口（<200µs，安全）。
 * @warning 等起始沿为忙等自旋，不可让出 CPU（起始沿 t0 虽由 EXTI ISR 捕获，
 * 但线程睡醒再采样会错过整个字节——62500 一字节仅 160µs）；自旋中检查
 * ABORT(0x12) 中止标志。
 */
static int recv_byte(uint8_t *out, uint32_t timeout_ms)
{
    uint8_t i;
    uint8_t b = 0U;
    uint32_t t0, old;
    rt_tick_t deadline = rt_tick_get() + rt_tick_from_millisecond(timeout_ms);

    s_start_flag = RT_FALSE;
    wire_rx_arm();
    while (!s_start_flag)
    {
        if (cmd_abort_pending() != 0)
        {
            wire_rx_disarm();
            return WIRE_E_ABORT;
        }
        if ((rt_int32_t)(rt_tick_get() - deadline) >= 0)
        {
            wire_rx_disarm();
            return WIRE_E_TIMEOUT;
        }
    }
    s_start_flag = RT_FALSE;
    t0 = s_start_t0;

    /* 采样窗口：BASEPRI 屏蔽，禁止 RTOS API；t0+1.5T、2.5T…8.5T 采 8 位 */
    old = __get_BASEPRI();
    __set_BASEPRI(WIRE_BASEPRI_MASK);
    for (i = 0U; i < 8U; i++)
    {
        wait_cyc(t0 + (3U + 2U * i) * (s_cpb / 2U));
        if (rt_pin_read(DBG_RX_PIN))
            b |= (uint8_t)(1U << i);
    }
    /* 9.5T 校验停止位（必须为高） */
    wait_cyc(t0 + 19U * (s_cpb / 2U));
    if (!rt_pin_read(DBG_RX_PIN))
    {
        __set_BASEPRI(old);
        return WIRE_E_FRAMING;
    }
    __set_BASEPRI(old);

    *out = b;
    return 0;
}

int wire_recv(uint8_t *buf, uint16_t len, uint32_t timeout_ms)
{
    uint16_t i;
    int rc;

    /* ≥250K 且等待 ≤200ms：整段 BASEPRI 屏蔽轮询收（屏蔽窗 = 全部字节，
     * SysTick 短暂冻结可接受；长超时仍走 EXTI 时间戳路径）。 */
    if (s_cpb <= 288U && timeout_ms <= 200U)
    {
        uint32_t old = __get_BASEPRI();

        __set_BASEPRI(WIRE_BASEPRI_MASK);
        for (i = 0U; i < len; i++)
        {
            /* 首字节用调用方超时；后续字节目标间隔很短，给 10 ms 上限 */
            rc = recv_byte_poll(&buf[i], (i == 0U) ? timeout_ms : 10U);
            if (rc != 0)
            {
                __set_BASEPRI(old);
                return rc;
            }
        }
        __set_BASEPRI(old);
        return (int)len;
    }

    for (i = 0U; i < len; i++)
    {
        /* 首字节用调用方超时；后续字节目标间隔很短，给 10 ms 上限 */
        rc = recv_byte(&buf[i], (i == 0U) ? timeout_ms : 10U);
        if (rc != 0)
            return rc;
    }
    return (int)len;
}

/* ------------------------------------------------------------------ */
/* 初始化 / 波特率 / 自测                                                */
/* ------------------------------------------------------------------ */

void wire_set_baud(uint32_t baud)
{
    s_cpb = CPU_HZ / baud;
}

void wire_init(void)
{
    dwt_init();
    wire_rx_disarm();
    dbg_tx_ensure_od();   /* 开漏输出 + 释放：等效高阻，不倒灌无电目标 */
    rt_pin_mode(SWCLK_PIN, PIN_MODE_INPUT);    /* 防 SWDIO-SWCLK 误短接互顶 */
}

/**
 * @brief 62500 单线程逐位回读自测
 * @note 逐位驱动 PB14 → 等 1 位时间 → 从 PB12 回读应与驱动值一致
 * （220Ω loopback）。开漏高电平由 PB12 内部上拉提供（~40k，62500 位宽
 * 16µs 足够爬升），目标侧上拉无电也能测；不验证 EXTI/时间戳帧接收路径。
 * 调用前提：目标未上电（避免与目标驱动冲突）。
 */
rt_err_t wire_loopback_test(void)
{
    /* 覆盖 0/1 密度、沿变化、单 bit 的各种组合 */
    static const uint8_t pattern[] = {
        0x00, 0xFF, 0x55, 0xAA, 0x0F, 0xF0, 0x01, 0x80, 0xA5, 0x5A,
    };
    uint32_t errors = 0U;
    uint32_t i, old;
    rt_base_t saved_cpb = (rt_base_t)s_cpb;

    wire_set_baud(WIRE_BAUD_62500);
    rt_pin_mode(DBG_RX_PIN, PIN_MODE_INPUT_PULLUP);   /* 开漏高的上拉源 */

    old = __get_BASEPRI();
    __set_BASEPRI(WIRE_BASEPRI_MASK);

    for (i = 0U; i < sizeof(pattern); i++)
    {
        uint32_t t = DWT->CYCCNT;
        uint8_t b = pattern[i];
        uint8_t n;

        /* start */
        tx_bit(RT_FALSE);
        t += s_cpb; wait_cyc(t);
        if (rt_pin_read(DBG_RX_PIN) != PIN_LOW) errors++;

        for (n = 0U; n < 8U; n++)
        {
            rt_bool_t one = (rt_bool_t)((b >> n) & 1U);
            tx_bit(one);
            t += s_cpb; wait_cyc(t);
            if ((rt_pin_read(DBG_RX_PIN) == PIN_HIGH) != one) errors++;
        }

        /* stop */
        tx_bit(RT_TRUE);
        t += s_cpb; wait_cyc(t);
        if (rt_pin_read(DBG_RX_PIN) != PIN_HIGH) errors++;
    }

    __set_BASEPRI(old);

    tx_bit(RT_TRUE);                              /* 回到空闲（输入） */
    rt_pin_mode(DBG_RX_PIN, PIN_MODE_INPUT);      /* 恢复无上下拉 */
    s_cpb = (uint32_t)saved_cpb;

    return (errors == 0U) ? RT_EOK : -RT_ERROR;
}
