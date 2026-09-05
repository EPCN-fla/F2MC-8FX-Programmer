/**
 * @file drv_pin.c
 * @brief RT-Thread 风格最小 pin 驱动层实现（STM32F1，寄存器级）
 *
 * 引脚时钟由 CubeMX 生成的 MX_GPIO_Init() 统一使能（GPIOA/GPIOB），
 * 本层不负责 RCC。模式配置直接写 CRL/CRH（F1 风格 CNF/MODE 位）。
 */
#include "drv_pin.h"
#include "main.h"

/** @name F1 CRL/CRH 每引脚 4 bit 编码（MODE[1:0] + CNF[1:0]）
 * @{ */
#define PIN_CFG_OUTPUT_PP_50M   0x3U    /**< MODE=11(50MHz) CNF=00：推挽输出 */
#define PIN_CFG_OUTPUT_OD_50M   0x7U    /**< MODE=11 CNF=01：开漏输出 */
#define PIN_CFG_INPUT_FLOAT     0x4U    /**< MODE=00 CNF=01：浮空输入 */
#define PIN_CFG_INPUT_PULL      0x8U    /**< MODE=00 CNF=10：上下拉输入（ODR 定方向） */
/** @} */

/** @brief 引脚编号 → GPIO 端口基址（A/B/... 等距排列） */
static GPIO_TypeDef *pin_port(rt_base_t pin)
{
    return (GPIO_TypeDef *)(GPIOA_BASE + (pin >> 4) * (GPIOB_BASE - GPIOA_BASE));
}

/** @brief 引脚编号 → 位掩码 */
static rt_uint32_t pin_mask(rt_base_t pin)
{
    return 1UL << (pin & 0x0F);
}

rt_base_t rt_pin_get(const char *name)
{
    /* 格式 "PA.9" / "PB12" / "PA9" */
    rt_base_t port, pin;

    if (name[0] != 'P')
        return PIN_NONE;

    port = name[1];
    if (port < 'A' || port > 'G')
        return PIN_NONE;

    {
        const char *p = (name[2] == '.') ? name + 3 : name + 2;
        if (*p < '0' || *p > '9')
            return PIN_NONE;
        pin = *p++ - '0';
        if (*p >= '0' && *p <= '9')
            pin = pin * 10 + (*p - '0');
    }

    if (pin < 0 || pin > 15)
        return PIN_NONE;

    return DRV_PIN(port, pin);
}

void rt_pin_mode(rt_base_t pin, rt_uint8_t mode)
{
    GPIO_TypeDef *port = pin_port(pin);
    rt_uint32_t n = pin & 0x0F;
    rt_uint32_t cfg;
    rt_uint32_t pos, tmp;

    switch (mode)
    {
    case PIN_MODE_OUTPUT:        cfg = PIN_CFG_OUTPUT_PP_50M; break;
    case PIN_MODE_OUTPUT_OD:     cfg = PIN_CFG_OUTPUT_OD_50M; break;
    case PIN_MODE_INPUT:         cfg = PIN_CFG_INPUT_FLOAT;   break;
    case PIN_MODE_INPUT_PULLUP:
    case PIN_MODE_INPUT_PULLDOWN: cfg = PIN_CFG_INPUT_PULL;   break;
    default: return;
    }

    rt_enter_critical();

    if (n < 8)
    {
        pos = n * 4;
        tmp = port->CRL;
        tmp &= ~(0xFU << pos);
        tmp |= cfg << pos;
        port->CRL = tmp;
    }
    else
    {
        pos = (n - 8) * 4;
        tmp = port->CRH;
        tmp &= ~(0xFU << pos);
        tmp |= cfg << pos;
        port->CRH = tmp;
    }

    /* 上下拉输入由 ODR 位选择上拉还是下拉 */
    if (mode == PIN_MODE_INPUT_PULLUP)
        port->BSRR = pin_mask(pin);
    else if (mode == PIN_MODE_INPUT_PULLDOWN)
        port->BRR = pin_mask(pin);

    rt_exit_critical();
}

void rt_pin_write(rt_base_t pin, rt_uint8_t value)
{
    if (value == PIN_LOW)
        pin_port(pin)->BRR = pin_mask(pin);
    else
        pin_port(pin)->BSRR = pin_mask(pin);
}

rt_int8_t rt_pin_read(rt_base_t pin)
{
    return (pin_port(pin)->IDR & pin_mask(pin)) ? PIN_HIGH : PIN_LOW;
}
