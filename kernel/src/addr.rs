//! アドレス操作ユーティリティ
//!
//! メモリアドレスに関する共通的な操作を提供します。

use crate::paging::{PagingError, phys_to_virt};

/// アドレス操作のエラー型
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    /// nullアドレス
    Null,
    /// アラインメント不正
    Unaligned,
    /// 範囲外
    OutOfRange,
    /// 変換失敗
    ConversionFailed,
}

impl core::fmt::Display for AddressError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            AddressError::Null => write!(f, "Null address"),
            AddressError::Unaligned => write!(f, "Unaligned address"),
            AddressError::OutOfRange => write!(f, "Address out of range"),
            AddressError::ConversionFailed => write!(f, "Address conversion failed"),
        }
    }
}

impl From<PagingError> for AddressError {
    fn from(err: PagingError) -> Self {
        match err {
            PagingError::InvalidAddress => AddressError::Null,
            PagingError::AddressConversionFailed => AddressError::ConversionFailed,
            _ => AddressError::ConversionFailed,
        }
    }
}

/// アドレスが指定したアラインメントに揃っているか確認
///
/// # Arguments
/// * `addr` - チェックするアドレス
/// * `align` - 要求されるアラインメント（2の累乗である必要がある）
///
/// # Returns
/// アラインメントに揃っている場合はtrue
///
/// # Examples
/// ```
/// assert!(is_aligned(0x1000, 0x1000)); // 4KB境界
/// assert!(is_aligned(0x2000, 0x1000)); // 4KB境界
/// assert!(!is_aligned(0x1001, 0x1000)); // 4KB境界ではない
/// ```
#[allow(dead_code)]
pub fn is_aligned(addr: u64, align: u64) -> bool {
    debug_assert!(align.is_power_of_two(), "Alignment must be a power of 2");
    addr % align == 0
}

/// アドレスを指定したアラインメントに切り上げ
///
/// # Arguments
/// * `addr` - 切り上げるアドレス
/// * `align` - アラインメント（2の累乗である必要がある）
///
/// # Returns
/// アラインメントに揃えられたアドレス
#[allow(dead_code)]
pub fn align_up(addr: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two(), "Alignment must be a power of 2");
    (addr + align - 1) & !(align - 1)
}

/// アドレスを指定したアラインメントに切り下げ
///
/// # Arguments
/// * `addr` - 切り下げるアドレス
/// * `align` - アラインメント（2の累乗である必要がある）
///
/// # Returns
/// アラインメントに揃えられたアドレス
#[allow(dead_code)]
pub fn align_down(addr: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two(), "Alignment must be a power of 2");
    addr & !(align - 1)
}

/// 物理アドレスを仮想アドレスに変換し、型安全な参照として取得
///
/// # Safety
/// 呼び出し元は、指定されたアドレスが有効で、型Tとして解釈可能であることを保証する必要があります。
///
/// # Arguments
/// * `phys_addr` - 物理アドレス
///
/// # Returns
/// 型Tへの不変参照、またはエラー
///
/// # Errors
/// * `AddressError::Null` - アドレスが0の場合
/// * `AddressError::ConversionFailed` - アドレス変換に失敗した場合
#[allow(dead_code)]
pub unsafe fn phys_to_ref<T>(phys_addr: u64) -> Result<&'static T, AddressError> {
    if phys_addr == 0 {
        return Err(AddressError::Null);
    }
    let virt_addr = phys_to_virt(phys_addr)?;
    // SAFETY: 呼び出し元がアドレスの有効性を保証する
    Ok(unsafe { &*(virt_addr as *const T) })
}

/// 物理アドレスを仮想アドレスに変換し、型安全な可変参照として取得
///
/// # Safety
/// 呼び出し元は、指定されたアドレスが有効で、型Tとして解釈可能であり、
/// 他の参照と競合しないことを保証する必要があります。
///
/// # Arguments
/// * `phys_addr` - 物理アドレス
///
/// # Returns
/// 型Tへの可変参照、またはエラー
///
/// # Errors
/// * `AddressError::Null` - アドレスが0の場合
/// * `AddressError::ConversionFailed` - アドレス変換に失敗した場合
#[allow(dead_code)]
pub unsafe fn phys_to_mut<T>(phys_addr: u64) -> Result<&'static mut T, AddressError> {
    if phys_addr == 0 {
        return Err(AddressError::Null);
    }
    let virt_addr = phys_to_virt(phys_addr)?;
    // SAFETY: 呼び出し元がアドレスの有効性と排他的アクセスを保証する
    Ok(unsafe { &mut *(virt_addr as *mut T) })
}

