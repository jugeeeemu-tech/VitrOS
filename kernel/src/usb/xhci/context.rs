use crate::dma::{DmaBuffer, DmaError};
use crate::usb::device::UsbSpeed;

use super::dma::XhciDmaProfile;

const INPUT_CONTROL_CONTEXT_ADD_FLAGS_OFFSET: usize = 0x04;
const SLOT_CONTEXT_DWORD0_OFFSET: usize = 0x00;
const SLOT_CONTEXT_DWORD1_OFFSET: usize = 0x04;
const ENDPOINT_CONTEXT_DWORD1_OFFSET: usize = 0x04;
const ENDPOINT_CONTEXT_DWORD2_OFFSET: usize = 0x08;
const ENDPOINT_CONTEXT_DWORD3_OFFSET: usize = 0x0C;
const ENDPOINT_CONTEXT_DWORD4_OFFSET: usize = 0x10;

const SLOT_CONTEXT_SPEED_SHIFT: u32 = 20;
const SLOT_CONTEXT_CONTEXT_ENTRIES_SHIFT: u32 = 27;
const SLOT_CONTEXT_ROOT_HUB_PORT_SHIFT: u32 = 16;
const SLOT_CONTEXT_USB_DEVICE_ADDRESS_MASK: u32 = 0xff;

const ENDPOINT_CONTEXT_CERR_SHIFT: u32 = 1;
const ENDPOINT_CONTEXT_EP_TYPE_SHIFT: u32 = 3;
const ENDPOINT_CONTEXT_MAX_PACKET_SIZE_SHIFT: u32 = 16;
const ENDPOINT_CONTEXT_AVERAGE_TRB_LENGTH_MASK: u32 = 0xffff;
const ENDPOINT_CONTEXT_CONTROL_EP_TYPE: u32 = 4;

const ADD_CONTEXT_FLAG_SLOT: u32 = 1 << 0;
const ADD_CONTEXT_FLAG_EP0: u32 = 1 << 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextLayout {
    context_size: usize,
}

impl ContextLayout {
    pub const fn new(context_size: usize) -> Option<Self> {
        match context_size {
            32 | 64 => Some(Self { context_size }),
            _ => None,
        }
    }

    pub const fn context_size(self) -> usize {
        self.context_size
    }

    pub const fn input_context_size(self) -> usize {
        33 * self.context_size
    }

    pub const fn device_context_size(self) -> usize {
        32 * self.context_size
    }

    pub const fn input_control_context_offset(self) -> usize {
        0
    }

    pub const fn input_slot_context_offset(self) -> usize {
        self.context_size
    }

    pub const fn input_ep0_context_offset(self) -> usize {
        self.context_size * 2
    }

    pub const fn device_slot_context_offset(self) -> usize {
        0
    }

    pub const fn device_ep0_context_offset(self) -> usize {
        self.context_size
    }
}

pub struct DeviceContextBuffer {
    buffer: DmaBuffer,
    layout: ContextLayout,
}

impl DeviceContextBuffer {
    pub fn new(dma: &XhciDmaProfile, layout: ContextLayout) -> Result<Self, DmaError> {
        Ok(Self {
            buffer: dma.allocate_context(layout.device_context_size())?,
            layout,
        })
    }

    pub fn phys_addr(&self) -> u64 {
        self.buffer.phys_addr()
    }

    pub fn usb_device_address(&self) -> u8 {
        let word = read_dword(
            self.buffer.as_slice(),
            self.layout.device_slot_context_offset(),
            3,
        );
        (word & SLOT_CONTEXT_USB_DEVICE_ADDRESS_MASK) as u8
    }
}

pub struct InputContextBuffer {
    buffer: DmaBuffer,
    layout: ContextLayout,
}

impl InputContextBuffer {
    pub fn new(dma: &XhciDmaProfile, layout: ContextLayout) -> Result<Self, DmaError> {
        Ok(Self {
            buffer: dma.allocate_context(layout.input_context_size())?,
            layout,
        })
    }

    pub fn phys_addr(&self) -> u64 {
        self.buffer.phys_addr()
    }

    pub fn set_address_device_context(
        &mut self,
        port_id: u8,
        speed: UsbSpeed,
        ep0_max_packet_size: u16,
        ep0_tr_dequeue_pointer: u64,
        dequeue_cycle_state: bool,
    ) {
        self.set_add_context_flags(ADD_CONTEXT_FLAG_SLOT | ADD_CONTEXT_FLAG_EP0);
        self.write_slot_context(port_id, speed, 1);
        self.write_ep0_context(ep0_max_packet_size, ep0_tr_dequeue_pointer, dequeue_cycle_state);
    }

    pub fn set_evaluate_context_for_ep0(
        &mut self,
        ep0_max_packet_size: u16,
        ep0_tr_dequeue_pointer: u64,
        dequeue_cycle_state: bool,
    ) {
        self.set_add_context_flags(ADD_CONTEXT_FLAG_EP0);
        self.write_ep0_context(ep0_max_packet_size, ep0_tr_dequeue_pointer, dequeue_cycle_state);
    }

    fn set_add_context_flags(&mut self, flags: u32) {
        write_u32(
            self.buffer.as_mut_slice(),
            self.layout.input_control_context_offset() + INPUT_CONTROL_CONTEXT_ADD_FLAGS_OFFSET,
            flags,
        );
    }

