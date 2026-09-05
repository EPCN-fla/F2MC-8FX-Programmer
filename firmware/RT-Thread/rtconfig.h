/* RT-Thread config file
 *
 * ⚠ 本文件为手工配置。
 *   CubeMX 重新生成会回退以下关键项，重生成后必须逐项恢复：
 *   - RT_USING_USER_MAIN / RT_USING_COMPONENTS_INIT 必须保持【未定义】
 *     （Nano 精简模型，main.c USER CODE 2 中调用 rtthread_startup()，
 *      rtthread_startup() 由 App/prog_tasks.c 提供；board 层以 CubeMX pack
 *      生成的 bsp/_template/cubemx_config/board.c 为准，不覆写）
 *   - RT_THREAD_PRIORITY_MAX = 8
 *   - RT_USING_MESSAGEQUEUE / RT_USING_MUTEX / RT_USING_OVERFLOW_CHECK 定义
 *   - RT_HOOK_USING_FUNC_PTR 定义（rt_tick_sethook 的编译前提，保 HAL 时基）
 *   - RT_USING_DEVICE 定义（App/drv_pin.c 的 rt_pin_* 挂接设备框架）
 *   - RT_USING_CONSOLE / RT_USING_FINSH 保持【未定义】
 *     （无 UART，日志走 SEGGER RTT；finsh 组件源不在本工程，定义会缺
 *     finsh_config.h 编译失败）
 */

#ifndef __RTTHREAD_CFG_H__
#define __RTTHREAD_CFG_H__

// <<< Use Configuration Wizard in Context Menu >>>
// <h>Basic Configuration
// <o>Maximal level of thread priority <8-256>
//  <i>Default: 32
#define RT_THREAD_PRIORITY_MAX 8

// <o>OS tick per second
//  <i>Default: 1000   (1ms)
#define RT_TICK_PER_SECOND 1000

// <o>Alignment size for CPU architecture data access
//  <i>Default: 4
#define RT_ALIGN_SIZE 4

// <o>the max length of object name<2-16>
//  <i>Default: 8
#define RT_NAME_MAX 16

// <c1>Using RT-Thread components initialization
//  <i>Using RT-Thread components initialization
//#define RT_USING_COMPONENTS_INIT
// </c>

// <c1>Using user main
//  <i>Using user main
//#define RT_USING_USER_MAIN
// </c>

// <o>the size of main thread<1-4086>
//  <i>Default: 512
#define RT_MAIN_THREAD_STACK_SIZE 1024
// </h>

// <h>Debug Configuration
// <c1>enable kernel debug configuration
//  <i>Default: enable kernel debug configuration
//#define RT_DEBUG
// </c>

// <o>enable components initialization debug configuration<0-1>
//  <i>Default: 0
#define RT_DEBUG_INIT 0

// <c1>thread stack over flow detect
//  <i> Diable Thread stack over flow detect
#define RT_USING_OVERFLOW_CHECK
// </c>
// </h>

// <h>Hook Configuration
// <c1>using hook
//  <i>using hook
#define RT_USING_HOOK
// </c>

// <c1>using idle hook
//  <i>using idle hook
//#define RT_USING_IDLE_HOOK
// </c>
// </h>

// <c1>using hook function pointer
//  <i>rt_tick_sethook/rt_scheduler_sethook 等函数指针式 hook
#define RT_HOOK_USING_FUNC_PTR
// </c>
// </h>

// <h>Software timers Configuration
// <c1> Enables user timers
// <i> Enables user timers
//#define RT_USING_TIMER_SOFT
// </c>

// <o>The priority level of timer thread <0-31>
//  <i>Default: 4
#define RT_TIMER_THREAD_PRIO 4

// <o>The stack size of timer thread <0-8192>
//  <i>Default: 512
#define RT_TIMER_THREAD_STACK_SIZE 512
// </h>

// <h>IPC(Inter-process communication) Configuration
// <c1>Using Semaphore
//  <i>Using Semaphore
#define RT_USING_SEMAPHORE
// </c>

// <c1>Using Mutex
//  <i>Using Mutex
#define RT_USING_MUTEX
// </c>

// <c1>Using Signal
//  <i>Using Signal
//#define RT_USING_SIGNALS
// </c>

// <c1>Using Event
//  <i>Using Event
//#define RT_USING_EVENT
// </c>

// <c1>Using MailBox
//  <i>Using MailBox
#define RT_USING_MAILBOX
// </c>

// <c1>Using Message Queue
//  <i>Using Message Queue
#define RT_USING_MESSAGEQUEUE
// </c>
// </h>

// <h>Memory Management Configuration
// <c1>Using Mempool Management
//  <i>Using Mempool Management
//#define RT_USING_MEMPOOL
// </c>

// <c1>Dynamic Heap Management
//  <i>Dynamic Heap Management
//  <i>⚠ 保持未定义：线程全部静态创建（rt_thread_init），零堆碎片风险
//#define RT_USING_HEAP
// </c>

// <c1>using small memory
//  <i>using small memory
//#define RT_USING_SMALL_MEM
// </c>

// <c1>Small Memory Algorithm
//  <i>Small Memory Algorithm
//#define RT_USING_SMALL_MEM_AS_HEAP
// </c>
// </h>

// <h>Console Configuration
// <c1>Using console
//  <i>Using console（无 UART，日志走 SEGGER RTT，见 App/rtt_log）
// #define RT_USING_CONSOLE
// </c>
// <o>the buffer size of console <1-1024>
//  <i>the buffer size of console
//  <i>Default: 128  (128Byte)
#define RT_CONSOLEBUF_SIZE 128
// </h>

// <h>Enable FinSH Configuration
// <c1>include shell config
//  <i> Select this choice if you using FinSH
//#define RT_USING_FINSH
// </c>
// <o>The stack size for finsh thread <64-40960>
//  <i>the buffer size of finsh
//  <i>Default: 1024  (1024 Byte)
#define FINSH_THREAD_STACK_SIZE 1024
// </h>

#if defined(RT_USING_FINSH)
    #include "finsh_config.h"
#endif

// <h>Device Configuration
// <c1>using device framework
//  <i>using device framework（App/drv_pin.c 提供 rt_pin_* 最小实现）
#define RT_USING_DEVICE
// </c>
// </h>

// <<< end of configuration section >>>

#endif

