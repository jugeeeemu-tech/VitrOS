//! xHCI register definitions (xHCI 1.1+)
//!
//! This module only provides register layout/types, offsets, and bit helpers.
//! MMIO read/write accessors are intentionally out of scope for Issue #31.

/// Capability Registers (read-only), located at MMIO base + 0x00.
#[repr(C)]
pub struct CapabilityRegisters {
    pub caplength: u8,
    _reserved0: u8,
    pub hciversion: u16,
    pub hcsparams1: u32,
    pub hcsparams2: u32,
    pub hcsparams3: u32,
    pub hccparams1: u32,
    pub dboff: u32,
    pub rtsoff: u32,
    pub hccparams2: u32,
}

/// Port Register Set (16 bytes per port).
#[repr(C)]
pub struct PortRegisterSet {
    pub portsc: u32,
    pub portpmsc: u32,
    pub portli: u32,
    pub porthlpmc: u32,
}

/// Operational Registers, located at MMIO base + CAPLENGTH.
#[repr(C)]
pub struct OperationalRegisters {
    pub usbcmd: u32,
    pub usbsts: u32,
    pub pagesize: u32,
    _reserved0: [u32; 2],
    pub dnctrl: u32,
    pub crcr: u64,
    _reserved1: [u32; 4],
    pub dcbaap: u64,
    pub config: u32,
    _reserved2: [u32; 241],
    pub ports: [PortRegisterSet; 256],
}

/// Interrupter Register Set (32 bytes).
#[repr(C)]
pub struct InterrupterRegisterSet {
    pub iman: u32,
    pub imod: u32,
    pub erstsz: u32,
    _reserved0: u32,
    pub erstba: u64,
    pub erdp: u64,
}

/// Runtime Registers, located at MMIO base + (RTSOFF & !0x1f).
#[repr(C)]
pub struct RuntimeRegisters {
    pub mfindex: u32,
    _reserved0: [u32; 7],
    pub ir: [InterrupterRegisterSet; 1024],
}

/// Doorbell Register (4 bytes each), located at MMIO base + (DBOFF & !0x3).
#[repr(C)]
pub struct DoorbellRegister {
    pub db: u32,
}

/// Offset calculation helpers derived from Capability registers.
pub mod offsets {
    /// Convert RTSOFF register value to runtime register space offset.
    #[inline]
    pub const fn runtime_offset(rtsoff: u32) -> u64 {
        (rtsoff & !0x1f) as u64
    }

    /// Convert DBOFF register value to doorbell register space offset.
    #[inline]
    pub const fn doorbell_offset(dboff: u32) -> u64 {
        (dboff & !0x3) as u64
    }
}

/// USBCMD bit definitions.
pub mod usbcmd {
    pub const RUN_STOP: u32 = 1 << 0;
    pub const HCRST: u32 = 1 << 1;
    pub const INTE: u32 = 1 << 2;
    pub const HSEE: u32 = 1 << 3;
    pub const LHCRST: u32 = 1 << 7;
    pub const CSS: u32 = 1 << 8;
    pub const CRS: u32 = 1 << 9;
    pub const EWE: u32 = 1 << 10;
}

/// USBSTS bit definitions.
pub mod usbsts {
    pub const HCH: u32 = 1 << 0;
    pub const HSE: u32 = 1 << 2;
    pub const EINT: u32 = 1 << 3;
    pub const PCD: u32 = 1 << 4;
    pub const SSS: u32 = 1 << 8;
    pub const RSS: u32 = 1 << 9;
    pub const SRE: u32 = 1 << 10;
    pub const CNR: u32 = 1 << 11;
    pub const HCE: u32 = 1 << 12;
}

/// PORTSC bit definitions and field helpers.
pub mod portsc {
    pub const CCS: u32 = 1 << 0;
    pub const PED: u32 = 1 << 1;
    pub const OCA: u32 = 1 << 3;
    pub const PR: u32 = 1 << 4;
    pub const PP: u32 = 1 << 9;
    pub const CSC: u32 = 1 << 17;
    pub const PEC: u32 = 1 << 18;
    pub const WRC: u32 = 1 << 19;
    pub const OCC: u32 = 1 << 20;
    pub const PRC: u32 = 1 << 21;
    pub const PLC: u32 = 1 << 22;
    pub const CEC: u32 = 1 << 23;
    pub const WPR: u32 = 1 << 31;

