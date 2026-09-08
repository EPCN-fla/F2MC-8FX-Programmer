# DA 结构解析

> 本文沉淀对 8FX 串行编程 DA（Download Agent）二进制的逆向过程与结论。
> 协议依据：`(SPEC)New8FX-Serial_PGM-V1.3.0.pdf` §7.5 / Table 7-1。
> 涉及固件：`firmware/App/new8fx.c`（DA 表与 FLASH_INIT 时序）。

---

## §1 DA 是什么

New8FX 目标芯片内置 mask boot ROM，提供串行编程模式（DBG 单线 UART）。
boot ROM 自身只完成握手/时钟修改/整片擦除；**读写 Flash 需要上位机先经
FLASH_INIT 把一段机器码（DA）下载到目标 RAM 并执行**——DA 接管后续通信
（并切到 500 Kbps），实现读/写/退出等命令。

关键事实：**DA 不是独立程序**。真正的读/写算法在 boot ROM 常驻的 RAM
监视器里，DA 只是"命令调度壳"——它解析 5 字节命令帧
`[AddrH, AddrL, LenH, LenL, CMD]`，然后跳进监视器完成实际工作。

---

## §2 反汇编方法（可复现）

1. **指令编码表**：[mnaberez/f2mc8dasm/tables.py](https://github.com/mnaberez/f2mc8dasm/blob/main/f2mc8dasm/tables.py)
   含完整 F2MC-8FX 指令表（256 条 opcode 全覆盖，含寄存器 bank 寻址
   `mov r0-r7` / `cmp r0-r7,#imm` 等 8FX 扩展）。
2. **交叉验证**：用 `FASM896S.EXE -cpu MB95F636H -l probe.asm` 汇编探针指令，
   从 `.lst` 列表文件读机器码，确认 `F1=movw a,sp`、`D4=movw abs,a`、`E5=movw sp,#imm16`、
   `70/71=movw a,ps / movw ps,a`、`E8-EF=callv #0-7`、`93=movw a,@a`、
   `E0=jmp @a`、`99 8?` 系列等关键编码与 f2mc8dasm 表一致。
3. **Spec 版 DA**：Table 7-1 的 PDF 文本含全部 198 字节（`XX*2`/`YY*3`
   为脚注占位符），按脚注代入：630H/690K 系列 `XX=0x02`（RW 期 500K）、`YY=0x7C`。

可用下面的脚本干净地线性反汇编到底（无死区）：按长度表跳过操作数即可
（本 ISA 无前缀字节、指令边界自同步）。把目标 DA 字节粘进 `DA` 即可复现 §3。

```python
"""da_dasm.py - F2MC-8FX DA 线性反汇编（无前缀字节，按长度表线性扫，边界自同步）"""

DA = bytes([  # 粘贴目标 DA；此处为 firmware/App/new8fx.c: DA_SPEC_M1
    0xF1,0xD4,0x01,0x56,0x05,0x7C,0x61,0x01,0x58,0xE5,0x01,0x60,0x41,0xF3,0x40,0x70,
    0x40,0xE4,0x70,0x30,0x71,0x04,0x00,0xEA,0x85,0x26,0x02,0xE9,0x10,0xE9,0xE3,0xE9,
    0x4F,0xE9,0x48,0xE9,0x49,0x99,0xFF,0xFD,0x31,0x99,0xAA,0xFD,0x56,0x99,0x55,0xFD,
    0x69,0x99,0x88,0xFD,0x7F,0x05,0x7C,0x64,0xCF,0x45,0x7C,0x98,0x00,0xFD,0x0F,0xD8,
    0xE4,0x00,0xD9,0x40,0xE4,0xFF,0xD4,0x93,0xE0,0xF2,0xEA,0x21,0x00,0xCB,0x9F,0x00,
    0xFD,0x05,0xDF,0xD8,0x21,0x00,0xD0,0x21,0x00,0xAB,0x05,0x7C,0x64,0xCF,0x45,0x7C,
    0xAE,0x0C,0x98,0x00,0xFD,0x0F,0xD8,0xE9,0xE2,0xE4,0x01,0x02,0x40,0xE4,0xFF,0xE0,
    0x93,0xE0,0x21,0x00,0xF2,0x9F,0x00,0xFD,0x05,0xDF,0xD8,0x21,0x00,0xF7,0xA6,0x0C,
    0x21,0x00,0xAB,0x05,0x7C,0x64,0xCF,0x45,0x7C,0xF3,0xE2,0xE4,0x01,0x24,0x40,0xE4,
    0xFF,0xDE,0x93,0xE0,0x05,0x81,0xEA,0x21,0x00,0xAB,0x05,0x7C,0x64,0xCF,0x45,0x7C,
    0xAE,0x0C,0x04,0x00,0x10,0x08,0xE2,0xE4,0x01,0x3F,0x40,0xE4,0xFD,0xDF,0xE0,0xA6,
    0x0C,0x21,0x00,0xAB,0x50,0x71,0x50,0xE3,0x51,0xC4,0x01,0x56,0xE1,0x60,0x01,0x58,
    0x45,0x7C,0x85,0x26,0x02,0x20,
])

# 长度表（字节）：默认 1；L2/L3 覆盖全部多字节编码
L2 = ({0x04,0x05,0x06,0x14,0x15,0x16,0x24,0x25,0x26,0x34,0x35,0x36,
       0x45,0x46,0x54,0x55,0x56,0x64,0x65,0x66,0x74,0x75,0x76,
       0xC5,0xC6,0xD5,0xD6}
      | set(range(0x88, 0x90)) | set(range(0x98, 0xA0))      # mov/cmp rN, #imm
      | set(range(0xA0, 0xB0)) | set(range(0xF8, 0x100)))    # setb/clrb, 条件分支
L3 = ({0x21,0x31,0x60,0x61,0x85,0x86,0x87,0x95,0x96,0x97,0xC4,0xD4}
      | set(range(0xB0, 0xC0)) | set(range(0xE4, 0xE8)))     # bbc/bbs, movw #imm16

FIX = {0x00:"nop", 0x01:"mulu a", 0x02:"rolc a", 0x03:"rorc a",
       0x10:"swap", 0x11:"divu a", 0x12:"cmp  a", 0x13:"cmpw a",
       0x20:"ret", 0x30:"reti",
       0x40:"pushw a", 0x41:"pushw ix", 0x42:"xch  a, t", 0x43:"xchw a, t",
       0x50:"popw a", 0x51:"popw ix", 0x52:"xor  a", 0x53:"xorw a",
       0x62:"and  a", 0x63:"andw a", 0x72:"or   a", 0x73:"orw  a",
       0x70:"movw a, ps", 0x71:"movw ps, a",
       0x80:"clri", 0x81:"clrc", 0x84:"daa", 0x90:"seti", 0x91:"setc",
       0x92:"mov  a, @a", 0x93:"movw a, @a", 0x94:"das",
       0xC0:"incw a", 0xC1:"incw sp", 0xC2:"incw ix", 0xC3:"incw ep",
       0xD0:"decw a", 0xD1:"decw sp", 0xD2:"decw ix", 0xD3:"decw ep",
       0xE0:"jmp  @a", 0xE1:"movw sp, a", 0xE2:"movw ix, a", 0xE3:"movw ep, a",
       0xF0:"movw a, pc", 0xF1:"movw a, sp", 0xF2:"movw a, ix", 0xF3:"movw a, ep",
       0xF4:"xchw a, pc", 0xF5:"xchw a, sp", 0xF6:"xchw a, ix", 0xF7:"xchw a, ep"}

ALU = {1:"cmp", 2:"addc", 3:"subc", 5:"xor", 6:"and", 7:"or"}
REG = {0:"mov  a, r{}", 1:"cmp  a, r{}", 2:"addc a, r{}", 3:"subc a, r{}",
       4:"mov  r{}, a", 5:"xor  a, r{}", 6:"and  a, r{}", 7:"or   a, r{}"}
BR  = ["bhs", "blo", "bp", "bn", "bne", "beq", "bge", "blt"]        # F8-FF
MW  = {0xE4:"a", 0xE5:"sp", 0xE6:"ix", 0xE7:"ep"}

def rel(pc, n, off):
    return pc + n + (off - 0x100 if off > 0x7F else off)

def text(pc, ins):
    op, n = ins[0], len(ins)
    b1 = ins[1] if n > 1 else 0
    w  = (ins[1] << 8) | ins[2] if n > 2 else 0
    hi, lo = op >> 4, op & 0x0F
    if lo >= 8 and hi < 8:                 return REG[hi].format(lo - 8)
    if hi in ALU and lo >= 4:
        if lo == 4: return f"{ALU[hi]:4s} a, #0x{b1:02X}"
        if lo == 5: return f"{ALU[hi]:4s} a, [0x{b1:02X}]"
        if lo == 6: return f"{ALU[hi]:4s} a, @ix+0x{b1:02X}"
        return f"{ALU[hi]:4s} a, @ep"      # lo == 7（单字节）
    if op == 0x04: return f"mov  a, #0x{b1:02X}"
    if op == 0x05: return f"mov  a, [0x{b1:02X}]"
    if op == 0x06: return f"mov  a, @ix+0x{b1:02X}"
    if op == 0x07: return "mov  a, @ep"
    if 0x88 <= op <= 0x9F and lo >= 8:
        return f"{'mov' if hi == 8 else 'cmp':4s} r{lo - 8}, #0x{b1:02X}"
    if 0xA0 <= op <= 0xAF:
        return f"{'clrb' if lo < 8 else 'setb'} [0x{b1:02X}]:{lo & 7}"
    if 0xB0 <= op <= 0xBF:
        return f"{'bbc' if lo < 8 else 'bbs':4s} [0x{b1:02X}]:{lo & 7}, 0x{rel(pc, 3, ins[2]):04X}"
    if 0xC8 <= op <= 0xCF: return f"inc  r{lo - 8}"
    if 0xD8 <= op <= 0xDF: return f"dec  r{lo - 8}"
    if 0xE8 <= op <= 0xEF: return f"callv #{lo - 8}"
    if 0xF8 <= op <= 0xFF: return f"{BR[lo - 8]:4s} 0x{rel(pc, 2, b1):04X}"
    if op in (0x21, 0x31): return f"{'jmp' if op == 0x21 else 'call':4s} 0x{w:04X}"
    if op == 0x45: return f"mov  [0x{b1:02X}], a"
    if op == 0x46: return f"mov  @ix+0x{b1:02X}, a"
    if op == 0x47: return "mov  @ep, a"
    if op == 0x60: return f"mov  a, 0x{w:04X}"
    if op == 0x61: return f"mov  0x{w:04X}, a"
    if op == 0x85: return f"mov  [0x{b1:02X}], #0x{ins[2]:02X}"
    if op == 0x86: return f"mov  @ix+0x{b1:02X}, #0x{ins[2]:02X}"
    if op == 0x87: return f"mov  @ep, #0x{ins[2]:02X}"
    if op == 0x95: return f"cmp  [0x{b1:02X}], #0x{ins[2]:02X}"
    if op == 0x96: return f"cmp  @ix+0x{b1:02X}, #0x{ins[2]:02X}"
    if op == 0x97: return f"cmp  @ep, #0x{ins[2]:02X}"
    if op == 0xC4: return f"movw a, 0x{w:04X}"
    if op == 0xC5: return f"movw a, [0x{b1:02X}]"
    if op == 0xC6: return f"movw a, @ix+0x{b1:02X}"
    if op == 0xC7: return "movw a, @ep"
    if op == 0xD4: return f"movw 0x{w:04X}, a"
    if op == 0xD5: return f"movw [0x{b1:02X}], a"
    if op == 0xD6: return f"movw @ix+0x{b1:02X}, a"
    if op == 0xD7: return "movw @ep, a"
    if op in MW: return f"movw {MW[op]}, #0x{w:04X}"
    return FIX.get(op, f".byte 0x{op:02X}")

pc = 0
while pc < len(DA):
    op = DA[pc]
    n = 3 if op in L3 else 2 if op in L2 else 1
    ins = DA[pc:pc + n]
    print(f"{pc:04X}: {' '.join(f'{b:02X}' for b in ins):12s}{text(pc, ins)}")
    pc += n
```

分支目标即脚本算出的绝对地址（rel8 相对下一条指令），16 位操作数为大端。
输出与 §3 逐行一致（注释为人工标注，脚本只产出反汇编文本）。

---

## §3 DA 结构剖析（DA_SPEC_M1）

```
0000: F1          movw a, sp            ; ┐
0001: D4 01 56    movw 0x0156, a        ; │ 保存 boot ROM 的 SP → 变量区
0004: 05 7C       mov  a, [0x7C]        ; │ 保存 RAM 变量 [0x7C]（YY 指向）
0006: 61 01 58    mov  0x0158, a        ; ┘
0009: E5 01 60    movw sp, #0x0160      ; 换 DA 自己的栈（RAM 0x0160 向下）
000C: 41          pushw ix              ; ┐ 保存 IX/EP/PS
000D: F3          movw a, ep            ; │
000E: 40          pushw a               ; │
000F: 70          movw a, ps            ; │
0010: 40          pushw a               ; ┘
0011: E4 70 30    movw a, #0x7030       ; ┐ PS=0x7030：切到寄存器 bank 14
0014: 71          movw ps, a            ; ┘ （r0-r7 → RAM 0x1E0-0x1EF）
0015: 04 00       mov  a, #0x00         ; ┐ CALLV #2 = boot ROM 服务"发 1 字节"：
0017: EA          callv #2              ; ┘ 回 ACK(0x00) —— DA 上线应答
0018: 85 26 02    mov  [0x26], #0x02    ; [0x26]=XX=0x02：通知 boot ROM 切 500K
                                        ;   （FLASH_INIT 尾帧后生效）
001B: E9          callv #1              ; ┐ CALLV #1 = boot ROM 服务"收 1 字节"
001C: 10          swap                  ; │ 收 5 字节命令帧头：
001D: E9          callv #1              ; │   b1(AddrH)→A(半字节交换，低用)
001E: E3          movw ep, a            ; │   b2(AddrL)→EP
001F: E9          callv #1              ; │   b3(LenH)→r7
0020: 4F          mov  r7, a            ; │   b4(LenL)→r0
0021: E9          callv #1              ; │   b5(CMD)→r1
0022: 48          mov  r0, a            ; │
0023: E9          callv #1              ; │
0024: 49          mov  r1, a            ; ┘
0025: 99 FF       cmp  r1, #0xFF        ; ┐ 命令调度（r1=CMD）：
0027: FD 31       beq  0x005A           ; │   0xFF 写    → 0x5A
0029: 99 AA       cmp  r1, #0xAA        ; │   0xAA 擦除  → 0x83
002B: FD 56       beq  0x0083           ; │   0x55 CR 写 → 0x9A
002D: 99 55       cmp  r1, #0x55        ; │   0x88 退出  → 0xB4（尾声）
002F: FD 69       beq  0x009A           ; │   其它(0x00) → 落进读路径 0x35
0031: 99 88       cmp  r1, #0x88        ; │
0033: FD 7F       beq  0x00B4           ; ┘
0035: 05 7C       mov  a, [0x7C]        ; ┐ 读路径：清 RAM 标志位 4/5
0037: 64 CF       and  a, #0xCF         ; │
0039: 45 7C       mov  [0x7C], a        ; ┘
003B: 98 00       cmp  r0, #0x00        ; r0=LenL==0？→ 查 LenH(r7)
003D: FD 0F       beq  0x004E
003F: D8          dec  r0               ; LenL--
0040: E4 00 D9    movw a, #0x00D9       ; ┐ 调监视器"读 1 字节"：
0043: 40          pushw a               ; │ 压返回地址 0x00D9（监视器内部续点）
0044: E4 FF D4    movw a, #0xFFD4       ; │ 取向量表 [0xFFD4]（boot ROM 入口）
0047: 93          movw a, @a            ; │
0048: E0          jmp  @a               ; ┘
0049: F2          movw a, ix            ; （监视器返回后经 0xD9 绕回这里）
004A: EA          callv #2              ; 把读到的字节经 CALLV #2 发给主机
004B: 21 00 CB    jmp  0x00CB           ; 跳监视器读循环（0x00CB）
004E: 9F 00       cmp  r7, #0x00        ; LenH==0？
0050: FD 05       beq  0x0057           ;   → 本命令完成，回主循环 0x00AB
0052: DF          dec  r7               ; LenH--，LenL 翻 0xFF 继续
0053: D8          dec  r0
0054: 21 00 D0    jmp  0x00D0           ; 跳监视器读循环另一入口（跨页）
0057: 21 00 AB    jmp  0x00AB           ; 回监视器主循环（等下一命令帧）
005A: 05 7C       mov  a, [0x7C]        ; ┐ 写路径（0xFF）：同样的标志位清理
005C: 64 CF       and  a, #0xCF         ; │
005E: 45 7C       mov  [0x7C], a        ; ┘
0060: AE 0C       setb [0x0C]:6         ; WDTC(0x0C) bit6：写期间喂狗配置
0062: 98 00       cmp  r0, #0x00
0064: FD 0F       beq  0x0075
0066: D8          dec  r0
0067: E9          callv #1              ; 收 1 个写数据字节
0068: E2          movw ix, a            ; IX = 数据
0069: E4 01 02    movw a, #0x0102       ; ┐ 调监视器"写 1 字节"（向量 [0xFFE0]）
006C: 40          pushw a               ; │
006D: E4 FF E0    movw a, #0xFFE0       ; │
0070: 93          movw a, @a            ; │
0071: E0          jmp  @a               ; ┘
0072: 21 00 F2    jmp  0x00F2           ; 监视器写循环（0x00F2）
0075: 9F 00       cmp  r7, #0x00
0077: FD 05       beq  0x007E
0079: DF          dec  r7
007A: D8          dec  r0
007B: 21 00 F7    jmp  0x00F7           ; 写循环跨页入口
007E: A6 0C       clrb [0x0C]:6         ; 写完恢复 WDTC
0080: 21 00 AB    jmp  0x00AB           ; 回监视器主循环
0083: 05 7C       mov  a, [0x7C]        ; ┐ 擦除路径（0xAA，RW 模式内）：标志位清理
0085: 64 CF       and  a, #0xCF         ; │
0087: 45 7C       mov  [0x7C], a        ; ┘
0089: F3          movw a, ep            ; ┐ IX = EP = 擦除地址
008A: E2          movw ix, a            ; ┘
008B: E4 01 24    movw a, #0x0124       ; ┐ 调监视器"擦除"（向量 [0xFFDE]），
008E: 40          pushw a               ; │ 返回点 0x0124
008F: E4 FF DE    movw a, #0xFFDE       ; │
0092: 93          movw a, @a            ; │
0093: E0          jmp  @a               ; ┘
0094: 05 81       mov  a, [0x81]        ; ┐ 取擦除结果（[0x81]）经 CALLV #2
0096: EA          callv #2              ; ┘ 发给主机
0097: 21 00 AB    jmp  0x00AB           ; 回监视器主循环
009A: 05 7C       mov  a, [0x7C]        ; ┐ CR 校准写路径（0x55）：标志位清理
009C: 64 CF       and  a, #0xCF         ; │
009E: 45 7C       mov  [0x7C], a        ; ┘
00A0: AE 0C       setb [0x0C]:6         ; WDTC bit6：写期间喂狗配置
00A2: 04 00       mov  a, #0x00         ; ┐ IX = r0（帧内 LenL 字段作写入数据；
00A4: 10          swap                  ; │ 前两条实际被 mov a,r0 覆盖）
00A5: 08          mov  a, r0            ; │
00A6: E2          movw ix, a            ; ┘
00A7: E4 01 3F    movw a, #0x013F       ; ┐ 直跳 boot ROM 的 CR 写例程 0xFDDF，
00AA: 40          pushw a               ; │ 返回点 0x013F
00AB: E4 FD DF    movw a, #0xFDDF       ; │
00AE: E0          jmp  @a               ; ┘
00AF: A6 0C       clrb [0x0C]:6         ; 写完恢复 WDTC
00B1: 21 00 AB    jmp  0x00AB           ; 回监视器主循环
00B4: 50          popw a                ; ┐ 退出路径（0x88）：恢复 PS/EP/IX
00B5: 71          movw ps, a            ; │
00B6: 50          popw a                ; │
00B7: E3          movw ep, a            ; │
00B8: 51          popw ix               ; ┘
00B9: C4 01 56    movw a, 0x0156        ; 恢复 boot ROM 的 SP
00BC: E1          movw sp, a
00BD: 60 01 58    mov  a, 0x0158        ; 恢复 [0x7C]
00C0: 45 7C       mov  [0x7C], a
00C2: 85 26 02    mov  [0x26], #0x02    ; 恢复波特率变量
00C5: 20          ret                   ; RET 回 boot ROM（boot ROM 回 ACK，
                                        ;   目标退回 62500 bootloader 世界）
```

---

## §4 与 boot ROM 的耦合点（型号敏感性的来源）

DA 里所有"除 DA 之外"的地址都是**硬编码绝对地址**，分三类：

| 类型 | DA 地址 | 说明 |
|---|---|---|
| 变量区（保存 SP/[0x7C]） | 0x0156 / 0x0158 | 位移 **+0x39** = 57 = 198−141（变量紧跟 DA 代码尾部） |
| 监视器入口（JMP 目标） | 0x00CB/0x00D0/0x00F2/0x00F7/0x00AB | 位移 **+8**（0x00AB 两版相同） |
| 监视器调用返回点 | 0x00D9 / 0x0102 | 位移 +8 |
| boot ROM 向量/直跳 | [0xFFD4] [0xFFE0] [0xFFDE] + `jmp 0xFDDF` | ROM/MON/擦除/CR 调用点 |

特殊命令处理：
- `0xAA`（RW 模式内擦除）：调监视器 [0xFFDE] 向量，返回点 0x0124；
- `0x55`（CR 校准写）：直接 `jmp 0xFDDF`（boot ROM 绝对地址），返回点 0x013F。

---

## §5 固件侧 DA 匹配（v0.2.0 现行设计）

DA 内嵌在固件 `new8fx.c`（`DA_SPEC_M1`）；上位机经 `SET_CHIP(0x13)` 下发
型号名（如 "MB95F698K"），固件按系列（型号名第 6-7 位数字）匹配 DA；
未匹配/未下发时用默认。DISCONNECT 后回到默认。
