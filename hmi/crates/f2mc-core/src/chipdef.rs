//! F2MC-8FX 型号库
//!
//! 数据源：`data/896.csv`（references/896.csv，富士通 8FX 器件表，family 列 = FMC8FX）。
//! Flash 布局（MB95630H-HARDWARE-MANUAL §26.1）：双 bank，
//! **下 bank 恒为 0x1000~0x1FFF（2KB×2）**，上 bank 起始地址由 CSV 第 3 列给出
//! （如 MB95F636H：0x8000~0xFFFF 32KB + 下 bank 4KB = 36KB）。

use std::sync::OnceLock;

/// 可烧录地址下限（所有型号一致）
pub const FLASH_LOW: u32 = 0x1000;
/// 下 bank 范围（所有 8FX 一致）
pub const LOWER_BANK_LOW: u32 = 0x1000;
pub const LOWER_BANK_HIGH: u32 = 0x1FFF;
/// 地址空间上限
pub const FLASH_HIGH: u32 = 0xFFFF;

/// F2MC-8FX 器件定义（Flash 布局 + 器件属性）
#[derive(Debug, Clone)]
pub struct ChipDef {
    /// 型号名，如 "MB95F636H"
    pub name: String,
    /// 上 bank 起始地址（CSV 第 3 列）
    pub upper_low: u32,
    /// Flash 总容量 = 下 bank 4KB + 上 bank
    pub flash_bytes: u32,
    /// 非 MB95F 前缀（FRAM/掩膜器件，New8FX 串行编程不适用，仅列出提示）
    pub fram: bool,
}

impl ChipDef {
    /// 地址是否落在可烧录 Flash 映射内（下 bank 或上 bank）
    ///
    /// @param addr 目标地址
    /// @returns 在双 bank 映射内为 true，空洞区为 false
    pub fn is_valid_addr(&self, addr: u32) -> bool {
        (LOWER_BANK_LOW..=LOWER_BANK_HIGH).contains(&addr) || addr >= self.upper_low
    }

    /// 有效区间描述（GUI 显示，如 "0x1000~0x1FFF + 0x8000~0xFFFF"）
    ///
    /// 上 bank 起点落在下 bank 内（如 MB95F698K 的 0x1000 连续映射）时，
    /// 上 bank 段从下 bank 之后起显示（"0x1000~0x1FFF + 0x2000~0xFFFF"）。
    pub fn ranges_desc(&self) -> String {
        let upper_show = self.upper_low.max(LOWER_BANK_HIGH + 1);
        format!(
            "0x{LOWER_BANK_LOW:04X}~0x{LOWER_BANK_HIGH:04X} + 0x{upper_show:04X}~0x{FLASH_HIGH:04X}"
        )
    }
}

static CHIPS: OnceLock<Vec<ChipDef>> = OnceLock::new();

/// 全部 FMC8FX 型号（按名称排序）
pub fn chips() -> &'static [ChipDef] {
    CHIPS.get_or_init(load_chips)
}

fn load_chips() -> Vec<ChipDef> {
    let csv = include_str!("../data/896.csv");
    let mut out = Vec::new();
    for line in csv.lines() {
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 3 || cols[1] != "FMC8FX" || cols[0].is_empty() {
            continue;
        }
        let name = cols[0].to_string();
        let upper_low = match parse_range_low(cols[2]) {
            Some(v) => v,
            None => continue,
        };
        // 上 bank 起点 ≤ 下 bank 起点（如 MB95F698K 的 0x1000:0xFFFF 连续映射）时
        // 整片为单一区域，不再重复计入下 bank
        let flash_bytes = if upper_low <= LOWER_BANK_LOW {
            0x10000 - LOWER_BANK_LOW
        } else {
            (LOWER_BANK_HIGH - LOWER_BANK_LOW + 1) + (0x10000 - upper_low)
        };
        let fram = !name.starts_with("MB95F");
        out.push(ChipDef { name, upper_low, flash_bytes, fram });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// "0x8000:0xFFFF" → 0x8000
fn parse_range_low(s: &str) -> Option<u32> {
    let low = s.split(':').next()?;
    u32::from_str_radix(low.trim().trim_start_matches("0x"), 16).ok()
}

/// 关键词过滤（大小写不敏感，子串匹配）
pub fn search(keyword: &str) -> Vec<&'static ChipDef> {
    let kw = keyword.trim().to_uppercase();
    chips()
        .iter()
        .filter(|c| kw.is_empty() || c.name.to_uppercase().contains(&kw))
        .collect()
}

/// 按名称精确查找型号
///
/// @param name 型号名，如 "MB95F636H"
pub fn by_name(name: &str) -> Option<&'static ChipDef> {
    chips().iter().find(|c| c.name == name)
}

/// 默认型号（MB95F636H；找不到时回退到包含 "F636" 的型号，再回退首项）
pub fn default_chip() -> &'static ChipDef {
    by_name("MB95F636H")
        .or_else(|| chips().iter().find(|c| c.name.contains("F636")))
        .unwrap_or(&chips()[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_loaded() {
        assert!(chips().len() > 200, "expect 216 FMC8FX, got {}", chips().len());
        // 有序
        let names: Vec<_> = chips().iter().map(|c| &c.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn f630_family_layout() {
        let f636 = by_name("MB95F636H").unwrap();
        assert_eq!(f636.upper_low, 0x8000);
        assert_eq!(f636.flash_bytes, 36 * 1024);
        // 上 bank 与下 bank 有效，空洞无效
        assert!(f636.is_valid_addr(0x8000));
        assert!(f636.is_valid_addr(0x1000));
        assert!(!f636.is_valid_addr(0x7000));

        let f632 = by_name("MB95F632H").unwrap();
        assert_eq!(f632.upper_low, 0xF000);
        assert_eq!(f632.flash_bytes, 8 * 1024);
    }

    #[test]
    fn f698_contiguous_map() {
        // MB95F698K：CSV 为 0x1000:0xFFFF 单一连续映射（60KB），无空洞
        let f698 = by_name("MB95F698K").unwrap();
        assert_eq!(f698.upper_low, 0x1000);
        assert_eq!(f698.flash_bytes, 60 * 1024);
        assert!(f698.is_valid_addr(0x1000));
        assert!(f698.is_valid_addr(0x5000)); // F636K 的空洞区在 F698K 有效
        assert!(!f698.is_valid_addr(0x0FFF));
        // 连续映射显示：上 bank 段从下 bank 之后起，不重叠
        assert_eq!(f698.ranges_desc(), "0x1000~0x1FFF + 0x2000~0xFFFF");
        let f636 = by_name("MB95F636H").unwrap();
        assert_eq!(f636.ranges_desc(), "0x1000~0x1FFF + 0x8000~0xFFFF");
    }

    #[test]
    fn search_works() {
        let r = search("F63");
        assert!(r.len() >= 8); // F632H/K, F633H/K, F634H/K, F636H/K 等
        assert!(r.iter().any(|c| c.name == "MB95F636H"));
        assert!(search("").len() == chips().len());
    }
}
