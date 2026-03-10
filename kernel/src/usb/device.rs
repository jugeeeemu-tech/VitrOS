use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsbDeviceHandle(u64);

impl UsbDeviceHandle {
    pub(crate) fn allocate() -> Self {
        static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbSpeed {
    Low,
    Full,
    High,
    Super,
    SuperPlus,
}

impl UsbSpeed {
    pub const fn from_port_speed_id(speed_id: u8) -> Option<Self> {
        match speed_id {
            1 => Some(Self::Full),
            2 => Some(Self::Low),
            3 => Some(Self::High),
            4 => Some(Self::Super),
            5 => Some(Self::SuperPlus),
            _ => None,
        }
    }

    pub const fn port_speed_id(self) -> u8 {
        match self {
            Self::Full => 1,
            Self::Low => 2,
            Self::High => 3,
            Self::Super => 4,
            Self::SuperPlus => 5,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Full => "Full",
            Self::High => "High",
            Self::Super => "Super",
            Self::SuperPlus => "SuperSpeedPlus",
        }
    }

    pub const fn default_ep0_max_packet_size(self) -> u16 {
        match self {
            Self::Low | Self::Full => 8,
            Self::High => 64,
            Self::Super | Self::SuperPlus => 512,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbEndpointInfo {
    pub address: u8,
    pub attributes: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInterfaceInfo {
    pub number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<UsbEndpointInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbConfigurationInfo {
    pub configuration_value: u8,
    pub attributes: u8,
    pub max_power: u8,
    pub interfaces: Vec<UsbInterfaceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDeviceInfo {
    pub handle: UsbDeviceHandle,
    pub port_id: u8,
    pub address: u8,
    pub speed: UsbSpeed,
    pub vendor_id: u16,
    pub product_id: u16,
    pub configurations: Vec<UsbConfigurationInfo>,
}

#[cfg(test)]
mod tests {
    use super::UsbSpeed;

    #[test_case]
    fn test_usb_speed_mapping_from_portsc_id() {
        assert_eq!(UsbSpeed::from_port_speed_id(0), None);
        assert_eq!(UsbSpeed::from_port_speed_id(1), Some(UsbSpeed::Full));
        assert_eq!(UsbSpeed::from_port_speed_id(2), Some(UsbSpeed::Low));
        assert_eq!(UsbSpeed::from_port_speed_id(3), Some(UsbSpeed::High));
        assert_eq!(UsbSpeed::from_port_speed_id(4), Some(UsbSpeed::Super));
        assert_eq!(UsbSpeed::from_port_speed_id(5), Some(UsbSpeed::SuperPlus));
    }

    #[test_case]
    fn test_usb_speed_default_ep0_packet_size() {
        assert_eq!(UsbSpeed::Low.default_ep0_max_packet_size(), 8);
        assert_eq!(UsbSpeed::Full.default_ep0_max_packet_size(), 8);
        assert_eq!(UsbSpeed::High.default_ep0_max_packet_size(), 64);
        assert_eq!(UsbSpeed::Super.default_ep0_max_packet_size(), 512);
        assert_eq!(UsbSpeed::SuperPlus.default_ep0_max_packet_size(), 512);
    }
}
