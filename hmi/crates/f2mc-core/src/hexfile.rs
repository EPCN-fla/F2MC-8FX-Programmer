//! 烧录文件解析与清洗：Intel HEX / Motorola S-record 自动嗅探、越界过滤、
//! NVR/安全位剔除、连续区间合并。
//!
//! 支持格式（按内容嗅探，不看扩展名）：
//! - Intel HEX（.hex / .ihx / .ehx）—— `:` 起始，含扩展地址记录
//! - Motorola S-record（.mhx / .s19 / .mot）—— `S` 起始（S1/S2/S3 数据记录）
//!   依据：references/fgm-monitor/EmlCmnB_896.mhx、8FX-MCU-master 各样例

use crate::chipdef::{ChipDef, FLASH_LOW};
use crate::error::{ProgError, Result};
use crate::new8fx::{NVR_HIGH, NVR_LOW, SECURE_ADDR};

/// 文件格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexFormat {
    /// Intel HEX（`:` 起始）
    IntelHex,
    /// Motorola S-record（`S` 起始）
    SRecord,
}

impl HexFormat {
    /// 格式显示名
    pub fn label(&self) -> &'static str {
        match self {
            HexFormat::IntelHex => "Intel HEX",
            HexFormat::SRecord => "Motorola S-record",
        }
    }
}

/// 清洗后的烧录镜像：若干连续区间（升序、已合并、无空洞填充）
#[derive(Debug, Clone)]
pub struct HexImage {
    /// (起始地址, 数据)，按起始地址升序，互不重叠
    pub segments: Vec<(u32, Vec<u8>)>,
    /// 解析/清洗过程产生的警告
    pub warnings: Vec<String>,
    /// 嗅探到的文件格式
    pub format: HexFormat,
}

impl HexImage {
    /// 镜像总字节数（各区间之和）
    pub fn total_bytes(&self) -> usize {
        self.segments.iter().map(|(_, d)| d.len()).sum()
    }
}

/// 解析烧录文件（Intel HEX / S-record 自动嗅探）并清洗：
/// - 越界 `< 0x1000`：硬错误
/// - NVR 区 `0xFFBB~0xFFBF`：剔除并告警（保护 CR 校准值）
/// - `0xFFFC` 安全位：剔除并告警（由"写入安全位"选项单独处理）
/// - 超出所选型号 Flash 映射范围（下 bank 0x1000~0x1FFF + 上 bank）：剔除并告警
///
/// @param text 文件全文
/// @param chip 目标型号（决定 Flash 映射范围）
/// @returns 清洗后的烧录镜像
pub fn parse(text: &str, chip: &ChipDef) -> Result<HexImage> {
    let format = sniff_format(text)?;
    let sparse = match format {
        HexFormat::IntelHex => parse_intel_hex(text)?,
        HexFormat::SRecord => parse_srecord(text)?,
    };
    wash(sparse, chip, format)
}

/// 内容嗅探：首个非空行 `:` → Intel HEX；`S` → S-record
fn sniff_format(text: &str) -> Result<HexFormat> {
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        return match l.chars().next() {
            Some(':') => Ok(HexFormat::IntelHex),
            Some('S') | Some('s') => Ok(HexFormat::SRecord),
            _ => Err(ProgError::HexFile(format!(
                "unrecognized file format (line: {:.20})",
                l
            ))),
        };
    }
    Err(ProgError::HexFile("empty file".into()))
}

fn parse_intel_hex(text: &str) -> Result<Vec<(u32, u8)>> {
    let reader = ihex::Reader::new(text);
    let mut sparse = Vec::new();
    for record in reader {
        let record = record.map_err(|e| ProgError::HexFile(format!("ihex parse: {e}")))?;
        if let ihex::Record::Data { offset, value } = record {
            sparse.extend(
                value.iter().enumerate().map(|(i, b)| (offset as u32 + i as u32, *b)),
            );
        }
    }
    if sparse.is_empty() {
        return Err(ProgError::HexFile("no data records".into()));
    }
    Ok(sparse)
}