    /// Extract protocol speed ID from bits 13:10.
    #[inline]
    pub const fn port_speed(portsc: u32) -> u8 {
        ((portsc >> 10) & 0x0f) as u8
    }

    /// Extract port link state from bits 8:5.
    #[inline]
    pub const fn port_link_state(portsc: u32) -> u8 {
        ((portsc >> 5) & 0x0f) as u8
    }
}

/// IMAN bit definitions.
pub mod iman {
    pub const IP: u32 = 1 << 0;
    pub const IE: u32 = 1 << 1;
}

/// ERDP bit definitions.
pub mod erdp {
    pub const EHB: u64 = 1 << 3;
}

/// HCSPARAMS1 field helpers.
pub mod hcsparams1 {
    #[inline]
    pub const fn max_slots(v: u32) -> u8 {
        (v & 0xff) as u8
    }

    #[inline]
    pub const fn max_intrs(v: u32) -> u16 {
        ((v >> 8) & 0x07ff) as u16
    }

    #[inline]
    pub const fn max_ports(v: u32) -> u8 {
        ((v >> 24) & 0xff) as u8
    }
}

/// HCSPARAMS2 field helpers.
pub mod hcsparams2 {
    /// Max Scratchpad Buffers:
    /// ((HCSPARAMS2 >> 16) & 0x3e0) | ((HCSPARAMS2 >> 27) & 0x1f)
    #[inline]
    pub const fn max_scratchpad_buffers(v: u32) -> usize {
        (((v >> 16) & 0x3e0) | ((v >> 27) & 0x1f)) as usize
    }
}

/// HCCPARAMS1 field helpers.
pub mod hccparams1 {
    #[inline]
    pub const fn ac64(v: u32) -> bool {
        (v & 1) != 0
    }

    /// Context Size bit (bit 2): false = 32B, true = 64B.
    #[inline]
    pub const fn csz(v: u32) -> bool {
        ((v >> 2) & 1) != 0
    }

    #[inline]
    pub const fn context_size(v: u32) -> usize {
        if csz(v) { 64 } else { 32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test_case]
    fn test_offset_masks() {
        assert_eq!(offsets::runtime_offset(0x1234_567f), 0x1234_5660);
        assert_eq!(offsets::doorbell_offset(0x89ab_cdef), 0x89ab_cdec);
    }

    #[test_case]
    fn test_hcsparams1_extractors() {
        let v = (0x56u32 << 24) | (0x3abu32 << 8) | 0x34;
        assert_eq!(hcsparams1::max_slots(v), 0x34);
        assert_eq!(hcsparams1::max_intrs(v), 0x3ab);
        assert_eq!(hcsparams1::max_ports(v), 0x56);
    }

    #[test_case]
    fn test_hcsparams2_scratchpad_count() {
        let hi = 0x15u32;
        let lo = 0x0bu32;
        let v = (hi << 21) | (lo << 27);
        assert_eq!(hcsparams2::max_scratchpad_buffers(v), ((hi << 5) | lo) as usize);
    }

    #[test_case]
    fn test_hccparams1_context_size() {
        assert!(!hccparams1::ac64(0));
        assert!(!hccparams1::csz(0));
        assert_eq!(hccparams1::context_size(0), 32);

        let v = 0b101;
        assert!(hccparams1::ac64(v));
        assert!(hccparams1::csz(v));
        assert_eq!(hccparams1::context_size(v), 64);
    }

    #[test_case]
    fn test_portsc_field_extractors() {
        let v = (0xau32 << 10) | (0x5u32 << 5);
        assert_eq!(portsc::port_speed(v), 0x0a);
        assert_eq!(portsc::port_link_state(v), 0x05);
    }

    #[test_case]
    fn test_layout_sizes() {
        assert_eq!(size_of::<CapabilityRegisters>(), 0x20);
        assert_eq!(size_of::<PortRegisterSet>(), 0x10);
        assert_eq!(size_of::<InterrupterRegisterSet>(), 0x20);
        assert_eq!(size_of::<RuntimeRegisters>(), 0x8020);
        assert_eq!(size_of::<DoorbellRegister>(), 0x04);
    }
}
