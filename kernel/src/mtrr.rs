//! MTRR (Memory Type Range Registers) 診断モジュール
//!
//! MTRRおよびPATの設定を表示するデバッグ機能を提供します。

use crate::msr;

/// メモリタイプの定義
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum MemoryType {
    Uncacheable = 0,      // UC
    WriteCombining = 1,   // WC
    WriteThrough = 4,     // WT
    WriteProtected = 5,   // WP
    WriteBack = 6,        // WB
    UncacheableMinus = 7, // UC-
    Unknown = 0xFF,
}

impl MemoryType {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => MemoryType::Uncacheable,
            1 => MemoryType::WriteCombining,
            4 => MemoryType::WriteThrough,
            5 => MemoryType::WriteProtected,
            6 => MemoryType::WriteBack,
            7 => MemoryType::UncacheableMinus,
            _ => MemoryType::Unknown,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            MemoryType::Uncacheable => "UC (Uncacheable)",
            MemoryType::WriteCombining => "WC (Write-Combining)",
            MemoryType::WriteThrough => "WT (Write-Through)",
            MemoryType::WriteProtected => "WP (Write-Protected)",
            MemoryType::WriteBack => "WB (Write-Back)",
            MemoryType::UncacheableMinus => "UC- (Uncacheable Minus)",
            MemoryType::Unknown => "Unknown",
        }
    }
}

/// MTRRとPATの情報を表示
pub fn dump() {
    use crate::info;

    info!("=== MTRR Configuration ===");

    unsafe {
        // MTRRCAP: MTRRの機能を確認
        let mtrrcap = msr::read(msr::IA32_MTRRCAP);
        let vcnt = (mtrrcap & 0xFF) as u8; // 可変範囲レジスタの数
        let fix_supported = (mtrrcap >> 8) & 1 != 0;
        let wc_supported = (mtrrcap >> 10) & 1 != 0;

        info!(
            "MTRRCAP: VCNT={}, FIX={}, WC={}",
            vcnt, fix_supported, wc_supported
        );

        // デフォルトメモリタイプ
        let def_type = msr::read(msr::IA32_MTRR_DEF_TYPE);
        let default_type = MemoryType::from_u8((def_type & 0xFF) as u8);
        let mtrr_enabled = (def_type >> 11) & 1 != 0;
        let fixed_enabled = (def_type >> 10) & 1 != 0;

        info!(
            "DEF_TYPE: {} (E={}, FE={})",
            default_type.as_str(),
            mtrr_enabled,
            fixed_enabled
        );

        // 可変範囲MTRR
        info!("Variable Range MTRRs:");
        for i in 0..vcnt.min(8) {
            let base_msr = msr::IA32_MTRR_PHYSBASE0 + (i as u32 * 2);
            let mask_msr = msr::IA32_MTRR_PHYSMASK0 + (i as u32 * 2);

            let base = msr::read(base_msr);
            let mask = msr::read(mask_msr);

            let valid = (mask >> 11) & 1 != 0;
            if valid {
                let mem_type = MemoryType::from_u8((base & 0xFF) as u8);
                let base_addr = base & 0xFFFF_FFFF_FFFF_F000;
                // マスクからサイズを計算
                // マスクの最下位の1ビットがサイズを決定する
                // 例: mask = 0xFF80000000 → 最下位1 = bit31 → サイズ = 2^31 = 2GB
                let mask_bits = mask & 0xFFFF_FFFF_FFFF_F000;
                let size = mask_bits & mask_bits.wrapping_neg(); // x & -x で最下位の1ビットを取得

                info!("  MTRR{}: base=0x{:016X} mask=0x{:016X}", i, base, mask);
                info!(
                    "         0x{:012X} - 0x{:012X} ({}MB) = {}",
                    base_addr,
                    base_addr.wrapping_add(size).wrapping_sub(1),
                    size / (1024 * 1024),
                    mem_type.as_str()
                );
            }
        }

        // PAT (Page Attribute Table)
        let pat = msr::read(msr::IA32_PAT);
        info!("PAT Register: 0x{:016X}", pat);
        info!("PAT Entries:");
        for i in 0..8 {
            let entry = ((pat >> (i * 8)) & 0xFF) as u8;
            let mem_type = MemoryType::from_u8(entry);
            info!("  PAT[{}] = {}", i, mem_type.as_str());
        }
    }
}
