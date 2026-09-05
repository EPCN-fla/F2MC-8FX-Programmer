/**
 * @file rtt_log.c
 * @brief 极简 SEGGER RTT 上行通道实现（协议见 SEGGER RTT 文档）
 *
 * 控制块静态初始化于 .data，调试器按 "SEGGER RTT" ID 扫描 RAM 定位。
 */
#include <rtthread.h>
#include "rtt_log.h"

#define RTT_UP_BUF_SIZE  1024    /**< 上行缓冲大小（字节） */
#define RTT_MODE_SKIP    2U      /**< 满则丢弃，绝不阻塞 */

/** @brief RTT 上行通道描述符（目标→主机） */
typedef struct
{
    const char          *sName;
    char                *pBuffer;
    unsigned             SizeOfBuffer;
    volatile unsigned    WrOff;
    volatile unsigned    RdOff;
    volatile unsigned    Flags;
} RTT_UP_BUF;

/** @brief RTT 下行通道描述符（主机→目标，占位未用） */
typedef struct
{
    const char          *sName;
    char                *pBuffer;
    unsigned             SizeOfBuffer;
    volatile unsigned    WrOff;
    volatile unsigned    RdOff;
    volatile unsigned    Flags;
} RTT_DOWN_BUF;

/** @brief RTT 控制块（调试器按 acID 扫描定位） */
typedef struct
{
    char         acID[16];                /**< "SEGGER RTT" + 0 填充 */
    int          MaxNumUpBuffers;
    int          MaxNumDownBuffers;
    RTT_UP_BUF   aUp[1];
    RTT_DOWN_BUF aDown[1];
} RTT_CB;

static char _up_buf[RTT_UP_BUF_SIZE];
static char _down_buf[16];

/**
 * @brief RTT 控制块（.data 静态初始化）
 * @warning 不要加 section 属性自定义段名：链接脚本无对应规则会成为孤儿段，
 * 初值不被 startup 拷贝，指针成员为垃圾值 → HardFault。
 */
static RTT_CB _cb = {
    .acID = {'S','E','G','G','E','R',' ','R','T','T','\0','\0','\0','\0','\0','\0'},
    .MaxNumUpBuffers = 1,
    .MaxNumDownBuffers = 1,
    .aUp = {
        {
            .sName = "Terminal",
            .pBuffer = _up_buf,
            .SizeOfBuffer = sizeof(_up_buf),
            .WrOff = 0, .RdOff = 0, .Flags = RTT_MODE_SKIP,
        },
    },
    .aDown = {
        {
            .sName = "Terminal",
            .pBuffer = _down_buf,
            .SizeOfBuffer = sizeof(_down_buf),
            .WrOff = 0, .RdOff = 0, .Flags = RTT_MODE_SKIP,
        },
    },
};

void rtt_log_init(void)
{
    /* 控制块为静态初始化，此处仅保证符号被引用（防 -Og 优化掉未显式使用成员） */
    volatile unsigned *flags = &_cb.aUp[0].Flags;
    RT_UNUSED(flags);
}

void rtt_log_write(const char *s, unsigned len)
{
    RTT_UP_BUF *p = &_cb.aUp[0];
    unsigned wr = p->WrOff;
    unsigned rd = p->RdOff;
    unsigned space = (rd > wr) ? (rd - wr - 1) : (p->SizeOfBuffer - wr + rd - 1);

    if (len > space)
        len = space;    /* SKIP 模式：写不下的直接丢弃 */

    while (len--)
    {
        p->pBuffer[wr] = *s++;
        wr = (wr + 1 == p->SizeOfBuffer) ? 0 : wr + 1;
    }
    p->WrOff = wr;
}

void rtt_log_printf(const char *fmt, ...)
{
    char buf[128];
    int n;
    va_list args;

    va_start(args, fmt);
    n = rt_vsnprintf(buf, sizeof(buf), fmt, args);
    va_end(args);

    if (n <= 0)
        return;
    if (n >= (int)sizeof(buf))
        n = sizeof(buf) - 1;

    rtt_log_write(buf, (unsigned)n);
}