    fn write_slot_context(&mut self, port_id: u8, speed: UsbSpeed, context_entries: u8) {
        let offset = self.layout.input_slot_context_offset();
        let dword0 = ((speed.port_speed_id() as u32) << SLOT_CONTEXT_SPEED_SHIFT)
            | ((context_entries as u32) << SLOT_CONTEXT_CONTEXT_ENTRIES_SHIFT);
        let dword1 = (port_id as u32) << SLOT_CONTEXT_ROOT_HUB_PORT_SHIFT;

        write_u32(
            self.buffer.as_mut_slice(),
            offset + SLOT_CONTEXT_DWORD0_OFFSET,
            dword0,
        );
        write_u32(
            self.buffer.as_mut_slice(),
            offset + SLOT_CONTEXT_DWORD1_OFFSET,
            dword1,
        );
    }

    fn write_ep0_context(
        &mut self,
        max_packet_size: u16,
        tr_dequeue_pointer: u64,
        dequeue_cycle_state: bool,
    ) {
        let offset = self.layout.input_ep0_context_offset();
        let dword1 = (3u32 << ENDPOINT_CONTEXT_CERR_SHIFT)
            | (ENDPOINT_CONTEXT_CONTROL_EP_TYPE << ENDPOINT_CONTEXT_EP_TYPE_SHIFT)
            | ((max_packet_size as u32) << ENDPOINT_CONTEXT_MAX_PACKET_SIZE_SHIFT);
        let dequeue_pointer = (tr_dequeue_pointer & !0x0f) | u64::from(dequeue_cycle_state);

        write_u32(
            self.buffer.as_mut_slice(),
            offset + ENDPOINT_CONTEXT_DWORD1_OFFSET,
            dword1,
        );
        write_u32(
            self.buffer.as_mut_slice(),
            offset + ENDPOINT_CONTEXT_DWORD2_OFFSET,
            dequeue_pointer as u32,
        );
        write_u32(
            self.buffer.as_mut_slice(),
            offset + ENDPOINT_CONTEXT_DWORD3_OFFSET,
            (dequeue_pointer >> 32) as u32,
        );
        write_u32(
            self.buffer.as_mut_slice(),
            offset + ENDPOINT_CONTEXT_DWORD4_OFFSET,
            8 & ENDPOINT_CONTEXT_AVERAGE_TRB_LENGTH_MASK,
        );
    }
}

fn read_dword(bytes: &[u8], base_offset: usize, dword_index: usize) -> u32 {
    let start = base_offset + dword_index * core::mem::size_of::<u32>();
    let raw = &bytes[start..start + core::mem::size_of::<u32>()];
    u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + core::mem::size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::{ContextLayout, DeviceContextBuffer, InputContextBuffer, read_dword};
    use crate::frame_allocator;
    use crate::paging;
    use crate::usb::device::UsbSpeed;
    use crate::usb::xhci::dma::XhciDmaProfile;
    use vitros_common::boot_info::{BootInfo, MemoryRegion};
    use vitros_common::uefi::EFI_CONVENTIONAL_MEMORY;

    fn boot_info_with_regions(regions: &[MemoryRegion]) -> BootInfo {
        let mut boot_info = BootInfo::new();
        for (i, region) in regions.iter().enumerate() {
            boot_info.memory_map[i] = *region;
        }
        boot_info.memory_map_count = regions.len();
        boot_info
    }

    fn setup_allocator() {
        frame_allocator::reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x12000,
            size: 0x80000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        frame_allocator::init(&boot_info).expect("init failed");
    }

    fn dma_profile() -> XhciDmaProfile {
        XhciDmaProfile::new(true, paging::PAGE_SIZE)
    }

    #[test_case]
    fn test_context_layout_offsets_for_both_sizes() {
        let small = ContextLayout::new(32).expect("layout");
        assert_eq!(small.input_context_size(), 1056);
        assert_eq!(small.device_context_size(), 1024);
        assert_eq!(small.input_slot_context_offset(), 32);
        assert_eq!(small.input_ep0_context_offset(), 64);

        let large = ContextLayout::new(64).expect("layout");
        assert_eq!(large.input_context_size(), 2112);
        assert_eq!(large.device_context_size(), 2048);
        assert_eq!(large.input_slot_context_offset(), 64);
        assert_eq!(large.input_ep0_context_offset(), 128);
    }

    #[test_case]
    fn test_input_context_writes_slot_and_ep0_at_variable_offsets() {
        setup_allocator();
        let layout = ContextLayout::new(64).expect("layout");
        let mut buffer = InputContextBuffer::new(&dma_profile(), layout).expect("input context");
        buffer.set_address_device_context(3, UsbSpeed::High, 64, 0x1234_5000, true);

        let bytes = buffer.buffer.as_slice();
        assert_eq!(read_dword(bytes, layout.input_control_context_offset(), 1), 0b11);
        assert_eq!(
            read_dword(bytes, layout.input_slot_context_offset(), 0),
            (3u32 << 20) | (1u32 << 27)
        );
        assert_eq!(
            read_dword(bytes, layout.input_slot_context_offset(), 1),
            3u32 << 16
        );
        assert_eq!(
            read_dword(bytes, layout.input_ep0_context_offset(), 1),
            (3u32 << 1) | (4u32 << 3) | (64u32 << 16)
        );
    }

    #[test_case]
    fn test_device_context_reads_usb_device_address() {
        setup_allocator();
        let layout = ContextLayout::new(32).expect("layout");
        let mut buffer = DeviceContextBuffer::new(&dma_profile(), layout).expect("device context");
        let slot_context_word3_offset = layout.device_slot_context_offset() + 12;
        buffer.buffer.as_mut_slice()[slot_context_word3_offset..slot_context_word3_offset + 4]
            .copy_from_slice(&0x0000_0042u32.to_le_bytes());

        assert_eq!(buffer.usb_device_address(), 0x42);
    }
}
