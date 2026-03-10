use alloc::vec::Vec;

use super::device::{UsbConfigurationInfo, UsbEndpointInfo, UsbInterfaceInfo};
use super::standard::descriptor_type;

const DEVICE_DESCRIPTOR_LEN: usize = 18;
const CONFIGURATION_DESCRIPTOR_LEN: usize = 9;
const INTERFACE_DESCRIPTOR_LEN: usize = 9;
const ENDPOINT_DESCRIPTOR_LEN: usize = 7;
const DESCRIPTOR_TYPE_INTERFACE: u8 = 4;
const DESCRIPTOR_TYPE_ENDPOINT: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorParseError {
    BufferTooShort,
    InvalidDescriptorType { expected: u8, actual: u8 },
    InvalidDescriptorLength(u8),
    TruncatedDescriptor { descriptor_type: u8, length: u8 },
    MissingInterfaceDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub usb_version: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub max_packet_size0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_version: u16,
    pub manufacturer_index: u8,
    pub product_index: u8,
    pub serial_number_index: u8,
    pub num_configurations: u8,
}

impl DeviceDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, DescriptorParseError> {
        if bytes.len() < DEVICE_DESCRIPTOR_LEN {
            return Err(DescriptorParseError::BufferTooShort);
        }
        if bytes[0] < DEVICE_DESCRIPTOR_LEN as u8 {
            return Err(DescriptorParseError::InvalidDescriptorLength(bytes[0]));
        }
        if bytes[1] != descriptor_type::DEVICE {
            return Err(DescriptorParseError::InvalidDescriptorType {
                expected: descriptor_type::DEVICE,
                actual: bytes[1],
            });
        }