/// 仮想アドレスから型安全な参照を取得（アラインメントチェック付き）
///
/// # Safety
/// 呼び出し元は、指定されたアドレスが有効で、型Tとして解釈可能であることを保証する必要があります。
///
/// # Arguments
/// * `virt_addr` - 仮想アドレス
///
/// # Returns
/// 型Tへの不変参照、またはエラー
///
/// # Errors
/// * `AddressError::Null` - アドレスが0の場合
/// * `AddressError::Unaligned` - アラインメントが不正な場合
#[allow(dead_code)]
pub unsafe fn virt_to_ref<T>(virt_addr: u64) -> Result<&'static T, AddressError> {
    if virt_addr == 0 {
        return Err(AddressError::Null);
    }
    let align = core::mem::align_of::<T>() as u64;
    if !is_aligned(virt_addr, align) {
        return Err(AddressError::Unaligned);
    }
    // SAFETY: 呼び出し元がアドレスの有効性を保証する
    Ok(unsafe { &*(virt_addr as *const T) })
}

/// 仮想アドレスから型安全な可変参照を取得（アラインメントチェック付き）
///
/// # Safety
/// 呼び出し元は、指定されたアドレスが有効で、型Tとして解釈可能であり、
/// 他の参照と競合しないことを保証する必要があります。
///
/// # Arguments
/// * `virt_addr` - 仮想アドレス
///
/// # Returns
/// 型Tへの可変参照、またはエラー
///
/// # Errors
/// * `AddressError::Null` - アドレスが0の場合
/// * `AddressError::Unaligned` - アラインメントが不正な場合
#[allow(dead_code)]
pub unsafe fn virt_to_mut<T>(virt_addr: u64) -> Result<&'static mut T, AddressError> {
    if virt_addr == 0 {
        return Err(AddressError::Null);
    }
    let align = core::mem::align_of::<T>() as u64;
    if !is_aligned(virt_addr, align) {
        return Err(AddressError::Unaligned);
    }
    // SAFETY: 呼び出し元がアドレスの有効性と排他的アクセスを保証する
    Ok(unsafe { &mut *(virt_addr as *mut T) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_is_aligned() {
        assert!(is_aligned(0x1000, 0x1000));
        assert!(is_aligned(0x2000, 0x1000));
        assert!(!is_aligned(0x1001, 0x1000));
        assert!(is_aligned(0, 0x1000));
    }

    #[test_case]
    fn test_align_up() {
        assert_eq!(align_up(0x1000, 0x1000), 0x1000);
        assert_eq!(align_up(0x1001, 0x1000), 0x2000);
        assert_eq!(align_up(0x1FFF, 0x1000), 0x2000);
        assert_eq!(align_up(0, 0x1000), 0);
    }

    #[test_case]
    fn test_align_down() {
        assert_eq!(align_down(0x1000, 0x1000), 0x1000);
        assert_eq!(align_down(0x1001, 0x1000), 0x1000);
        assert_eq!(align_down(0x1FFF, 0x1000), 0x1000);
        assert_eq!(align_down(0, 0x1000), 0);
    }

    #[test_case]
    fn test_is_aligned_edge_cases() {
        // 最小アラインメント(1)は常に成功
        assert!(is_aligned(0, 1));
        assert!(is_aligned(1, 1));
        assert!(is_aligned(0xFFFF_FFFF_FFFF_FFFF, 1));

        // 大きな値でのテスト
        assert!(is_aligned(0x1_0000_0000, 0x1000)); // 4GB
        assert!(!is_aligned(0x1_0000_0001, 0x1000));
    }

    #[test_case]
    fn test_align_up_edge_cases() {
        // 最小アラインメント(1)
        assert_eq!(align_up(42, 1), 42);

        // 2MBアラインメント
        const ALIGN_2MB: u64 = 0x20_0000;
        assert_eq!(align_up(0, ALIGN_2MB), 0);
        assert_eq!(align_up(1, ALIGN_2MB), ALIGN_2MB);
        assert_eq!(align_up(ALIGN_2MB, ALIGN_2MB), ALIGN_2MB);
        assert_eq!(align_up(ALIGN_2MB + 1, ALIGN_2MB), ALIGN_2MB * 2);
    }

    #[test_case]
    fn test_align_down_edge_cases() {
        // 大きな値のテスト
        assert_eq!(align_down(0x1_0000_1234, 0x1000), 0x1_0000_1000);

        // 2MBアラインメント
        const ALIGN_2MB: u64 = 0x20_0000;
        assert_eq!(align_down(0, ALIGN_2MB), 0);
        assert_eq!(align_down(ALIGN_2MB - 1, ALIGN_2MB), 0);
        assert_eq!(align_down(ALIGN_2MB, ALIGN_2MB), ALIGN_2MB);
        assert_eq!(align_down(ALIGN_2MB + 1, ALIGN_2MB), ALIGN_2MB);
    }
}
