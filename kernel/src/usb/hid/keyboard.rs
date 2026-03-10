use crate::input::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crate::usb::device::{UsbDeviceInfo, UsbEndpointInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KeyboardDescriptor {
    pub configuration_value: u8,
    pub interface_number: u8,
    pub endpoint_address: u8,
    pub endpoint_interval: u8,
    pub max_packet_size: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReportDecodeError {
    Rollover,
}

pub(super) fn find_boot_keyboard(info: &UsbDeviceInfo) -> Option<KeyboardDescriptor> {
    for configuration in &info.configurations {
        for interface in &configuration.interfaces {
            if interface.class != 0x03 || interface.subclass != 0x01 || interface.protocol != 0x01 {
                continue;
            }

            let endpoint = interface
                .endpoints
                .iter()
                .find(|endpoint| is_interrupt_in_endpoint(endpoint))?;
            return Some(KeyboardDescriptor {
                configuration_value: configuration.configuration_value,
                interface_number: interface.number,
                endpoint_address: endpoint.address,
                endpoint_interval: endpoint.interval,
                max_packet_size: endpoint.max_packet_size,
            });
        }
    }

    None
}

pub(super) fn dispatch_report(
    last_report: &mut [u8; 8],
    report: &[u8; 8],
    mut sink: impl FnMut(KeyEvent),
) -> Result<(), ReportDecodeError> {
    if report[2..].contains(&0x01) {
        return Err(ReportDecodeError::Rollover);
    }

    let modifiers = modifiers_from_bits(report[0]);
    let previous_modifiers = last_report[0];
    let current_modifiers = report[0];

    for bit in 0..8 {
        let was_pressed = (previous_modifiers & (1 << bit)) != 0;
        let is_pressed = (current_modifiers & (1 << bit)) != 0;
        if was_pressed && !is_pressed {
            if let Some(code) = modifier_keycode(bit) {
                sink(KeyEvent {
                    code,
                    kind: KeyEventKind::Release,
                    modifiers,
                });
            }
        }
    }

    for_each_unique_usage(last_report, |usage| {
        if !usage_present(report, usage) {
            if let Some(code) = keycode_from_usage(usage) {
                sink(KeyEvent {
                    code,
                    kind: KeyEventKind::Release,
                    modifiers,
                });
            }
        }
    });

    for bit in 0..8 {
        let was_pressed = (previous_modifiers & (1 << bit)) != 0;
        let is_pressed = (current_modifiers & (1 << bit)) != 0;
        if !was_pressed && is_pressed {
            if let Some(code) = modifier_keycode(bit) {
                sink(KeyEvent {
                    code,
                    kind: KeyEventKind::Press,
                    modifiers,
                });
            }
        }
    }

    for_each_unique_usage(report, |usage| {
        if !usage_present(last_report, usage) {
            if let Some(code) = keycode_from_usage(usage) {
                sink(KeyEvent {
                    code,
                    kind: KeyEventKind::Press,
                    modifiers,
                });
            }
        }
    });

    *last_report = *report;
    Ok(())
}

pub(super) fn modifiers_from_bits(bits: u8) -> KeyModifiers {
    KeyModifiers {
        shift: bits & 0b0010_0010 != 0,
        ctrl: bits & 0b0001_0001 != 0,
        alt: bits & 0b0100_0100 != 0,
        gui: bits & 0b1000_1000 != 0,
    }
}

fn is_interrupt_in_endpoint(endpoint: &UsbEndpointInfo) -> bool {
    (endpoint.address & 0x80) != 0 && (endpoint.attributes & 0x03) == 0x03
}

fn usage_present(report: &[u8; 8], usage: u8) -> bool {
    report[2..].contains(&usage)
}

fn for_each_unique_usage(report: &[u8; 8], mut f: impl FnMut(u8)) {
    for (index, usage) in report[2..].iter().copied().enumerate() {
        if usage == 0 {
            continue;
        }

        if report[2..(index + 2)].contains(&usage) {
            continue;
        }

        f(usage);
    }
}

fn modifier_keycode(bit: usize) -> Option<KeyCode> {
    match bit {
        0 => Some(KeyCode::LeftCtrl),
        1 => Some(KeyCode::LeftShift),
        2 => Some(KeyCode::LeftAlt),
        3 => Some(KeyCode::LeftGui),
        4 => Some(KeyCode::RightCtrl),
        5 => Some(KeyCode::RightShift),
        6 => Some(KeyCode::RightAlt),
        7 => Some(KeyCode::RightGui),
        _ => None,
    }
}

fn keycode_from_usage(usage: u8) -> Option<KeyCode> {
    match usage {
        0x04 => Some(KeyCode::A),
        0x05 => Some(KeyCode::B),
        0x06 => Some(KeyCode::C),
        0x07 => Some(KeyCode::D),
        0x08 => Some(KeyCode::E),
        0x09 => Some(KeyCode::F),
        0x0A => Some(KeyCode::G),
        0x0B => Some(KeyCode::H),
        0x0C => Some(KeyCode::I),
        0x0D => Some(KeyCode::J),
        0x0E => Some(KeyCode::K),
        0x0F => Some(KeyCode::L),
        0x10 => Some(KeyCode::M),
        0x11 => Some(KeyCode::N),
        0x12 => Some(KeyCode::O),
        0x13 => Some(KeyCode::P),
        0x14 => Some(KeyCode::Q),
        0x15 => Some(KeyCode::R),
        0x16 => Some(KeyCode::S),
        0x17 => Some(KeyCode::T),
        0x18 => Some(KeyCode::U),
        0x19 => Some(KeyCode::V),
        0x1A => Some(KeyCode::W),
        0x1B => Some(KeyCode::X),
        0x1C => Some(KeyCode::Y),
        0x1D => Some(KeyCode::Z),
        0x1E => Some(KeyCode::Digit1),
        0x1F => Some(KeyCode::Digit2),
        0x20 => Some(KeyCode::Digit3),
        0x21 => Some(KeyCode::Digit4),
        0x22 => Some(KeyCode::Digit5),
        0x23 => Some(KeyCode::Digit6),
        0x24 => Some(KeyCode::Digit7),
        0x25 => Some(KeyCode::Digit8),
        0x26 => Some(KeyCode::Digit9),
        0x27 => Some(KeyCode::Digit0),
        0x28 => Some(KeyCode::Enter),
        0x29 => Some(KeyCode::Escape),
        0x2A => Some(KeyCode::Backspace),
        0x2B => Some(KeyCode::Tab),
        0x2C => Some(KeyCode::Space),
        0x2D => Some(KeyCode::Minus),
        0x2E => Some(KeyCode::Equal),
        0x2F => Some(KeyCode::LeftBracket),
        0x30 => Some(KeyCode::RightBracket),
        0x31 => Some(KeyCode::Backslash),
        0x33 => Some(KeyCode::Semicolon),
        0x34 => Some(KeyCode::Apostrophe),
        0x35 => Some(KeyCode::Grave),
        0x36 => Some(KeyCode::Comma),
        0x37 => Some(KeyCode::Period),
        0x38 => Some(KeyCode::Slash),
        0x39 => Some(KeyCode::CapsLock),
        0x3A => Some(KeyCode::F1),
        0x3B => Some(KeyCode::F2),
        0x3C => Some(KeyCode::F3),
        0x3D => Some(KeyCode::F4),
        0x3E => Some(KeyCode::F5),
        0x3F => Some(KeyCode::F6),
        0x40 => Some(KeyCode::F7),
        0x41 => Some(KeyCode::F8),
        0x42 => Some(KeyCode::F9),
        0x43 => Some(KeyCode::F10),
        0x44 => Some(KeyCode::F11),
        0x45 => Some(KeyCode::F12),
        0x49 => Some(KeyCode::Insert),
        0x4A => Some(KeyCode::Home),
        0x4B => Some(KeyCode::PageUp),
        0x4C => Some(KeyCode::Delete),
        0x4D => Some(KeyCode::End),
        0x4E => Some(KeyCode::PageDown),
        0x4F => Some(KeyCode::ArrowRight),
        0x50 => Some(KeyCode::ArrowLeft),
        0x51 => Some(KeyCode::ArrowDown),
        0x52 => Some(KeyCode::ArrowUp),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{KeyboardDescriptor, ReportDecodeError, dispatch_report, find_boot_keyboard};
    use crate::input::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use crate::usb::device::{
        UsbConfigurationInfo, UsbDeviceHandle, UsbDeviceInfo, UsbEndpointInfo, UsbInterfaceInfo,
        UsbSpeed,
    };

    fn keyboard_device() -> UsbDeviceInfo {
        UsbDeviceInfo {
            handle: UsbDeviceHandle::allocate(),
            port_id: 1,
            address: 1,
            speed: UsbSpeed::Full,
            vendor_id: 0x1234,
            product_id: 0x5678,
            configurations: vec![UsbConfigurationInfo {
                configuration_value: 1,
                attributes: 0x80,
                max_power: 50,
                interfaces: vec![UsbInterfaceInfo {
                    number: 0,
                    alternate_setting: 0,
                    class: 0x03,
                    subclass: 0x01,
                    protocol: 0x01,
                    endpoints: vec![UsbEndpointInfo {
                        address: 0x81,
                        attributes: 0x03,
                        max_packet_size: 8,
                        interval: 10,
                    }],
                }],
            }],
        }
    }

    #[test_case]
    fn test_find_boot_keyboard_interface() {
        assert_eq!(
            find_boot_keyboard(&keyboard_device()),
            Some(KeyboardDescriptor {
                configuration_value: 1,
                interface_number: 0,
                endpoint_address: 0x81,
                endpoint_interval: 10,
                max_packet_size: 8,
            })
        );
    }

    #[test_case]
    fn test_dispatch_report_generates_press_and_release_events() {
        let mut last_report = [0; 8];
        let mut events = Vec::new();

        dispatch_report(&mut last_report, &[0, 0, 0x04, 0, 0, 0, 0, 0], |event| {
            events.push(event);
        })
        .expect("report decode");
        dispatch_report(&mut last_report, &[0, 0, 0, 0, 0, 0, 0, 0], |event| {
            events.push(event);
        })
        .expect("report decode");

        assert_eq!(
            events,
            vec![
                KeyEvent {
                    code: KeyCode::A,
                    kind: KeyEventKind::Press,
                    modifiers: KeyModifiers::default(),
                },
                KeyEvent {
                    code: KeyCode::A,
                    kind: KeyEventKind::Release,
                    modifiers: KeyModifiers::default(),
                },
            ]
        );
    }

    #[test_case]
    fn test_dispatch_report_tracks_modifier_snapshot() {
        let mut last_report = [0; 8];
        let mut events = Vec::new();

        dispatch_report(
            &mut last_report,
            &[0b0000_0010, 0, 0x04, 0, 0, 0, 0, 0],
            |event| {
                events.push(event);
            },
        )
        .expect("report decode");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code, KeyCode::LeftShift);
        assert_eq!(events[0].kind, KeyEventKind::Press);
        assert!(events[1].modifiers.shift);
        assert_eq!(events[1].code, KeyCode::A);
    }

    #[test_case]
    fn test_dispatch_report_rejects_rollover() {
        let mut last_report = [0; 8];
        let result = dispatch_report(&mut last_report, &[0, 0, 0x01, 0, 0, 0, 0, 0], |_| {});
        assert_eq!(result, Err(ReportDecodeError::Rollover));
        assert_eq!(last_report, [0; 8]);
    }
}
