#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupPacket {
    pub const fn new(
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
    ) -> Self {
        Self {
            request_type,
            request,
            value,
            index,
            length,
        }
    }

    pub const fn direction_in(self) -> bool {
        (self.request_type & request_type::DIRECTION_IN) != 0
    }

    pub const fn to_bytes(self) -> [u8; 8] {
        [
            self.request_type,
            self.request,
            self.value as u8,
            (self.value >> 8) as u8,
            self.index as u8,
            (self.index >> 8) as u8,
            self.length as u8,
            (self.length >> 8) as u8,
        ]
    }

    pub const fn to_u64(self) -> u64 {
        u64::from_le_bytes(self.to_bytes())
    }

    pub const fn get_descriptor(descriptor_type: u8, descriptor_index: u8, length: u16) -> Self {
        Self::new(
            request_type::DIRECTION_IN | request_type::TYPE_STANDARD | request_type::RECIPIENT_DEVICE,
            request::GET_DESCRIPTOR,
            ((descriptor_type as u16) << 8) | descriptor_index as u16,
            0,
            length,
        )
    }
}

pub mod request_type {
    pub const DIRECTION_IN: u8 = 1 << 7;
    pub const TYPE_STANDARD: u8 = 0x00;
    pub const RECIPIENT_DEVICE: u8 = 0x00;
}

pub mod request {
    pub const GET_DESCRIPTOR: u8 = 6;
}

pub mod descriptor_type {
    pub const DEVICE: u8 = 1;
    pub const CONFIGURATION: u8 = 2;
}

#[cfg(test)]
mod tests {
    use super::{SetupPacket, descriptor_type};

    #[test_case]
    fn test_setup_packet_serialization() {
        let packet = SetupPacket::get_descriptor(descriptor_type::DEVICE, 0, 18);
        assert_eq!(packet.to_bytes(), [0x80, 6, 0, 1, 0, 0, 18, 0]);
        assert_eq!(packet.to_u64(), 0x0012_0000_0100_0680);
        assert!(packet.direction_in());
    }
}