/// Motorola S-record：S1(16bit)/S2(24bit)/S3(32bit) 数据记录；
/// S0=头 S5=计数 S7/S8/S9=结束（忽略）
fn parse_srecord(text: &str) -> Result<Vec<(u32, u8)>> {
    let mut sparse = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let bytes = (2..l.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(l.get(i..i + 2).unwrap_or(""), 16)
                    .map_err(|_| ProgError::HexFile(format!("srec line {}: bad hex", lineno + 1)))
            })
            .collect::<Result<Vec<u8>>>()?;
        if bytes.len() < 2 {
            return Err(ProgError::HexFile(format!("srec line {}: too short", lineno + 1)));
        }
        // 校验和：count+addr+data+checksum 之和低 8 位 = 0xFF
        if bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0xFF {
            return Err(ProgError::HexFile(format!(
                "srec line {}: checksum error",
                lineno + 1
            )));
        }
        let count = bytes[0] as usize;
        if count + 1 != bytes.len() {
            return Err(ProgError::HexFile(format!("srec line {}: bad count", lineno + 1)));
        }
        let ty = l.as_bytes()[1];
        let addr_len = match ty {
            b'0' => continue,             // 头记录
            b'1' => 2,
            b'2' => 3,
            b'3' => 4,
            b'5' | b'7' | b'8' | b'9' => continue, // 计数/结束
            _ => {
                return Err(ProgError::HexFile(format!(
                    "srec line {}: unknown record S{}",
                    lineno + 1,
                    ty as char
                )))
            }
        };
        let data_len = count - addr_len - 1;
        let mut addr = 0u32;
        for b in &bytes[1..1 + addr_len] {
            addr = (addr << 8) | *b as u32;
        }
        sparse.extend(
            bytes[1 + addr_len..1 + addr_len + data_len]
                .iter()
                .enumerate()
                .map(|(i, b)| (addr + i as u32, *b)),
        );
    }
    if sparse.is_empty() {
        return Err(ProgError::HexFile("no S-record data".into()));
    }
    Ok(sparse)
}

