/**
 * @file swd_engine.c
 * @brief SWD 位流引擎编译包装（官方 SW_DP.c 以 O2 编译）
 *
 * 背景：SWD 为 bit-bang，每位耗时 = GPIO 写 + 延时循环 + 宏/调用开销。
 * 全局 -Og/-O0 时宏开销 ~20-30 周期/位，实际 SWCLK 远低于名义值
 * （-O0 下名义 1MHz 实测只有 ~0.8MHz，是烧录偏慢的主因之一）。
 * 本文件用 #pragma 将官方引擎单独提到 O2（开销 ~8-12 周期/位），
 * SW_DP.c 源文件本身保持逐字节官方原样。
 *
 * 构建配套：源列表只收编本文件；App/cmsis_dap/SW_DP.c 须排除编译
 * （否则会被重复编译、符号冲突）——CubeIDE 在 .cproject sourceEntry 加 excluding="cmsis_dap/SW_DP.c"。
 */
#pragma GCC push_options
#pragma GCC optimize("O2")
#include "cmsis_dap/SW_DP.c"
#pragma GCC pop_options
