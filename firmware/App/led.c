/**
 * led.c — 状态指示 LED（PA9 = TIM1_CH2 硬件 PWM）
 *
 * 为什么是硬件 PWM 而不是线程：
 *   烧录期间 f2mc 线程长时间忙等自旋（擦除 1.3s、读等待、DA.BIN 上传），
 *   低优先级 blink 线程被饿死 → LED 冻结；而高优先级线程的周期性抢占又
 *   曾导致 flash_init 上传丢帧（2026-08-06 A/B 实证，见 prog_tasks.c 沿革）。
 *   TIM1_CH2 硬件 PWM 不占任何 CPU/线程/中断资源，物理上不可能干扰 L2
 *   时序——包括 BASEPRI 屏蔽窗在内的任何阶段都能稳定闪烁。
 *
 * 实现要点：
 *   - PA9 在 GPIO 推挽输出（OFF/SOLID 静态电平）与 AF 推挽（PWM 闪烁）
 *     之间切换；切模式前先写 ODR 避免毛刺（同 wire.c TX 惯例）；
 *   - TIM1 时基 10kHz（PSC=7199）：SLOW=0.5Hz（ARR=19999），FAST=5Hz
 *     （ARR=1999），占空比恒 50%；ARPE=0，ARR 即时生效（切换瞬间可能
 *     缩短一个周期，纯视觉无感）；
 *   - TIM1 为高级定时器，BDTR.MOE 必须置位才有输出；
 *   - 极性：高电平点亮（PWM 高=亮）。若板子低电平点亮，CCER 加 CC2P(1<<5)
 *     并交换 pin_gpio_output 的电平即可；
 *   - TIM1 无 CubeMX 配置、无中断，寄存器直配（同 drv_pin 惯例）；
 *     CH4(PA11=USB DM) 不使能，与 USB 无冲突。
 */
#include "led.h"
#include "main.h"          /* GPIOA/TIM1/RCC（CMSIS 设备头） */
#include "rtt_log.h"

/* PA9 在 CRH 的 nibble 位置（pin 9 → CRH[7:4]）；CRL/CRH nibble 编码 */
#define PA9_CRH_SHIFT       4U
#define CRH_MODE_OUT_PP     0x3U    /* 推挽输出 50MHz */
#define CRH_MODE_AF_PP      0xBU    /* 复用推挽 50MHz */

#define TIM_TICK_HZ         10000U                    /* PSC=7199 → 10kHz */
#define ARR_FAST            (TIM_TICK_HZ / 2U - 1U)   /* 2Hz：4999 */
#define ARR_SLOW            (TIM_TICK_HZ * 2U - 1U)   /* 0.5Hz：19999 */

static volatile led_mode_t s_mode      = LED_MODE_OFF;
static volatile uint8_t    s_host_seen = 0U;

/* PA9 → GPIO 推挽输出静态电平（先写 ODR 再切模式，无毛刺） */
static void pin_gpio_output(uint8_t high)
{
    uint32_t crh;

    GPIOA->BSRR = (high != 0U) ? GPIO_BSRR_BS9 : GPIO_BSRR_BR9;
    crh  = GPIOA->CRH;
    crh &= ~(0xFU << PA9_CRH_SHIFT);
    crh |=  (CRH_MODE_OUT_PP << PA9_CRH_SHIFT);
    GPIOA->CRH = crh;
}

/* PA9 → TIM1_CH2 复用推挽（TIM1 立即接管引脚） */
static void pin_af_pwm(void)
{
    uint32_t crh = GPIOA->CRH;

    crh &= ~(0xFU << PA9_CRH_SHIFT);
    crh |=  (CRH_MODE_AF_PP << PA9_CRH_SHIFT);
    GPIOA->CRH = crh;
}

static void pwm_set_period(uint32_t arr)
{
    TIM1->ARR  = arr;
    TIM1->CCR2 = (arr + 1U) / 2U;   /* 50% 占空比 */
}

void led_init(void)
{
    RCC->APB2ENR |= RCC_APB2ENR_TIM1EN;
    TIM1->PSC   = 7199U;                    /* 72MHz / 7200 = 10kHz */
    TIM1->ARR   = ARR_FAST;
    TIM1->CCR2  = (ARR_FAST + 1U) / 2U;
    TIM1->CCMR1 = (6U << 12) | (1U << 11);  /* OC2M=PWM 模式1，OC2PE 预装载 */
    TIM1->CCER  = (1U << 4);                /* CC2E；低电平点亮的板子加 (1U<<5)=CC2P */
    TIM1->BDTR  = (1U << 15);               /* MOE——高级定时器输出总开关 */
    TIM1->CR1   = 1U;                       /* CEN */

    pin_gpio_output(0U);                    /* 默认 OFF（未连接上位机） */
}

void led_set_mode(led_mode_t m)
{
    if (m == s_mode)
        return;
    s_mode = m;

    switch (m)
    {
    case LED_MODE_OFF:
        pin_gpio_output(0U);
        break;
    case LED_MODE_SOLID:
        pin_gpio_output(1U);
        break;
    case LED_MODE_SLOW:
        pwm_set_period(ARR_SLOW);
        pin_af_pwm();
        break;
    case LED_MODE_FAST:
    default:
        pwm_set_period(ARR_FAST);
        pin_af_pwm();
        break;
    }
}

led_mode_t led_get_mode(void)
{
    return s_mode;
}

void led_notify_host_cmd(void)
{
    if (s_host_seen == 0U)
    {
        s_host_seen = 1U;
        if (s_mode == LED_MODE_OFF)
        {
            led_set_mode(LED_MODE_SOLID);
            LOGI("led: host online\n");
        }
    }
}

void led_notify_host_disconnect(void)
{
    s_host_seen = 0U;
    led_set_mode(LED_MODE_OFF);
    LOGI("led: host offline (DISCONNECT)\n");
}