/// 清洗：过滤 + 合并连续区间（空洞不填充）
fn wash(mut sparse: Vec<(u32, u8)>, chip: &ChipDef, format: HexFormat) -> Result<HexImage> {
    sparse.sort_unstable();

    let mut warnings = Vec::new();
    let mut nvr_dropped = 0usize;
    let mut secure_dropped = 0usize;
    let mut below_low = false;
    let mut out_of_range = 0usize;

    sparse.retain(|&(addr, _)| {
        if addr < FLASH_LOW {
            below_low = true; // 越界即硬错误（retain 后统一报错）
            return false;
        }
        if (NVR_LOW..=NVR_HIGH).contains(&addr) {
            nvr_dropped += 1;
            return false;
        }
        if addr == SECURE_ADDR {
            secure_dropped += 1;
            return false;
        }
        if !chip.is_valid_addr(addr) {
            out_of_range += 1;
            return false;
        }
        true
    });
    if below_low {
        return Err(ProgError::HexFile(format!(
            "data below 0x{FLASH_LOW:04X}（越界硬错误）"
        )));
    }
    if nvr_dropped > 0 {
        warnings.push(format!(
            "剔除 NVR 区 0xFFBB~0xFFBF 共 {nvr_dropped} 字节（保护 CR 校准值）"
        ));
    }
    if secure_dropped > 0 {
        warnings.push("剔除 0xFFFC 安全位（由“写入安全位”选项处理）".into());
    }
    if out_of_range > 0 {
        warnings.push(format!(
            "剔除超出 {} Flash 映射（{}）共 {out_of_range} 字节，请确认型号选择",
            chip.name,
            chip.ranges_desc()
        ));
    }
    if sparse.is_empty() {
        return Err(ProgError::HexFile(
            "no burnable data after filtering (all in NVR/secure/out-of-range)".into(),
        ));
    }

    let mut segments: Vec<(u32, Vec<u8>)> = Vec::new();
    for (addr, byte) in sparse {
        match segments.last_mut() {
            Some((start, data)) if addr == *start + data.len() as u32 => data.push(byte),
            _ => segments.push((addr, vec![byte])),
        }
    }

    Ok(HexImage { segments, warnings, format })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chipdef::{by_name, default_chip};

    fn hex_line(addr: u16, rectype: u8, data: &[u8]) -> String {
        let mut bytes: Vec<u8> = vec![data.len() as u8, (addr >> 8) as u8, addr as u8, rectype];
        bytes.extend_from_slice(data);
        let sum: u8 = bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        let chk = (!sum).wrapping_add(1);
        let mut s = String::from(":");
        for b in bytes {
            s.push_str(&format!("{b:02X}"));
        }
        s.push_str(&format!("{chk:02X}\n"));
        s
    }

    /// 生成 S1 记录（16 位地址）
    fn s1_line(addr: u16, data: &[u8]) -> String {
        let count = (data.len() + 3) as u8;
        let mut bytes: Vec<u8> = vec![count, (addr >> 8) as u8, addr as u8];
        bytes.extend_from_slice(data);
        let sum: u8 = bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        let chk = !sum;
        let mut s = String::from("S1");
        for b in bytes {
            s.push_str(&format!("{b:02X}"));
        }
        s.push_str(&format!("{chk:02X}\n"));
        s
    }

    #[test]
    fn parse_simple() {
        let chip = default_chip();
        let text = hex_line(0x8000, 0, &[1, 2, 3]) + &hex_line(0, 1, &[]);
        let img = parse(&text, chip).unwrap();
        assert_eq!(img.segments, vec![(0x8000, vec![1, 2, 3])]);
        assert!(img.warnings.is_empty());
        assert_eq!(img.format, HexFormat::IntelHex);
    }

    #[test]
    fn strips_nvr_and_secure() {
        let chip = default_chip();
        let text = hex_line(0x8000, 0, &[0xAA])
            + &hex_line(0xFFBB, 0, &[1, 2, 3])
            + &hex_line(0xFFFC, 0, &[0x01])
            + &hex_line(0, 1, &[]);
        let img = parse(&text, chip).unwrap();
        assert_eq!(img.segments, vec![(0x8000, vec![0xAA])]);
        assert_eq!(img.warnings.len(), 2);
        assert!(img.warnings[0].contains("NVR"));
        assert!(img.warnings[1].contains("0xFFFC"));
    }

    #[test]
    fn below_0x1000_is_hard_error() {
        let chip = default_chip();
        let text = hex_line(0x0800, 0, &[1]) + &hex_line(0, 1, &[]);
        assert!(parse(&text, chip).is_err());
    }

    #[test]
    fn chip_mismatch_warning() {
        let chip8k = by_name("MB95F632H").unwrap(); // 上 bank 0xF000
        // 0x9000 越界（空洞区）剔除+告警；0x1000 下 bank 有效；0xF000 上 bank 有效
        let text = hex_line(0x9000, 0, &[1, 2])
            + &hex_line(0x1000, 0, &[9])
            + &hex_line(0xF000, 0, &[3])
            + &hex_line(0, 1, &[]);
        let img = parse(&text, chip8k).unwrap();
        assert_eq!(img.segments, vec![(0x1000, vec![9]), (0xF000, vec![3])]);
        assert!(img.warnings.iter().any(|w| w.contains("型号")));
    }

    #[test]
    fn holes_not_filled() {
        let chip = default_chip();
        let text = hex_line(0x8000, 0, &[1]) + &hex_line(0x8010, 0, &[2]) + &hex_line(0, 1, &[]);
        let img = parse(&text, chip).unwrap();
        assert_eq!(img.segments.len(), 2);
    }

    #[test]
    fn parse_srecord_s1() {
        let chip = default_chip();
        let text = String::from("S0030000FC\n")
            + &s1_line(0x8000, &[0xDE, 0xAD, 0xBE, 0xEF])
            + "S9030000FC\n";
        let img = parse(&text, chip).unwrap();
        assert_eq!(img.format, HexFormat::SRecord);
        assert_eq!(img.segments, vec![(0x8000, vec![0xDE, 0xAD, 0xBE, 0xEF])]);
    }

    #[test]
    fn srecord_checksum_error_detected() {
        let chip = default_chip();
        let mut bad = s1_line(0x8000, &[1, 2, 3]);
        bad.replace_range(bad.len() - 3..bad.len() - 1, "00");
        assert!(parse(&bad, chip).is_err());
    }
}
