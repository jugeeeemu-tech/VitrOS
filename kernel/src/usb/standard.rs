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
            request_type::DIRECTION_IN
                | request_type::TYPE_STANDARD
                | request_type::RECIPIENT_DEVICE,
            request::GET_DESCRIPTOR,
            ((descriptor_type as u16) << 8) | descriptor_index as u16,
            0,
            length,
        )
    }

    pub const fn set_configuration(configuration_value: u8) -> Self {
        Self::new(
            request_type::DIRECTION_OUT
                | request_type::TYPE_STANDARD
                | request_type::RECIPIENT_DEVICE,
            request::SET_CONFIGURATION,
            configuration_value as u16,
            0,
            0,
        )
    }

    pub const fn set_protocol(interface_number: u8, protocol: u16) -> Self {
        Self::new(
            request_type::DIRECTION_OUT
                | request_type::TYPE_CLASS
                | request_type::RECIPIENT_INTERFACE,
            request::SET_PROTOCOL,
            protocol,
            interface_number as u16,
            0,
        )
    }
}

pub mod request_type {
    pub const DIRECTION_OUT: u8 = 0;
    pub const DIRECTION_IN: u8 = 1 << 7;
    pub const TYPE_STANDARD: u8 = 0x00;
    pub const TYPE_CLASS: u8 = 0x20;
    pub const RECIPIENT_DEVICE: u8 = 0x00;
    pub const RECIPIENT_INTERFACE: u8 = 0x01;
}

pub mod request {
    pub const GET_DESCRIPTOR: u8 = 6;
    pub const SET_CONFIGURATION: u8 = 9;
    pub const SET_PROTOCOL: u8 = 0x0B;
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

    #[test_case]
    fn test_set_configuration_packet_serialization() {
        let packet = SetupPacket::set_configuration(1);
        assert_eq!(packet.to_bytes(), [0x00, 9, 1, 0, 0, 0, 0, 0]);
        assert!(!packet.direction_in());
    }

    #[test_case]
    fn test_set_protocol_packet_serialization() {
        let packet = SetupPacket::set_protocol(2, 0);
        assert_eq!(packet.to_bytes(), [0x21, 0x0B, 0, 0, 2, 0, 0, 0]);
        assert!(!packet.direction_in());
    }
}