        Ok(Self {
            usb_version: u16::from_le_bytes([bytes[2], bytes[3]]),
            device_class: bytes[4],
            device_subclass: bytes[5],
            device_protocol: bytes[6],
            max_packet_size0: bytes[7],
            vendor_id: u16::from_le_bytes([bytes[8], bytes[9]]),
            product_id: u16::from_le_bytes([bytes[10], bytes[11]]),
            device_version: u16::from_le_bytes([bytes[12], bytes[13]]),
            manufacturer_index: bytes[14],
            product_index: bytes[15],
            serial_number_index: bytes[16],
            num_configurations: bytes[17],
        })
    }

    pub fn parse_max_packet_size0(bytes: &[u8]) -> Result<u8, DescriptorParseError> {
        if bytes.len() < 8 {
            return Err(DescriptorParseError::BufferTooShort);
        }
        if bytes[0] < 8 {
            return Err(DescriptorParseError::InvalidDescriptorLength(bytes[0]));
        }
        if bytes[1] != descriptor_type::DEVICE {
            return Err(DescriptorParseError::InvalidDescriptorType {
                expected: descriptor_type::DEVICE,
                actual: bytes[1],
            });
        }

        Ok(bytes[7])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigurationDescriptorHeader {
    pub total_length: u16,
    pub num_interfaces: u8,
    pub configuration_value: u8,
    pub attributes: u8,
    pub max_power: u8,
}

impl ConfigurationDescriptorHeader {
    pub fn parse(bytes: &[u8]) -> Result<Self, DescriptorParseError> {
        if bytes.len() < CONFIGURATION_DESCRIPTOR_LEN {
            return Err(DescriptorParseError::BufferTooShort);
        }
        if bytes[0] < CONFIGURATION_DESCRIPTOR_LEN as u8 {
            return Err(DescriptorParseError::InvalidDescriptorLength(bytes[0]));
        }
        if bytes[1] != descriptor_type::CONFIGURATION {
            return Err(DescriptorParseError::InvalidDescriptorType {
                expected: descriptor_type::CONFIGURATION,
                actual: bytes[1],
            });
        }

        Ok(Self {
            total_length: u16::from_le_bytes([bytes[2], bytes[3]]),
            num_interfaces: bytes[4],
            configuration_value: bytes[5],
            attributes: bytes[7],
            max_power: bytes[8],
        })
    }
}

pub fn parse_configuration(bytes: &[u8]) -> Result<UsbConfigurationInfo, DescriptorParseError> {
    let header = ConfigurationDescriptorHeader::parse(bytes)?;
    let total_length = usize::from(header.total_length);
    if bytes.len() < total_length {
        return Err(DescriptorParseError::BufferTooShort);
    }

    let mut interfaces = Vec::new();
    let mut current_interface = None::<UsbInterfaceInfo>;
    let mut offset = 0usize;

    while offset < total_length {
        if offset + 2 > total_length {
            return Err(DescriptorParseError::BufferTooShort);
        }

        let length = bytes[offset];
        let descriptor_type = bytes[offset + 1];
        if length == 0 {
            return Err(DescriptorParseError::InvalidDescriptorLength(0));
        }
        let end = offset + usize::from(length);
        if end > total_length {
            return Err(DescriptorParseError::TruncatedDescriptor {
                descriptor_type,
                length,
            });
        }

        match descriptor_type {
            descriptor_type::CONFIGURATION => {
                if length < CONFIGURATION_DESCRIPTOR_LEN as u8 {
                    return Err(DescriptorParseError::InvalidDescriptorLength(length));
                }
            }
            DESCRIPTOR_TYPE_INTERFACE => {
                if length < INTERFACE_DESCRIPTOR_LEN as u8 {
                    return Err(DescriptorParseError::InvalidDescriptorLength(length));
                }

                if let Some(interface) = current_interface.take() {
                    interfaces.push(interface);
                }

                current_interface = Some(UsbInterfaceInfo {
                    number: bytes[offset + 2],
                    alternate_setting: bytes[offset + 3],
                    class: bytes[offset + 5],
                    subclass: bytes[offset + 6],
                    protocol: bytes[offset + 7],
                    endpoints: Vec::new(),
                });
            }
            DESCRIPTOR_TYPE_ENDPOINT => {
                if length < ENDPOINT_DESCRIPTOR_LEN as u8 {
                    return Err(DescriptorParseError::InvalidDescriptorLength(length));
                }

                let interface = current_interface
                    .as_mut()
                    .ok_or(DescriptorParseError::MissingInterfaceDescriptor)?;
                interface.endpoints.push(UsbEndpointInfo {
                    address: bytes[offset + 2],
                    attributes: bytes[offset + 3],
                    max_packet_size: u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]),
                    interval: bytes[offset + 6],
                });
            }
            _ => {}
        }

        offset = end;
    }

    if let Some(interface) = current_interface.take() {
        interfaces.push(interface);
    }

    Ok(UsbConfigurationInfo {
        configuration_value: header.configuration_value,
        attributes: header.attributes,
        max_power: header.max_power,
        interfaces,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigurationDescriptorHeader, DescriptorParseError, DeviceDescriptor, parse_configuration,
    };

    #[test_case]
    fn test_parse_device_descriptor() {
        let bytes = [
            18, 1, 0x10, 0x02, 0, 0, 0, 64, 0x34, 0x12, 0x78, 0x56, 0, 1, 1, 2, 3, 1,
        ];
        let descriptor = DeviceDescriptor::parse(&bytes).expect("device descriptor");

        assert_eq!(descriptor.vendor_id, 0x1234);
        assert_eq!(descriptor.product_id, 0x5678);
        assert_eq!(descriptor.max_packet_size0, 64);
        assert_eq!(descriptor.num_configurations, 1);
    }

    #[test_case]
    fn test_parse_device_descriptor_prefix_reads_mps0() {
        let bytes = [18, 1, 0, 2, 0, 0, 0, 8];
        assert_eq!(DeviceDescriptor::parse_max_packet_size0(&bytes), Ok(8));
    }

    #[test_case]
    fn test_parse_configuration_descriptor_header() {
        let bytes = [9, 2, 25, 0, 1, 1, 0, 0xA0, 50];
        let header = ConfigurationDescriptorHeader::parse(&bytes).expect("config header");

        assert_eq!(header.total_length, 25);
        assert_eq!(header.num_interfaces, 1);
        assert_eq!(header.configuration_value, 1);
        assert_eq!(header.attributes, 0xA0);
        assert_eq!(header.max_power, 50);
    }

    #[test_case]
    fn test_parse_configuration_summary() {
        let bytes = [
            9, 2, 25, 0, 1, 1, 0, 0xA0, 50, // config
            9, 4, 0, 0, 1, 3, 1, 1, 0, // interface
            7, 5, 0x81, 0x03, 8, 0, 10, // endpoint
        ];

        let config = parse_configuration(&bytes).expect("configuration");
        assert_eq!(config.configuration_value, 1);
        assert_eq!(config.interfaces.len(), 1);
        let interface = &config.interfaces[0];
        assert_eq!(interface.class, 3);
        assert_eq!(interface.subclass, 1);
        assert_eq!(interface.protocol, 1);
        assert_eq!(interface.endpoints.len(), 1);
        assert_eq!(interface.endpoints[0].address, 0x81);
        assert_eq!(interface.endpoints[0].max_packet_size, 8);
    }

    #[test_case]
    fn test_parse_configuration_rejects_endpoint_before_interface() {
        let bytes = [9, 2, 16, 0, 1, 1, 0, 0x80, 50, 7, 5, 0x81, 0x03, 8, 0, 10];
        assert_eq!(
            parse_configuration(&bytes),
            Err(DescriptorParseError::MissingInterfaceDescriptor)
        );
    }
}
