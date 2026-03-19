//! 入力経路可視化用のトレース記録とオーバーレイ描画
//!
//! `visualize-input` feature が有効なときのみビルドされます。

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::graphics::{self, TaskWriter};
use crate::hpet;
use crate::io::without_interrupts;
use crate::sync::wait_queue::WaitQueue;
use crate::timer;
use crate::usb::hid::KeyboardInputPathSnapshot;
use crate::usb::xhci::event::CompletionCode;
use spin::Mutex;

pub type TraceId = u64;

const TRACE_CAPACITY: usize = 64;
const CONNECTOR_COUNT: usize = 8;
const MODULE_COUNT: usize = 6;
const LAYOUT_CONNECTOR_COUNT: usize = 8;

const SHELL_VIEWPORT_MIN_WIDTH: u32 = 320;
const SHELL_VIEWPORT_MAX_WIDTH: u32 = 420;
const COMPACT_SCREEN_THRESHOLD: u32 = 640;

const AFTERGLOW_DURATION_MS: u64 = 480;
const AFTERGLOW_STEP_MS: u64 = AFTERGLOW_DURATION_MS / 3;
const ANIMATION_INTERVAL_MS: u64 = 16;
const LABEL_STAGGER_STEP: u32 = 14;
const LABEL_STAGGER_SECONDARY_STEP: u32 = 10;
const LABEL_STAGGER_MAX_ALONG_STEPS: usize = 8;
const LABEL_STAGGER_MAX_SIDE_STEPS: usize = 3;
const LABEL_CLEARANCE: u32 = 4;
const ARROW_SIZE: u32 = 4;
const ARROW_MARGIN: u32 = 8;

const PANE_MARGIN: u32 = 14;
const PANE_GAP: u32 = 16;
const HEADER_TEXT_HEIGHT: u32 = 52;
const RING_BOX_HEADER_HEIGHT: u32 = 18;

const BG_COLOR: u32 = 0x000A11;
const PANEL_BG_COLOR: u32 = 0x00111A;
const BOX_BG_COLOR: u32 = 0x001722;
const BOX_BG_ACTIVE: u32 = 0x001E2B;
const BORDER_COLOR: u32 = 0x234454;
const SPLIT_BORDER_COLOR: u32 = 0x2C5A70;
const TITLE_COLOR: u32 = 0xFFD166;
const TEXT_COLOR: u32 = 0xF3F7FA;
const DIM_TEXT_COLOR: u32 = 0x86A6B5;
const SUCCESS_COLOR: u32 = 0x63D471;
const ACTIVE_COLOR: u32 = 0xFFB347;
const ERROR_COLOR: u32 = 0xFF6B6B;

const CONNECTOR_BASE_COLOR: u32 = 0x1A2C39;
const AFTERGLOW_STRONG_COLOR: u32 = 0x4CB4E7;
const AFTERGLOW_MEDIUM_COLOR: u32 = 0x327A9A;
const AFTERGLOW_WEAK_COLOR: u32 = 0x25556D;
const ACTIVE_GLOW_COLOR: u32 = 0xE1AA2D;
const ACTIVE_CORE_COLOR: u32 = 0xFFD166;
const ERROR_GLOW_COLOR: u32 = 0xC84E4E;
const ERROR_CORE_COLOR: u32 = 0xFF7B7B;

const SLOT_EMPTY_COLOR: u32 = 0x213748;
const SLOT_FILLED_COLOR: u32 = 0x66C7F4;
const SLOT_ERROR_COLOR: u32 = 0xFF6B6B;

const TRANSFER_RING_DIAGRAM_SLOTS: usize = 32;
const EVENT_RING_SEGMENT_COUNT: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampSource {
    HpetMs,
    TimerTick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceTimestamp {
    pub value: u64,
    pub source: TimestampSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferEventSnapshot {
    pub slot_id: u8,
    pub endpoint_id: u8,
    pub trb_pointer: u64,
    pub completion_code: CompletionCode,
    pub transfer_length: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PendingUsbTraceContext {
    slot_id: u8,
    endpoint_id: u8,
    transfer_ring_write_at: Option<TraceTimestamp>,
    doorbell_at: Option<TraceTimestamp>,
    transfer_ring_read_at: Option<TraceTimestamp>,
    report_dma_write_at: Option<TraceTimestamp>,
    event_ring_write_at: Option<TraceTimestamp>,
    interrupt_notify_at: Option<TraceTimestamp>,
    event_ring_os_read_at: Option<TraceTimestamp>,
    transfer_event: Option<TransferEventSnapshot>,
    report: [u8; 8],
    report_bytes: u8,
    transfer_failure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceRecord {
    pub id: TraceId,
    pub report: [u8; 8],
    pub report_bytes: u8,
    pub transfer_ring_write_at: Option<TraceTimestamp>,
    pub doorbell_at: Option<TraceTimestamp>,
    pub transfer_ring_read_at: Option<TraceTimestamp>,
    pub report_dma_write_at: Option<TraceTimestamp>,
    pub event_ring_write_at: Option<TraceTimestamp>,
    pub interrupt_notify_at: Option<TraceTimestamp>,
    pub event_ring_os_read_at: Option<TraceTimestamp>,
    pub transfer_event: Option<TransferEventSnapshot>,
    pub transfer_failure: bool,
}

impl TraceRecord {
    fn from_pending(id: TraceId, pending: PendingUsbTraceContext) -> Self {
        Self {
            id,
            report: pending.report,
            report_bytes: pending.report_bytes,
            transfer_ring_write_at: pending.transfer_ring_write_at,
            doorbell_at: pending.doorbell_at,
            transfer_ring_read_at: pending.transfer_ring_read_at,
            report_dma_write_at: pending.report_dma_write_at,
            event_ring_write_at: pending.event_ring_write_at,
            interrupt_notify_at: pending.interrupt_notify_at,
            event_ring_os_read_at: pending.event_ring_os_read_at,
            transfer_event: pending.transfer_event,
            transfer_failure: pending.transfer_failure,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CommittedReportSnapshot {
    pub bytes: [u8; 8],
    pub len: u8,
    pub updated_at: Option<TraceTimestamp>,
}

impl CommittedReportSnapshot {
    pub const fn empty() -> Self {
        Self {
            bytes: [0; 8],
            len: 0,
            updated_at: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TraceStatusSnapshot {
    pub enabled: bool,
    pub stored_records: usize,
    pub dropped_records: u64,
    pub active_keyboard_present: bool,
    pub controller_snapshot_available: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramStage {
    TransferRingWrite,
    Doorbell,
    TransferRingRead,
    Poll,
    ReportDmaWrite,
    EventRingWrite,
    InterruptNotify,
    EventRingRead,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorId {
    OsToTransferRingWrite,
    OsToXhciDoorbell,
    XhciToTransferRingRead,
    XhciToKeyboardUsbPoll,
    XhciToReportDmaWrite,
    XhciToEventRingWrite,
    XhciToOsInterrupt,
    OsToEventRingRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorGlow {
    Off,
    Active,
    AfterglowStrong,
    AfterglowMedium,
    AfterglowWeak,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorSnapshot {
    pub id: ConnectorId,
    pub glow: ConnectorGlow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleId {
    Os,
    Xhci,
    KeyboardUsbDevice,
    TransferRing,
    ReportBuffer,
    EventRing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleState {
    Idle,
    Active,
    Complete,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingDiagramCell {
    occupied: bool,
    link: bool,
    enqueue: bool,
    reclaim: bool,
    dequeue: bool,
    error: bool,
}

impl RingDiagramCell {
    const fn empty() -> Self {
        Self {
            occupied: false,
            link: false,
            enqueue: false,
            reclaim: false,
            dequeue: false,
            error: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RingSide {
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleSnapshot {
    pub id: ModuleId,
    pub state: ModuleState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputTraceSnapshot {
    pub status: TraceStatusSnapshot,
    pub(crate) keyboard_path: Option<Box<KeyboardInputPathSnapshot>>,
    pub(crate) controller_event_overflowed: bool,
    pub latest_transfer: Option<TraceRecord>,
    pub latest_stage: Option<DiagramStage>,
    pub last_committed_report: CommittedReportSnapshot,
    pub shared_device_path_glow: ConnectorGlow,
    pub modules: [ModuleSnapshot; MODULE_COUNT],
    pub connectors: [ConnectorSnapshot; CONNECTOR_COUNT],
    pub has_afterglow: bool,
}

impl Default for InputTraceSnapshot {
    fn default() -> Self {
        Self {
            status: TraceStatusSnapshot::default(),
            keyboard_path: None,
            controller_event_overflowed: false,
            latest_transfer: None,
            latest_stage: None,
            last_committed_report: CommittedReportSnapshot::empty(),
            shared_device_path_glow: ConnectorGlow::Off,
            modules: default_modules(),
            connectors: default_connectors(),
            has_afterglow: false,
        }
    }
}

struct TraceState {
    enabled: bool,
    next_id: TraceId,
    head: usize,
    len: usize,
    dropped_records: u64,
    pending_usb: Option<PendingUsbTraceContext>,
    last_committed_report: CommittedReportSnapshot,
    controller_event_overflowed: bool,
    generation: u64,
    records: [Option<TraceRecord>; TRACE_CAPACITY],
}

impl TraceState {
    const fn new() -> Self {
        Self {
            enabled: false,
            next_id: 1,
            head: 0,
            len: 0,
            dropped_records: 0,
            pending_usb: None,
            last_committed_report: CommittedReportSnapshot::empty(),
            controller_event_overflowed: false,
            generation: 0,
            records: [const { None }; TRACE_CAPACITY],
        }
    }

    fn clear_records(&mut self) {
        self.head = 0;
        self.len = 0;
        self.dropped_records = 0;
        self.pending_usb = None;
        self.last_committed_report = CommittedReportSnapshot::empty();
        self.controller_event_overflowed = false;
        self.records = [const { None }; TRACE_CAPACITY];
    }

    fn next_id(&mut self) -> TraceId {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    fn push_record(&mut self, record: TraceRecord) {
        let index = if self.len < TRACE_CAPACITY {
            let idx = (self.head + self.len) % TRACE_CAPACITY;
            self.len += 1;
            idx
        } else {
            let idx = self.head;
            self.head = (self.head + 1) % TRACE_CAPACITY;
            self.dropped_records = self.dropped_records.saturating_add(1);
            idx
        };

        self.records[index] = Some(record);
        self.generation = self.generation.saturating_add(1);
    }

    fn status(&self) -> TraceStatusSnapshot {
        TraceStatusSnapshot {
            enabled: self.enabled,
            stored_records: self.len,
            dropped_records: self.dropped_records,
            active_keyboard_present: false,
            controller_snapshot_available: false,
            generation: self.generation,
        }
    }

    fn latest_stored(&self) -> Option<TraceRecord> {
        if self.len == 0 {
            return None;
        }
        let index = (self.head + self.len - 1) % TRACE_CAPACITY;
        self.records[index]
    }

    fn latest_visible(&self) -> Option<TraceRecord> {
        self.pending_usb
            .map(|pending| TraceRecord::from_pending(0, pending))
            .or_else(|| self.latest_stored())
    }
}

static TRACE_STATE: Mutex<TraceState> = Mutex::new(TraceState::new());

static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);
static UPDATE_SIGNAL: AtomicBool = AtomicBool::new(false);
static UPDATE_WAIT: WaitQueue = WaitQueue::new();

pub fn status_snapshot() -> TraceStatusSnapshot {
    let mut status = without_interrupts(|| {
        let state = TRACE_STATE.lock();
        state.status()
    });
    status.active_keyboard_present = crate::usb::hid::active_keyboard_present();
    status.controller_snapshot_available = crate::usb::hid::keyboard_input_path_available();
    status
}

pub fn snapshot() -> InputTraceSnapshot {
    let mut snapshot = InputTraceSnapshot::default();
    snapshot_into(&mut snapshot);
    snapshot
}

pub fn snapshot_into(snapshot: &mut InputTraceSnapshot) {
    let now = current_timestamp();
    without_interrupts(|| {
        let state = TRACE_STATE.lock();
        snapshot.status = state.status();
        snapshot.latest_transfer = state.latest_visible();
        snapshot.last_committed_report = state.last_committed_report;
        snapshot.controller_event_overflowed = state.controller_event_overflowed;
    });

    snapshot.keyboard_path = crate::usb::hid::snapshot_keyboard_input_path();
    snapshot.status.active_keyboard_present = crate::usb::hid::active_keyboard_present();
    snapshot.status.controller_snapshot_available = snapshot.keyboard_path.is_some();

    let poll_active = snapshot.status.enabled && snapshot.status.active_keyboard_present;
    without_interrupts(|| {
        let state = TRACE_STATE.lock();
        snapshot.connectors = connector_snapshots_from_state(
            &state,
            now,
            poll_active,
            snapshot.controller_event_overflowed,
        );
    });

    snapshot.latest_stage = snapshot
        .latest_transfer
        .map(diagram_stage)
        .or_else(|| poll_active.then_some(DiagramStage::Poll));
    snapshot.shared_device_path_glow =
        connector_glow_from_snapshots(&snapshot.connectors, ConnectorId::XhciToKeyboardUsbPoll);
    snapshot.modules = module_snapshots(
        &snapshot.connectors,
        snapshot.status.active_keyboard_present,
        snapshot.controller_event_overflowed,
    );
    snapshot.has_afterglow = snapshot
        .connectors
        .iter()
        .any(|connector| !matches!(connector.glow, ConnectorGlow::Off));
}

pub fn set_enabled(enabled: bool) {
    TRACE_ENABLED.store(enabled, Ordering::Release);
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        state.enabled = enabled;
        state.generation = state.generation.saturating_add(1);
    });
    notify_overlay();
}

pub fn clear() {
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        let enabled = state.enabled;
        state.clear_records();
        state.enabled = enabled;
        state.generation = state.generation.saturating_add(1);
    });
    notify_overlay();
}

pub fn is_enabled() -> bool {
    TRACE_ENABLED.load(Ordering::Acquire)
}

pub fn record_transfer_queued(slot_id: u8, endpoint_id: u8, _trb_pointer: u64) {
    if !is_enabled() {
        return;
    }

    let now = current_timestamp();
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        state.pending_usb = Some(PendingUsbTraceContext {
            slot_id,
            endpoint_id,
            transfer_ring_write_at: Some(now),
            ..PendingUsbTraceContext::default()
        });
        state.generation = state.generation.saturating_add(1);
    });
    notify_overlay();
}

pub fn record_transfer_doorbell(slot_id: u8, endpoint_id: u8, _trb_pointer: u64) {
    if !is_enabled() {
        return;
    }

    let now = current_timestamp();
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        let mut pending = state.pending_usb.unwrap_or_default();
        pending.slot_id = slot_id;
        pending.endpoint_id = endpoint_id;
        if pending.transfer_ring_write_at.is_none() {
            pending.transfer_ring_write_at = Some(now);
        }
        pending.doorbell_at = Some(now);
        pending.transfer_ring_read_at = Some(now);
        state.pending_usb = Some(pending);
        state.generation = state.generation.saturating_add(1);
    });
    notify_overlay();
}

pub fn record_interrupt_notify() {
    if !is_enabled() {
        return;
    }

    let now = current_timestamp();
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        let mut pending = state.pending_usb.unwrap_or_default();
        if pending.transfer_ring_write_at.is_none() {
            pending.transfer_ring_write_at = Some(now);
        }
        pending.interrupt_notify_at = Some(now);
        state.pending_usb = Some(pending);
        state.generation = state.generation.saturating_add(1);
    });
    notify_overlay();
}

pub fn record_transfer_event(
    slot_id: u8,
    endpoint_id: u8,
    trb_pointer: u64,
    completion_code: CompletionCode,
    transfer_length: u32,
) {
    if !is_enabled() {
        return;
    }

    let now = current_timestamp();
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        let mut pending = state.pending_usb.unwrap_or_default();
        pending.slot_id = slot_id;
        pending.endpoint_id = endpoint_id;
        if pending.transfer_ring_write_at.is_none() {
            pending.transfer_ring_write_at = Some(now);
        }
        if pending.doorbell_at.is_none() {
            pending.doorbell_at = Some(now);
        }
        if pending.transfer_ring_read_at.is_none() {
            pending.transfer_ring_read_at = Some(now);
        }
        pending.event_ring_write_at = Some(now);
        pending.transfer_event = Some(TransferEventSnapshot {
            slot_id,
            endpoint_id,
            trb_pointer,
            completion_code,
            transfer_length,
        });
        pending.transfer_failure = !matches!(
            completion_code,
            CompletionCode::Success | CompletionCode::ShortPacket
        );
        state.pending_usb = Some(pending);
        state.generation = state.generation.saturating_add(1);
    });
    notify_overlay();
}

pub fn record_event_ring_os_read(slot_id: u8, endpoint_id: u8) {
    if !is_enabled() {
        return;
    }

    let now = current_timestamp();
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        let mut pending = state.pending_usb.unwrap_or_default();
        pending.slot_id = slot_id;
        pending.endpoint_id = endpoint_id;
        if pending.transfer_ring_write_at.is_none() {
            pending.transfer_ring_write_at = Some(now);
        }
        if pending.event_ring_write_at.is_none() {
            pending.event_ring_write_at = Some(now);
        }
        pending.event_ring_os_read_at = Some(now);
        state.pending_usb = Some(pending);
        state.generation = state.generation.saturating_add(1);
    });
    notify_overlay();
}

pub fn record_report_ready(report: [u8; 8], report_bytes: u8) {
    if !is_enabled() {
        return;
    }

    let now = current_timestamp();
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        let mut pending = state.pending_usb.unwrap_or_default();
        if pending.transfer_ring_write_at.is_none() {
            pending.transfer_ring_write_at = Some(now);
        }
        pending.report = report;
        pending.report_bytes = report_bytes;
        pending.report_dma_write_at = Some(now);
        state.last_committed_report = CommittedReportSnapshot {
            bytes: report,
            len: report_bytes,
            updated_at: Some(now),
        };
        let id = state.next_id();
        state.push_record(TraceRecord::from_pending(id, pending));
        state.pending_usb = None;
    });
    notify_overlay();
}

pub fn record_transfer_failure(
    slot_id: u8,
    endpoint_id: u8,
    trb_pointer: u64,
    completion_code: CompletionCode,
    transfer_length: u32,
) {
    if !is_enabled() {
        return;
    }

    let now = current_timestamp();
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        let mut pending = state.pending_usb.unwrap_or_default();
        pending.slot_id = slot_id;
        pending.endpoint_id = endpoint_id;
        if pending.transfer_ring_write_at.is_none() {
            pending.transfer_ring_write_at = Some(now);
        }
        if pending.doorbell_at.is_none() {
            pending.doorbell_at = Some(now);
        }
        if pending.transfer_ring_read_at.is_none() {
            pending.transfer_ring_read_at = Some(now);
        }
        if pending.event_ring_write_at.is_none() {
            pending.event_ring_write_at = Some(now);
        }
        pending.transfer_event = Some(TransferEventSnapshot {
            slot_id,
            endpoint_id,
            trb_pointer,
            completion_code,
            transfer_length,
        });
        pending.transfer_failure = true;
        let id = state.next_id();
        state.push_record(TraceRecord::from_pending(id, pending));
        state.pending_usb = None;
    });
    notify_overlay();
}

pub fn record_controller_event_overflow() {
    if !is_enabled() {
        return;
    }

    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        state.controller_event_overflowed = true;
        state.generation = state.generation.saturating_add(1);
    });
    notify_overlay();
}

pub fn shell_viewport_width(screen_width: u32) -> u32 {
    if screen_width <= COMPACT_SCREEN_THRESHOLD {
        return (screen_width / 2).max(1);
    }

    let preferred = (screen_width / 3).clamp(SHELL_VIEWPORT_MIN_WIDTH, SHELL_VIEWPORT_MAX_WIDTH);
    preferred
        .min(screen_width.saturating_sub(SHELL_VIEWPORT_MIN_WIDTH))
        .max(1)
}

pub extern "C" fn overlay_task() -> ! {
    crate::info!("[InputTrace] Overlay task started");

    let (screen_width, screen_height) = graphics::compositor::screen_size();
    let region = visualization_region(screen_width, screen_height);
    let buffer =
        graphics::compositor::register_writer(region).expect("Failed to register input overlay");
    let mut writer = TaskWriter::new(buffer, TEXT_COLOR);
    let mut trace_snapshot = Box::new(InputTraceSnapshot::default());
    let mut last_generation = 0;
    let mut animate = false;

    loop {
        if animate {
            crate::sched::sleep_ms(ANIMATION_INTERVAL_MS);
        } else {
            wait_for_update(last_generation);
        }

        snapshot_into(trace_snapshot.as_mut());
        last_generation = trace_snapshot.status.generation;

        if !trace_snapshot.status.enabled {
            animate = false;
            continue;
        }

        render_visualization(
            trace_snapshot.as_ref(),
            &mut writer,
            region.width,
            region.height,
            region.x,
        );
        animate = trace_snapshot.has_afterglow;
    }
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    TRACE_ENABLED.store(false, Ordering::Release);
    UPDATE_SIGNAL.store(false, Ordering::Release);
    without_interrupts(|| {
        let mut state = TRACE_STATE.lock();
        *state = TraceState::new();
    });
}

fn default_connectors() -> [ConnectorSnapshot; CONNECTOR_COUNT] {
    [
        ConnectorSnapshot {
            id: ConnectorId::OsToTransferRingWrite,
            glow: ConnectorGlow::Off,
        },
        ConnectorSnapshot {
            id: ConnectorId::OsToXhciDoorbell,
            glow: ConnectorGlow::Off,
        },
        ConnectorSnapshot {
            id: ConnectorId::XhciToTransferRingRead,
            glow: ConnectorGlow::Off,
        },
        ConnectorSnapshot {
            id: ConnectorId::XhciToKeyboardUsbPoll,
            glow: ConnectorGlow::Off,
        },
        ConnectorSnapshot {
            id: ConnectorId::XhciToReportDmaWrite,
            glow: ConnectorGlow::Off,
        },
        ConnectorSnapshot {
            id: ConnectorId::XhciToEventRingWrite,
            glow: ConnectorGlow::Off,
        },
        ConnectorSnapshot {
            id: ConnectorId::XhciToOsInterrupt,
            glow: ConnectorGlow::Off,
        },
        ConnectorSnapshot {
            id: ConnectorId::OsToEventRingRead,
            glow: ConnectorGlow::Off,
        },
    ]
}

fn default_modules() -> [ModuleSnapshot; MODULE_COUNT] {
    [
        ModuleSnapshot {
            id: ModuleId::Os,
            state: ModuleState::Idle,
        },
        ModuleSnapshot {
            id: ModuleId::Xhci,
            state: ModuleState::Idle,
        },
        ModuleSnapshot {
            id: ModuleId::KeyboardUsbDevice,
            state: ModuleState::Idle,
        },
        ModuleSnapshot {
            id: ModuleId::TransferRing,
            state: ModuleState::Idle,
        },
        ModuleSnapshot {
            id: ModuleId::ReportBuffer,
            state: ModuleState::Idle,
        },
        ModuleSnapshot {
            id: ModuleId::EventRing,
            state: ModuleState::Idle,
        },
    ]
}

fn connector_snapshots_from_state(
    state: &TraceState,
    now: TraceTimestamp,
    poll_active: bool,
    controller_event_overflowed: bool,
) -> [ConnectorSnapshot; CONNECTOR_COUNT] {
    let mut snapshots = default_connectors();
    let latest_stored_id = if state.pending_usb.is_some() {
        None
    } else {
        state.latest_stored().map(|record| record.id)
    };

    for offset in 0..state.len {
        let index = (state.head + state.len - 1 - offset) % TRACE_CAPACITY;
        let Some(record) = state.records[index] else {
            continue;
        };
        apply_record_connector_glows(
            &mut snapshots,
            record,
            latest_stored_id == Some(record.id),
            now,
        );
    }

    if let Some(pending) = state.pending_usb {
        apply_record_connector_glows(
            &mut snapshots,
            TraceRecord::from_pending(0, pending),
            true,
            now,
        );
    }

    if poll_active {
        let current_glow =
            connector_glow_from_snapshots(&snapshots, ConnectorId::XhciToKeyboardUsbPoll);
        set_connector_glow(
            &mut snapshots,
            ConnectorId::XhciToKeyboardUsbPoll,
            stronger_glow(current_glow, polling_glow(now)),
        );
    }

    if controller_event_overflowed {
        set_connector_glow(
            &mut snapshots,
            ConnectorId::XhciToEventRingWrite,
            ConnectorGlow::Error,
        );
    }

    snapshots
}

fn apply_record_connector_glows(
    snapshots: &mut [ConnectorSnapshot; CONNECTOR_COUNT],
    record: TraceRecord,
    is_latest: bool,
    now: TraceTimestamp,
) {
    let latest_stage = is_latest.then_some(diagram_stage(record));
    let latest_elapsed = latest_trace_elapsed(record, latest_stage, now);

    for connector in snapshots.iter_mut() {
        let candidate = glow_for_connector(
            record,
            connector.id,
            is_latest,
            latest_stage,
            latest_elapsed,
            now,
        );
        if glow_priority(candidate) > glow_priority(connector.glow) {
            connector.glow = candidate;
        }
    }
}

fn module_snapshots(
    connectors: &[ConnectorSnapshot; CONNECTOR_COUNT],
    active_keyboard_present: bool,
    controller_event_overflowed: bool,
) -> [ModuleSnapshot; MODULE_COUNT] {
    [
        ModuleSnapshot {
            id: ModuleId::Os,
            state: module_state_from_connectors(
                connectors,
                &[
                    ConnectorId::OsToTransferRingWrite,
                    ConnectorId::OsToXhciDoorbell,
                    ConnectorId::XhciToOsInterrupt,
                    ConnectorId::OsToEventRingRead,
                ],
            ),
        },
        ModuleSnapshot {
            id: ModuleId::Xhci,
            state: module_state_from_connectors(
                connectors,
                &[
                    ConnectorId::OsToXhciDoorbell,
                    ConnectorId::XhciToTransferRingRead,
                    ConnectorId::XhciToKeyboardUsbPoll,
                    ConnectorId::XhciToReportDmaWrite,
                    ConnectorId::XhciToEventRingWrite,
                    ConnectorId::XhciToOsInterrupt,
                ],
            ),
        },
        ModuleSnapshot {
            id: ModuleId::KeyboardUsbDevice,
            state: if active_keyboard_present {
                module_state_from_connectors(connectors, &[ConnectorId::XhciToKeyboardUsbPoll])
            } else {
                ModuleState::Idle
            },
        },
        ModuleSnapshot {
            id: ModuleId::TransferRing,
            state: module_state_from_connectors(
                connectors,
                &[
                    ConnectorId::OsToTransferRingWrite,
                    ConnectorId::XhciToTransferRingRead,
                ],
            ),
        },
        ModuleSnapshot {
            id: ModuleId::ReportBuffer,
            state: module_state_from_connectors(connectors, &[ConnectorId::XhciToReportDmaWrite]),
        },
        ModuleSnapshot {
            id: ModuleId::EventRing,
            state: if controller_event_overflowed {
                ModuleState::Error
            } else {
                module_state_from_connectors(
                    connectors,
                    &[
                        ConnectorId::XhciToEventRingWrite,
                        ConnectorId::OsToEventRingRead,
                    ],
                )
            },
        },
    ]
}

fn module_state_from_connectors(
    connectors: &[ConnectorSnapshot; CONNECTOR_COUNT],
    related: &[ConnectorId],
) -> ModuleState {
    let mut state = ModuleState::Idle;
    for connector in connectors {
        if !related.contains(&connector.id) {
            continue;
        }
        let candidate = match connector.glow {
            ConnectorGlow::Off => ModuleState::Idle,
            ConnectorGlow::Active => ModuleState::Active,
            ConnectorGlow::AfterglowStrong
            | ConnectorGlow::AfterglowMedium
            | ConnectorGlow::AfterglowWeak => ModuleState::Complete,
            ConnectorGlow::Error => ModuleState::Error,
        };
        state = stronger_module_state(state, candidate);
    }
    state
}

fn stronger_module_state(left: ModuleState, right: ModuleState) -> ModuleState {
    fn rank(state: ModuleState) -> u8 {
        match state {
            ModuleState::Idle => 0,
            ModuleState::Complete => 1,
            ModuleState::Active => 2,
            ModuleState::Error => 3,
        }
    }

    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

fn notify_overlay() {
    UPDATE_SIGNAL.store(true, Ordering::Release);
    UPDATE_WAIT.wake_one();
}

fn wait_for_update(last_generation: u64) {
    loop {
        if current_generation() != last_generation {
            return;
        }

        if UPDATE_SIGNAL.swap(false, Ordering::AcqRel) {
            return;
        }

        UPDATE_WAIT.wait();
    }
}

fn current_generation() -> u64 {
    without_interrupts(|| TRACE_STATE.lock().generation)
}

fn current_timestamp() -> TraceTimestamp {
    if hpet::is_available() {
        TraceTimestamp {
            value: hpet::elapsed_ms(),
            source: TimestampSource::HpetMs,
        }
    } else {
        TraceTimestamp {
            value: timer::current_tick(),
            source: TimestampSource::TimerTick,
        }
    }
}

fn elapsed_ms(now: TraceTimestamp, then: TraceTimestamp) -> u64 {
    match (now.source, then.source) {
        (TimestampSource::HpetMs, TimestampSource::HpetMs) => now.value.saturating_sub(then.value),
        (TimestampSource::TimerTick, TimestampSource::TimerTick) => {
            let ticks = now.value.saturating_sub(then.value);
            let hz = timer::frequency_hz();
            if hz == 0 {
                0
            } else {
                ticks.saturating_mul(1000) / hz
            }
        }
        _ => now.value.saturating_sub(then.value),
    }
}

fn visualization_region(screen_width: u32, screen_height: u32) -> graphics::Region {
    let shell_width = shell_viewport_width(screen_width).min(screen_width.saturating_sub(1).max(1));
    graphics::Region::new(
        shell_width,
        0,
        screen_width.saturating_sub(shell_width).max(1),
        screen_height,
    )
}

fn diagram_stage(record: TraceRecord) -> DiagramStage {
    if record.transfer_failure {
        return DiagramStage::Error;
    }

    let candidates = [
        (
            DiagramStage::TransferRingWrite,
            record.transfer_ring_write_at,
            0u8,
        ),
        (DiagramStage::Doorbell, record.doorbell_at, 1u8),
        (
            DiagramStage::TransferRingRead,
            record.transfer_ring_read_at,
            2u8,
        ),
        (
            DiagramStage::EventRingWrite,
            record.event_ring_write_at,
            3u8,
        ),
        (
            DiagramStage::InterruptNotify,
            record.interrupt_notify_at,
            4u8,
        ),
        (
            DiagramStage::EventRingRead,
            record.event_ring_os_read_at,
            5u8,
        ),
        (
            DiagramStage::ReportDmaWrite,
            record.report_dma_write_at,
            6u8,
        ),
    ];

    candidates
        .into_iter()
        .filter_map(|(stage, timestamp, order)| {
            timestamp.map(|timestamp| (stage, timestamp, order))
        })
        .max_by_key(|(_, timestamp, order)| (timestamp.value, *order))
        .map(|(stage, _, _)| stage)
        .unwrap_or(DiagramStage::TransferRingWrite)
}

fn glow_for_connector(
    record: TraceRecord,
    connector: ConnectorId,
    is_latest: bool,
    latest_stage: Option<DiagramStage>,
    latest_elapsed: Option<u64>,
    now: TraceTimestamp,
) -> ConnectorGlow {
    let timestamp = match connector {
        ConnectorId::OsToTransferRingWrite => record.transfer_ring_write_at,
        ConnectorId::OsToXhciDoorbell => record.doorbell_at,
        ConnectorId::XhciToTransferRingRead => record.transfer_ring_read_at,
        ConnectorId::XhciToKeyboardUsbPoll => record.report_dma_write_at,
        ConnectorId::XhciToReportDmaWrite => record.report_dma_write_at,
        ConnectorId::XhciToEventRingWrite => record.event_ring_write_at,
        ConnectorId::XhciToOsInterrupt => record.interrupt_notify_at,
        ConnectorId::OsToEventRingRead => record.event_ring_os_read_at,
    };
    let error = matches!(connector, ConnectorId::XhciToEventRingWrite) && record.transfer_failure;
    let latest_connector = match latest_stage {
        Some(DiagramStage::TransferRingWrite) => Some(ConnectorId::OsToTransferRingWrite),
        Some(DiagramStage::Doorbell) => Some(ConnectorId::OsToXhciDoorbell),
        Some(DiagramStage::TransferRingRead) => Some(ConnectorId::XhciToTransferRingRead),
        Some(DiagramStage::Poll) => Some(ConnectorId::XhciToKeyboardUsbPoll),
        Some(DiagramStage::ReportDmaWrite) => Some(ConnectorId::XhciToReportDmaWrite),
        Some(DiagramStage::EventRingWrite) | Some(DiagramStage::Error) => {
            Some(ConnectorId::XhciToEventRingWrite)
        }
        Some(DiagramStage::InterruptNotify) => Some(ConnectorId::XhciToOsInterrupt),
        Some(DiagramStage::EventRingRead) => Some(ConnectorId::OsToEventRingRead),
        None => None,
    };
    let is_latest_connector = latest_connector == Some(connector)
        || (matches!(latest_stage, Some(DiagramStage::ReportDmaWrite))
            && connector == ConnectorId::XhciToKeyboardUsbPoll);
    path_glow(
        timestamp,
        now,
        is_latest,
        latest_elapsed,
        is_latest_connector,
        error,
    )
}

fn latest_trace_elapsed(
    record: TraceRecord,
    latest_stage: Option<DiagramStage>,
    now: TraceTimestamp,
) -> Option<u64> {
    let timestamp = match latest_stage? {
        DiagramStage::TransferRingWrite => record.transfer_ring_write_at,
        DiagramStage::Doorbell => record.doorbell_at,
        DiagramStage::TransferRingRead => record.transfer_ring_read_at,
        DiagramStage::Poll => None,
        DiagramStage::ReportDmaWrite => record.report_dma_write_at,
        DiagramStage::EventRingWrite | DiagramStage::Error => record.event_ring_write_at,
        DiagramStage::InterruptNotify => record.interrupt_notify_at,
        DiagramStage::EventRingRead => record.event_ring_os_read_at,
    }?;
    Some(elapsed_ms(now, timestamp))
}

fn path_glow(
    timestamp: Option<TraceTimestamp>,
    now: TraceTimestamp,
    is_latest: bool,
    latest_elapsed: Option<u64>,
    is_latest_connector: bool,
    error: bool,
) -> ConnectorGlow {
    let Some(timestamp) = timestamp else {
        return ConnectorGlow::Off;
    };
    if error {
        return ConnectorGlow::Error;
    }

    let elapsed = if is_latest {
        latest_elapsed.unwrap_or_else(|| elapsed_ms(now, timestamp))
    } else {
        elapsed_ms(now, timestamp)
    };
    if is_latest_connector {
        connector_glow_from_age(elapsed, true, false)
    } else {
        connector_glow_from_age(elapsed, false, false)
    }
}

fn connector_glow_from_age(elapsed_ms: u64, is_latest: bool, error: bool) -> ConnectorGlow {
    if elapsed_ms >= AFTERGLOW_DURATION_MS {
        return ConnectorGlow::Off;
    }

    if error {
        return ConnectorGlow::Error;
    }

    if is_latest && elapsed_ms < AFTERGLOW_STEP_MS {
        return ConnectorGlow::Active;
    }

    if elapsed_ms < AFTERGLOW_STEP_MS {
        ConnectorGlow::AfterglowStrong
    } else if elapsed_ms < AFTERGLOW_STEP_MS * 2 {
        ConnectorGlow::AfterglowMedium
    } else {
        ConnectorGlow::AfterglowWeak
    }
}

fn polling_glow(now: TraceTimestamp) -> ConnectorGlow {
    let phase = now.value % AFTERGLOW_DURATION_MS;
    if phase < AFTERGLOW_STEP_MS {
        ConnectorGlow::AfterglowStrong
    } else if phase < AFTERGLOW_STEP_MS * 2 {
        ConnectorGlow::AfterglowMedium
    } else {
        ConnectorGlow::AfterglowWeak
    }
}

fn glow_priority(glow: ConnectorGlow) -> u8 {
    match glow {
        ConnectorGlow::Off => 0,
        ConnectorGlow::AfterglowWeak => 1,
        ConnectorGlow::AfterglowMedium => 2,
        ConnectorGlow::AfterglowStrong => 3,
        ConnectorGlow::Error => 4,
        ConnectorGlow::Active => 5,
    }
}

fn stronger_glow(left: ConnectorGlow, right: ConnectorGlow) -> ConnectorGlow {
    if glow_priority(left) >= glow_priority(right) {
        left
    } else {
        right
    }
}

fn connector_glow(trace_snapshot: &InputTraceSnapshot, id: ConnectorId) -> ConnectorGlow {
    connector_glow_from_snapshots(&trace_snapshot.connectors, id)
}

fn connector_glow_from_snapshots(
    connectors: &[ConnectorSnapshot; CONNECTOR_COUNT],
    id: ConnectorId,
) -> ConnectorGlow {
    connectors
        .iter()
        .find(|connector| connector.id == id)
        .map(|connector| connector.glow)
        .unwrap_or(ConnectorGlow::Off)
}

fn set_connector_glow(
    connectors: &mut [ConnectorSnapshot; CONNECTOR_COUNT],
    id: ConnectorId,
    glow: ConnectorGlow,
) {
    if let Some(connector) = connectors.iter_mut().find(|connector| connector.id == id) {
        connector.glow = glow;
    }
}

fn module_state(trace_snapshot: &InputTraceSnapshot, id: ModuleId) -> ModuleState {
    trace_snapshot
        .modules
        .iter()
        .find(|module| module.id == id)
        .map(|module| module.state)
        .unwrap_or(ModuleState::Idle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Point {
    x: u32,
    y: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LabelledConnectorLayout {
    id: ConnectorId,
    source: ModuleId,
    target: ModuleId,
    label: &'static str,
    render_mode: LabelRenderMode,
    label_segment_index: usize,
    label_side: LabelSide,
    label_side_offset: u32,
    label_along_offset: u32,
    label_rect: graphics::Region,
    points: [Point; 4],
    len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelSide {
    Left,
    Right,
    Above,
    Below,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelRenderMode {
    ExternalLabel,
    InlineCutoutLabel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LabelOverlapGroup {
    OsDown,
    OsRight,
    XhciDown,
    XhciLeft,
    XhciRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectorArrowMode {
    SingleToTarget,
    Bidirectional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ControllerDiagramLayout {
    os_box: graphics::Region,
    xhci_box: graphics::Region,
    keyboard_box: graphics::Region,
    transfer_box: graphics::Region,
    report_box: graphics::Region,
    event_box: graphics::Region,
    report_bytes_area: graphics::Region,
    report_status_area: graphics::Region,
    connectors: [LabelledConnectorLayout; LAYOUT_CONNECTOR_COUNT],
}

fn controller_diagram_layout(width: u32, height: u32) -> ControllerDiagramLayout {
    let inner_width = width.saturating_sub(PANE_MARGIN * 2);
    let top_y = HEADER_TEXT_HEIGHT + 4;
    let actor_height = (height / 10).clamp(64, 84);
    let row_gap = (height / 12).clamp(44, 72);
    let module_y = top_y + actor_height + row_gap;
    let module_height = height.saturating_sub(module_y + PANE_MARGIN).max(1);
    let col_width = inner_width.saturating_sub(PANE_GAP * 2) / 3;

    let os_box = graphics::Region::new(PANE_MARGIN, top_y, col_width, actor_height);
    let xhci_box = graphics::Region::new(
        os_box.x + os_box.width + PANE_GAP,
        top_y,
        col_width,
        actor_height,
    );
    let keyboard_box = graphics::Region::new(
        xhci_box.x + xhci_box.width + PANE_GAP,
        top_y,
        col_width,
        actor_height,
    );

    let transfer_box = graphics::Region::new(os_box.x, module_y, col_width, module_height);
    let report_box = graphics::Region::new(xhci_box.x, module_y, col_width, module_height);
    let event_box = graphics::Region::new(keyboard_box.x, module_y, col_width, module_height);

    let write_lane_y = top_y + actor_height + row_gap / 3;
    let lane_mid_y = top_y + actor_height + row_gap / 2;
    let event_lane_y = top_y + actor_height + (row_gap * 2) / 3;
    let event_read_lane_y = top_y + actor_height + (row_gap * 5) / 6;
    let doorbell_y = os_box.y + os_box.height / 3;
    let interrupt_y = os_box.y + (os_box.height * 2) / 3;
    let poll_y = xhci_box.y + xhci_box.height / 2;
    let report_bytes_area = graphics::Region::new(
        report_box.x + 18,
        report_box.y + 60,
        report_box.width.saturating_sub(36),
        28,
    );
    let report_status_area = graphics::Region::new(
        report_box.x + 10,
        report_box.y + 34,
        report_box.width.saturating_sub(20),
        18,
    );

    let write_points = [
        Point {
            x: center_x(&os_box),
            y: os_box.y + os_box.height,
        },
        Point {
            x: center_x(&os_box),
            y: write_lane_y,
        },
        Point {
            x: transfer_box.x + transfer_box.width / 3,
            y: write_lane_y,
        },
        Point {
            x: transfer_box.x + transfer_box.width / 3,
            y: transfer_box.y,
        },
    ];
    let doorbell_points = [
        Point {
            x: os_box.x + os_box.width,
            y: doorbell_y,
        },
        Point {
            x: xhci_box.x,
            y: doorbell_y,
        },
        Point { x: 0, y: 0 },
        Point { x: 0, y: 0 },
    ];
    let read_points = [
        Point {
            x: xhci_box.x + xhci_box.width / 3,
            y: xhci_box.y + xhci_box.height,
        },
        Point {
            x: xhci_box.x + xhci_box.width / 3,
            y: lane_mid_y,
        },
        Point {
            x: transfer_box.x + (transfer_box.width * 2) / 3,
            y: lane_mid_y,
        },
        Point {
            x: transfer_box.x + (transfer_box.width * 2) / 3,
            y: transfer_box.y,
        },
    ];
    let poll_points = [
        Point {
            x: xhci_box.x + xhci_box.width,
            y: poll_y,
        },
        Point {
            x: keyboard_box.x,
            y: poll_y,
        },
        Point { x: 0, y: 0 },
        Point { x: 0, y: 0 },
    ];
    let dma_write_points = [
        Point {
            x: center_x(&xhci_box),
            y: xhci_box.y + xhci_box.height,
        },
        Point {
            x: center_x(&xhci_box),
            y: write_lane_y,
        },
        Point {
            x: center_x(&report_box),
            y: write_lane_y,
        },
        Point {
            x: center_x(&report_box),
            y: report_box.y,
        },
    ];
    let event_write_points = [
        Point {
            x: xhci_box.x + (xhci_box.width * 2) / 3,
            y: xhci_box.y + xhci_box.height,
        },
        Point {
            x: xhci_box.x + (xhci_box.width * 2) / 3,
            y: event_lane_y,
        },
        Point {
            x: event_box.x + event_box.width / 3,
            y: event_lane_y,
        },
        Point {
            x: event_box.x + event_box.width / 3,
            y: event_box.y,
        },
    ];
    let event_read_points = [
        Point {
            x: os_box.x + (os_box.width * 2) / 3,
            y: os_box.y + os_box.height,
        },
        Point {
            x: os_box.x + (os_box.width * 2) / 3,
            y: event_read_lane_y,
        },
        Point {
            x: event_box.x + (event_box.width * 2) / 3,
            y: event_read_lane_y,
        },
        Point {
            x: event_box.x + (event_box.width * 2) / 3,
            y: event_box.y,
        },
    ];
    let interrupt_points = [
        Point {
            x: xhci_box.x,
            y: interrupt_y,
        },
        Point {
            x: os_box.x + os_box.width,
            y: interrupt_y,
        },
        Point { x: 0, y: 0 },
        Point { x: 0, y: 0 },
    ];

    let mut connectors = [
        labelled_connector(
            ConnectorId::OsToTransferRingWrite,
            ModuleId::Os,
            ModuleId::TransferRing,
            "write",
            write_points,
            4,
            LabelRenderMode::ExternalLabel,
            0,
            LabelSide::Left,
            14,
            14,
        ),
        labelled_connector(
            ConnectorId::OsToXhciDoorbell,
            ModuleId::Os,
            ModuleId::Xhci,
            "doorbell",
            doorbell_points,
            2,
            LabelRenderMode::ExternalLabel,
            0,
            LabelSide::Above,
            16,
            16,
        ),
        labelled_connector(
            ConnectorId::XhciToTransferRingRead,
            ModuleId::Xhci,
            ModuleId::TransferRing,
            "fetch",
            read_points,
            4,
            LabelRenderMode::InlineCutoutLabel,
            0,
            LabelSide::Right,
            14,
            14,
        ),
        labelled_connector(
            ConnectorId::XhciToKeyboardUsbPoll,
            ModuleId::Xhci,
            ModuleId::KeyboardUsbDevice,
            "poll",
            poll_points,
            2,
            LabelRenderMode::ExternalLabel,
            0,
            LabelSide::Above,
            16,
            18,
        ),
        labelled_connector(
            ConnectorId::XhciToReportDmaWrite,
            ModuleId::Xhci,
            ModuleId::ReportBuffer,
            "dma write",
            dma_write_points,
            4,
            LabelRenderMode::InlineCutoutLabel,
            0,
            LabelSide::Right,
            14,
            14,
        ),
        labelled_connector(
            ConnectorId::XhciToEventRingWrite,
            ModuleId::Xhci,
            ModuleId::EventRing,
            "event write",
            event_write_points,
            4,
            LabelRenderMode::InlineCutoutLabel,
            0,
            LabelSide::Right,
            14,
            14,
        ),
        labelled_connector(
            ConnectorId::XhciToOsInterrupt,
            ModuleId::Xhci,
            ModuleId::Os,
            "interrupt",
            interrupt_points,
            2,
            LabelRenderMode::ExternalLabel,
            0,
            LabelSide::Below,
            16,
            18,
        ),
        labelled_connector(
            ConnectorId::OsToEventRingRead,
            ModuleId::Os,
            ModuleId::EventRing,
            "read",
            event_read_points,
            4,
            LabelRenderMode::InlineCutoutLabel,
            0,
            LabelSide::Right,
            14,
            16,
        ),
    ];
    resolve_label_rects(
        &mut connectors,
        &[
            os_box,
            xhci_box,
            keyboard_box,
            transfer_box,
            report_box,
            event_box,
            report_status_area,
            report_bytes_area,
        ],
    );

    ControllerDiagramLayout {
        os_box,
        xhci_box,
        keyboard_box,
        transfer_box,
        report_box,
        event_box,
        report_bytes_area,
        report_status_area,
        connectors,
    }
}

fn render_visualization(
    trace_snapshot: &InputTraceSnapshot,
    writer: &mut TaskWriter,
    width: u32,
    height: u32,
    shell_width: u32,
) {
    writer.clear(BG_COLOR);
    writer.fill_rect(0, 0, width, height, PANEL_BG_COLOR);
    writer.fill_rect(0, 0, 2, height, SPLIT_BORDER_COLOR);

    draw_header(trace_snapshot, writer, shell_width, width);
    let layout = controller_diagram_layout(width, height);

    for connector in &layout.connectors {
        draw_connector_path(
            writer,
            connector,
            connector_glow(trace_snapshot, connector.id),
        );
    }

    draw_centered_actor_box(
        writer,
        &layout.os_box,
        "OS",
        None,
        module_state(trace_snapshot, ModuleId::Os),
    );
    draw_centered_actor_box(
        writer,
        &layout.xhci_box,
        "xHCI",
        None,
        module_state(trace_snapshot, ModuleId::Xhci),
    );
    draw_centered_actor_box(
        writer,
        &layout.keyboard_box,
        "Keyboard / USB Device",
        (!trace_snapshot.status.active_keyboard_present).then_some("no active keyboard"),
        module_state(trace_snapshot, ModuleId::KeyboardUsbDevice),
    );
    draw_transfer_ring_box(
        writer,
        &layout.transfer_box,
        trace_snapshot,
        trace_snapshot.keyboard_path.as_deref(),
        module_state(trace_snapshot, ModuleId::TransferRing),
    );
    draw_report_box(
        writer,
        &layout,
        &layout.report_box,
        trace_snapshot,
        module_state(trace_snapshot, ModuleId::ReportBuffer),
    );
    draw_event_ring_box(
        writer,
        &layout.event_box,
        trace_snapshot,
        trace_snapshot.keyboard_path.as_deref(),
        trace_snapshot.latest_transfer,
        module_state(trace_snapshot, ModuleId::EventRing),
    );

    for connector in &layout.connectors {
        draw_connector_label(
            writer,
            connector,
            connector_glow(trace_snapshot, connector.id),
        );
    }

    writer.flush();
}

fn draw_header(
    trace_snapshot: &InputTraceSnapshot,
    writer: &mut TaskWriter,
    shell_width: u32,
    pane_width: u32,
) {
    writer.set_color(TITLE_COLOR);
    writer.draw_string_at(PANE_MARGIN, 12, "Input Path Visualization");

    writer.set_color(TEXT_COLOR);
    writer.draw_string_at(
        PANE_MARGIN,
        24,
        &format!(
            "mode=controller diagram  path=usb-transaction  shell={}px  diagram={}px",
            shell_width, pane_width
        ),
    );

    writer.set_color(DIM_TEXT_COLOR);
    writer.draw_string_at(
        PANE_MARGIN,
        36,
        &format!(
            "stored={}  dropped={}  keyboard={}  controller={}  afterglow={}ms",
            trace_snapshot.status.stored_records,
            trace_snapshot.status.dropped_records,
            if trace_snapshot.status.active_keyboard_present {
                "present"
            } else {
                "absent"
            },
            if trace_snapshot.status.controller_snapshot_available {
                "ready"
            } else {
                "unavailable"
            },
            AFTERGLOW_DURATION_MS
        ),
    );
}

fn draw_centered_actor_box(
    writer: &mut TaskWriter,
    rect: &graphics::Region,
    title: &str,
    subtitle: Option<&str>,
    state: ModuleState,
) {
    let border = border_for_module(state);
    let fill = if matches!(state, ModuleState::Active) {
        BOX_BG_ACTIVE
    } else {
        BOX_BG_COLOR
    };
    writer.fill_rect(rect.x, rect.y, rect.width, rect.height, fill);
    draw_outline(writer, rect.x, rect.y, rect.width, rect.height, border);

    writer.set_color(TEXT_COLOR);
    let title_width = (title.len() as u32) * 8;
    let title_x = rect
        .x
        .saturating_add(rect.width.saturating_sub(title_width) / 2);
    let title_y = rect.y + rect.height / 2 - if subtitle.is_some() { 12 } else { 4 };
    writer.draw_string_at(title_x, title_y, title);

    if let Some(subtitle) = subtitle {
        writer.set_color(DIM_TEXT_COLOR);
        let subtitle_width = (subtitle.len() as u32) * 8;
        let subtitle_x = rect
            .x
            .saturating_add(rect.width.saturating_sub(subtitle_width) / 2);
        writer.draw_string_at(subtitle_x, title_y + 14, subtitle);
    }
}

fn draw_transfer_ring_box(
    writer: &mut TaskWriter,
    rect: &graphics::Region,
    trace_snapshot: &InputTraceSnapshot,
    keyboard_path: Option<&KeyboardInputPathSnapshot>,
    state: ModuleState,
) {
    draw_ring_box_frame(writer, rect, "Transfer Ring", state);

    let Some(path) = keyboard_path else {
        draw_dim_text(writer, rect.x + 10, rect.y + 34, "runtime unavailable");
        return;
    };

    let ring_rect = transfer_ring_rect(rect);
    let cells = transfer_ring_cells(trace_snapshot, path, trace_snapshot.latest_transfer);
    let visible_slots = transfer_ring_visible_slot_count(path, &cells);
    draw_ring_diagram(writer, &ring_rect, &cells[..visible_slots]);

    draw_dim_text(
        writer,
        rect.x + 10,
        rect.y + rect.height.saturating_sub(18),
        if path.pending_trb_pointer.is_some() {
            "pending completion"
        } else {
            "idle"
        },
    );
}

fn draw_report_box(
    writer: &mut TaskWriter,
    layout: &ControllerDiagramLayout,
    rect: &graphics::Region,
    trace_snapshot: &InputTraceSnapshot,
    state: ModuleState,
) {
    draw_ring_box_frame(writer, rect, "Report Buffer DMA", state);

    let Some(path) = trace_snapshot.keyboard_path.as_deref() else {
        draw_dim_text(writer, rect.x + 10, rect.y + 34, "buffer unavailable");
        return;
    };

    let report = trace_snapshot.last_committed_report.bytes;
    let bytes = trace_snapshot.last_committed_report.len;

    draw_dim_text(
        writer,
        layout.report_status_area.x,
        layout.report_status_area.y,
        &format!("bytes={} / {}", bytes, path.report_buffer_len),
    );

    let cell_gap = 8;
    let total_gap = cell_gap * 7;
    let cell_width = layout
        .report_bytes_area
        .width
        .saturating_sub(total_gap)
        .max(8)
        / 8;
    let y = layout.report_bytes_area.y;
    for (index, byte) in report.iter().enumerate() {
        let x = layout.report_bytes_area.x + (index as u32) * (cell_width + cell_gap);
        writer.fill_rect(
            x,
            y,
            cell_width,
            layout.report_bytes_area.height,
            BOX_BG_ACTIVE,
        );
        draw_outline(
            writer,
            x,
            y,
            cell_width,
            layout.report_bytes_area.height,
            border_for_module(state),
        );
        writer.set_color(TEXT_COLOR);
        writer.draw_string_at(
            x + cell_width.saturating_sub(16) / 2,
            y + 8,
            &format!("{:02X}", byte),
        );
    }

    draw_dim_text(
        writer,
        rect.x + 10,
        rect.y + rect.height.saturating_sub(18),
        if trace_snapshot.last_committed_report.updated_at.is_none() {
            "waiting for report"
        } else {
            "dma payload updated"
        },
    );
}

fn draw_event_ring_box(
    writer: &mut TaskWriter,
    rect: &graphics::Region,
    trace_snapshot: &InputTraceSnapshot,
    keyboard_path: Option<&KeyboardInputPathSnapshot>,
    record: Option<TraceRecord>,
    state: ModuleState,
) {
    draw_ring_box_frame(writer, rect, "Event Ring", state);

    let Some(path) = keyboard_path else {
        draw_dim_text(writer, rect.x + 10, rect.y + 34, "event ring unavailable");
        return;
    };

    let ring_rect = event_ring_rect(rect);
    let cells = event_ring_cells(trace_snapshot, path, record);
    draw_ring_diagram(writer, &ring_rect, &cells);

    draw_dim_text(
        writer,
        rect.x + 10,
        rect.y + rect.height.saturating_sub(18),
        if path.event_overflowed {
            "overflow"
        } else if record.and_then(|record| record.transfer_event).is_some() {
            "transfer event visible"
        } else {
            "awaiting completion"
        },
    );
}

fn transfer_ring_rect(rect: &graphics::Region) -> graphics::Region {
    graphics::Region::new(
        rect.x + 18,
        rect.y + RING_BOX_HEADER_HEIGHT + 16,
        rect.width.saturating_sub(36),
        rect.height.saturating_sub(RING_BOX_HEADER_HEIGHT + 52),
    )
}

fn event_ring_rect(rect: &graphics::Region) -> graphics::Region {
    graphics::Region::new(
        rect.x + 18,
        rect.y + RING_BOX_HEADER_HEIGHT + 16,
        rect.width.saturating_sub(36),
        rect.height.saturating_sub(RING_BOX_HEADER_HEIGHT + 52),
    )
}

fn transfer_ring_cells(
    _trace_snapshot: &InputTraceSnapshot,
    path: &KeyboardInputPathSnapshot,
    _record: Option<TraceRecord>,
) -> [RingDiagramCell; TRANSFER_RING_DIAGRAM_SLOTS] {
    let mut cells = [const { RingDiagramCell::empty() }; TRANSFER_RING_DIAGRAM_SLOTS];
    let mut visible_index = 0usize;

    for slot in path.interrupt_ring.slots.iter().copied().take(
        path.interrupt_ring
            .slot_count
            .min(TRANSFER_RING_DIAGRAM_SLOTS),
    ) {
        if slot.is_link
            || visible_index
                >= path
                    .interrupt_ring
                    .capacity
                    .min(TRANSFER_RING_DIAGRAM_SLOTS)
        {
            continue;
        }
        let cell = RingDiagramCell {
            occupied: slot.occupied,
            link: false,
            enqueue: slot.is_enqueue,
            reclaim: slot.is_reclaim,
            dequeue: false,
            error: slot.is_error,
        };
        cells[visible_index] = cell;
        visible_index += 1;
    }

    cells
}

fn transfer_ring_visible_slot_count(
    path: &KeyboardInputPathSnapshot,
    cells: &[RingDiagramCell; TRANSFER_RING_DIAGRAM_SLOTS],
) -> usize {
    let visible = path
        .interrupt_ring
        .capacity
        .min(TRANSFER_RING_DIAGRAM_SLOTS);
    let packed = cells
        .iter()
        .take(visible)
        .filter(|cell| cell.occupied || cell.enqueue || cell.reclaim || cell.error)
        .count();
    visible.max(packed).max(1)
}

fn event_ring_cells(
    _trace_snapshot: &InputTraceSnapshot,
    path: &KeyboardInputPathSnapshot,
    record: Option<TraceRecord>,
) -> [RingDiagramCell; EVENT_RING_SEGMENT_COUNT] {
    let mut cells = [const { RingDiagramCell::empty() }; EVENT_RING_SEGMENT_COUNT];
    if path.event_ring.slot_count == 0 {
        return cells;
    }

    let segment_span = path
        .event_ring
        .slot_count
        .div_ceil(EVENT_RING_SEGMENT_COUNT)
        .max(1);
    let is_error =
        path.event_overflowed || record.map(|item| item.transfer_failure).unwrap_or(false);
    let error_index = if is_error {
        Some(previous_ring_index(
            path.event_ring.dequeue_index,
            path.event_ring.slot_count,
        ))
    } else {
        None
    };

    for (segment_index, cell) in cells.iter_mut().enumerate() {
        let start = segment_index * segment_span;
        if start >= path.event_ring.slot_count {
            break;
        }
        let end = (start + segment_span).min(path.event_ring.slot_count);
        let slots = &path.event_ring.slots[start..end];
        cell.occupied = slots.iter().any(|slot| slot.occupied);
        cell.dequeue = slots.iter().any(|slot| slot.is_dequeue);
        if let Some(error_index) = error_index
            && error_index >= start
            && error_index < end
        {
            cell.error = true;
        }
        if path.event_overflowed && cell.dequeue {
            cell.error = true;
        }
    }

    cells
}

fn previous_ring_index(index: usize, slot_count: usize) -> usize {
    if slot_count == 0 {
        0
    } else if index == 0 {
        slot_count - 1
    } else {
        index - 1
    }
}

fn draw_ring_diagram(writer: &mut TaskWriter, rect: &graphics::Region, cells: &[RingDiagramCell]) {
    if rect.width < 48 || rect.height < 48 || cells.is_empty() {
        return;
    }

    let slot_size = (rect.width.min(rect.height) / 10).clamp(7, 12);
    let outer = graphics::Region::new(
        rect.x + 4,
        rect.y + 4,
        rect.width.saturating_sub(8),
        rect.height.saturating_sub(8),
    );
    let inner_margin = slot_size + 8;
    writer.fill_rect(outer.x, outer.y, outer.width, outer.height, PANEL_BG_COLOR);
    draw_outline(
        writer,
        outer.x,
        outer.y,
        outer.width,
        outer.height,
        CONNECTOR_BASE_COLOR,
    );
    if outer.width > inner_margin * 2 && outer.height > inner_margin * 2 {
        let inner = graphics::Region::new(
            outer.x + inner_margin,
            outer.y + inner_margin,
            outer.width - inner_margin * 2,
            outer.height - inner_margin * 2,
        );
        writer.fill_rect(inner.x, inner.y, inner.width, inner.height, BOX_BG_COLOR);
        draw_outline(
            writer,
            inner.x,
            inner.y,
            inner.width,
            inner.height,
            CONNECTOR_BASE_COLOR,
        );
    }

    for (index, cell) in cells.iter().enumerate() {
        let (center_x, center_y, side) = perimeter_position(&outer, slot_size, index, cells.len());
        let slot_x = center_x.saturating_sub(slot_size / 2);
        let slot_y = center_y.saturating_sub(slot_size / 2);
        writer.fill_rect(
            slot_x,
            slot_y,
            slot_size,
            slot_size,
            ring_cell_fill_color(*cell),
        );
        draw_outline(
            writer,
            slot_x,
            slot_y,
            slot_size,
            slot_size,
            ring_cell_border_color(*cell),
        );
        if let Some(marker) = ring_cell_marker_label(*cell) {
            draw_ring_marker(writer, marker, slot_x, slot_y, slot_size, side, *cell);
        }
    }
}

fn perimeter_position(
    rect: &graphics::Region,
    slot_size: u32,
    index: usize,
    slot_count: usize,
) -> (u32, u32, RingSide) {
    let width = rect.width.saturating_sub(slot_size).max(1);
    let height = rect.height.saturating_sub(slot_size).max(1);
    let perimeter = u64::from(width) * 2 + u64::from(height) * 2;
    let distance = ((index as u64 * 2 + 1) * perimeter) / ((slot_count as u64) * 2);
    let half = slot_size / 2;
    let width_u64 = u64::from(width);
    let height_u64 = u64::from(height);

    if distance < width_u64 {
        (
            rect.x + half + distance as u32,
            rect.y + half,
            RingSide::Top,
        )
    } else if distance < width_u64 + height_u64 {
        (
            rect.x + half + width,
            rect.y + half + (distance - width_u64) as u32,
            RingSide::Right,
        )
    } else if distance < width_u64 * 2 + height_u64 {
        (
            rect.x + half + width - (distance - width_u64 - height_u64) as u32,
            rect.y + half + height,
            RingSide::Bottom,
        )
    } else {
        (
            rect.x + half,
            rect.y + height - (distance - width_u64 * 2 - height_u64) as u32,
            RingSide::Left,
        )
    }
}

fn ring_cell_fill_color(cell: RingDiagramCell) -> u32 {
    if cell.error {
        return SLOT_ERROR_COLOR;
    }

    if cell.occupied {
        SLOT_FILLED_COLOR
    } else {
        SLOT_EMPTY_COLOR
    }
}

fn ring_cell_border_color(cell: RingDiagramCell) -> u32 {
    if cell.error {
        ERROR_COLOR
    } else {
        BORDER_COLOR
    }
}

fn ring_cell_marker_label(cell: RingDiagramCell) -> Option<&'static str> {
    match (cell.enqueue, cell.reclaim, cell.dequeue) {
        (true, true, false) => Some("ER"),
        (true, false, false) => Some("E"),
        (false, true, false) => Some("R"),
        (false, false, true) => Some("D"),
        _ => None,
    }
}

fn draw_ring_marker(
    writer: &mut TaskWriter,
    label: &str,
    slot_x: u32,
    slot_y: u32,
    slot_size: u32,
    side: RingSide,
    cell: RingDiagramCell,
) {
    let text_width = (label.len() as u32) * 8;
    let color = if cell.error { ERROR_COLOR } else { TITLE_COLOR };
    let (text_x, text_y) = match side {
        RingSide::Top => (
            slot_x + slot_size / 2 - text_width / 2,
            slot_y.saturating_sub(10),
        ),
        RingSide::Right => (slot_x + slot_size + 2, slot_y + slot_size / 2 - 4),
        RingSide::Bottom => (
            slot_x + slot_size / 2 - text_width / 2,
            slot_y + slot_size + 2,
        ),
        RingSide::Left => (
            slot_x.saturating_sub(text_width + 2),
            slot_y + slot_size / 2 - 4,
        ),
    };
    writer.set_color(color);
    writer.draw_string_at(text_x, text_y, label);
}

fn draw_ring_box_frame(
    writer: &mut TaskWriter,
    rect: &graphics::Region,
    title: &str,
    state: ModuleState,
) {
    let border = border_for_module(state);
    let fill = if matches!(state, ModuleState::Active) {
        BOX_BG_ACTIVE
    } else {
        BOX_BG_COLOR
    };
    writer.fill_rect(rect.x, rect.y, rect.width, rect.height, fill);
    draw_outline(writer, rect.x, rect.y, rect.width, rect.height, border);
    writer.fill_rect(
        rect.x,
        rect.y,
        rect.width,
        RING_BOX_HEADER_HEIGHT,
        PANEL_BG_COLOR,
    );
    writer.set_color(TITLE_COLOR);
    writer.draw_string_at(rect.x + 8, rect.y + 5, title);
}

fn draw_connector_path(
    writer: &mut TaskWriter,
    connector: &LabelledConnectorLayout,
    glow: ConnectorGlow,
) {
    for (segment_index, segment) in connector.points[..connector.len].windows(2).enumerate() {
        let start = segment[0];
        let end = segment[1];
        let cutout = inline_cutout_for_segment(connector, segment_index);
        if start.x == end.x {
            draw_straight_vertical(writer, start.x, start.y, end.y, glow, cutout);
        } else if start.y == end.y {
            draw_straight_horizontal(writer, start.x, end.x, start.y, glow, cutout);
        }
    }
    draw_connector_arrows(writer, connector, glow);
}

fn draw_connector_label(
    writer: &mut TaskWriter,
    connector: &LabelledConnectorLayout,
    glow: ConnectorGlow,
) {
    debug_assert_eq!(
        connector.label_rect,
        compute_label_rect(
            connector.label,
            connector.render_mode,
            &connector.points,
            connector.label_segment_index,
            connector.label_side,
            connector.label_side_offset,
            connector.label_along_offset,
        )
    );
    let background = connector_label_background_rect(connector);
    writer.fill_rect(
        background.x,
        background.y,
        background.width,
        background.height,
        PANEL_BG_COLOR,
    );
    let color = match glow {
        ConnectorGlow::Off => DIM_TEXT_COLOR,
        ConnectorGlow::Error => ERROR_COLOR,
        _ => TITLE_COLOR,
    };
    writer.set_color(color);
    writer.draw_string_at(
        connector.label_rect.x,
        connector.label_rect.y,
        connector.label,
    );
}

fn inline_cutout_for_segment(
    connector: &LabelledConnectorLayout,
    segment_index: usize,
) -> Option<graphics::Region> {
    if matches!(connector.render_mode, LabelRenderMode::InlineCutoutLabel)
        && connector.label_segment_index == segment_index
    {
        Some(connector_label_background_rect(connector))
    } else {
        None
    }
}

fn connector_label_background_rect(connector: &LabelledConnectorLayout) -> graphics::Region {
    graphics::Region::new(
        connector.label_rect.x.saturating_sub(2),
        connector.label_rect.y.saturating_sub(2),
        connector.label_rect.width + 4,
        connector.label_rect.height + 4,
    )
}

fn draw_straight_horizontal(
    writer: &mut TaskWriter,
    x0: u32,
    x1: u32,
    y: u32,
    glow: ConnectorGlow,
    cutout: Option<graphics::Region>,
) {
    let start = x0.min(x1);
    let end = x0.max(x1);
    if let Some(cutout) = cutout {
        let cut_start = cutout.x.saturating_sub(2).max(start);
        let cut_end = (cutout.x + cutout.width + 2).min(end);
        if cut_start > start {
            let width = cut_start.saturating_sub(start).max(2);
            writer.fill_rect(start, y.saturating_sub(1), width, 2, CONNECTOR_BASE_COLOR);
            draw_glow_overlay_horizontal(writer, start, y, width, glow);
        }
        if cut_end < end {
            let width = end.saturating_sub(cut_end).max(2);
            writer.fill_rect(cut_end, y.saturating_sub(1), width, 2, CONNECTOR_BASE_COLOR);
            draw_glow_overlay_horizontal(writer, cut_end, y, width, glow);
        }
    } else {
        let width = end.saturating_sub(start).max(2);
        writer.fill_rect(start, y.saturating_sub(1), width, 2, CONNECTOR_BASE_COLOR);
        draw_glow_overlay_horizontal(writer, start, y, width, glow);
    }
}

fn draw_straight_vertical(
    writer: &mut TaskWriter,
    x: u32,
    y0: u32,
    y1: u32,
    glow: ConnectorGlow,
    cutout: Option<graphics::Region>,
) {
    let start = y0.min(y1);
    let end = y0.max(y1);
    if let Some(cutout) = cutout {
        let cut_start = cutout.y.saturating_sub(2).max(start);
        let cut_end = (cutout.y + cutout.height + 2).min(end);
        if cut_start > start {
            let height = cut_start.saturating_sub(start).max(2);
            writer.fill_rect(x.saturating_sub(1), start, 2, height, CONNECTOR_BASE_COLOR);
            draw_glow_overlay_vertical(writer, x, start, height, glow);
        }
        if cut_end < end {
            let height = end.saturating_sub(cut_end).max(2);
            writer.fill_rect(
                x.saturating_sub(1),
                cut_end,
                2,
                height,
                CONNECTOR_BASE_COLOR,
            );
            draw_glow_overlay_vertical(writer, x, cut_end, height, glow);
        }
    } else {
        let height = end.saturating_sub(start).max(2);
        writer.fill_rect(x.saturating_sub(1), start, 2, height, CONNECTOR_BASE_COLOR);
        draw_glow_overlay_vertical(writer, x, start, height, glow);
    }
}

fn draw_glow_overlay_horizontal(
    writer: &mut TaskWriter,
    x: u32,
    y: u32,
    width: u32,
    glow: ConnectorGlow,
) {
    let (glow_color, glow_thickness, core_color, core_thickness) = glow_style(glow);
    if glow_color != 0 {
        let offset = glow_thickness / 2;
        writer.fill_rect(
            x,
            y.saturating_sub(offset),
            width,
            glow_thickness.max(1),
            glow_color,
        );
    }
    if core_color != 0 {
        let offset = core_thickness / 2;
        writer.fill_rect(
            x,
            y.saturating_sub(offset),
            width,
            core_thickness.max(1),
            core_color,
        );
    }
}

fn draw_glow_overlay_vertical(
    writer: &mut TaskWriter,
    x: u32,
    y: u32,
    height: u32,
    glow: ConnectorGlow,
) {
    let (glow_color, glow_thickness, core_color, core_thickness) = glow_style(glow);
    if glow_color != 0 {
        let offset = glow_thickness / 2;
        writer.fill_rect(
            x.saturating_sub(offset),
            y,
            glow_thickness.max(1),
            height,
            glow_color,
        );
    }
    if core_color != 0 {
        let offset = core_thickness / 2;
        writer.fill_rect(
            x.saturating_sub(offset),
            y,
            core_thickness.max(1),
            height,
            core_color,
        );
    }
}

fn draw_connector_arrows(
    writer: &mut TaskWriter,
    connector: &LabelledConnectorLayout,
    glow: ConnectorGlow,
) {
    let first = [connector.points[0], connector.points[1]];
    let last = [
        connector.points[connector.len - 2],
        connector.points[connector.len - 1],
    ];

    match connector_arrow_mode(connector.id) {
        ConnectorArrowMode::SingleToTarget => {
            draw_arrowhead_on_segment(writer, last[0], last[1], glow);
        }
        ConnectorArrowMode::Bidirectional => {
            draw_arrowhead_on_segment(writer, first[1], first[0], glow);
            draw_arrowhead_on_segment(writer, last[0], last[1], glow);
        }
    }
}

fn connector_arrow_mode(id: ConnectorId) -> ConnectorArrowMode {
    match id {
        ConnectorId::XhciToKeyboardUsbPoll => ConnectorArrowMode::Bidirectional,
        _ => ConnectorArrowMode::SingleToTarget,
    }
}

fn draw_arrowhead_on_segment(
    writer: &mut TaskWriter,
    start: Point,
    end: Point,
    glow: ConnectorGlow,
) {
    let color = arrow_color(glow);
    if color == 0 {
        return;
    }

    let tip = if start.x == end.x {
        Point {
            x: end.x,
            y: if start.y < end.y {
                end.y.saturating_sub(ARROW_MARGIN)
            } else {
                end.y.saturating_add(ARROW_MARGIN)
            },
        }
    } else {
        Point {
            x: if start.x < end.x {
                end.x.saturating_sub(ARROW_MARGIN)
            } else {
                end.x.saturating_add(ARROW_MARGIN)
            },
            y: end.y,
        }
    };

    if start.x == end.x {
        if start.y < end.y {
            draw_arrow_down(writer, tip.x, tip.y, color);
        } else {
            draw_arrow_up(writer, tip.x, tip.y, color);
        }
    } else if start.x < end.x {
        draw_arrow_right(writer, tip.x, tip.y, color);
    } else {
        draw_arrow_left(writer, tip.x, tip.y, color);
    }
}

fn arrow_color(glow: ConnectorGlow) -> u32 {
    let (_, _, core_color, _) = glow_style(glow);
    if core_color != 0 {
        core_color
    } else {
        CONNECTOR_BASE_COLOR
    }
}

fn draw_arrow_right(writer: &mut TaskWriter, tip_x: u32, tip_y: u32, color: u32) {
    for step in 0..ARROW_SIZE {
        writer.fill_rect(
            tip_x.saturating_sub(step),
            tip_y.saturating_sub(step),
            1,
            step.saturating_mul(2).saturating_add(1),
            color,
        );
    }
}

fn draw_arrow_left(writer: &mut TaskWriter, tip_x: u32, tip_y: u32, color: u32) {
    for step in 0..ARROW_SIZE {
        writer.fill_rect(
            tip_x.saturating_add(step),
            tip_y.saturating_sub(step),
            1,
            step.saturating_mul(2).saturating_add(1),
            color,
        );
    }
}

fn draw_arrow_down(writer: &mut TaskWriter, tip_x: u32, tip_y: u32, color: u32) {
    for step in 0..ARROW_SIZE {
        writer.fill_rect(
            tip_x.saturating_sub(step),
            tip_y.saturating_sub(step),
            step.saturating_mul(2).saturating_add(1),
            1,
            color,
        );
    }
}

fn draw_arrow_up(writer: &mut TaskWriter, tip_x: u32, tip_y: u32, color: u32) {
    for step in 0..ARROW_SIZE {
        writer.fill_rect(
            tip_x.saturating_sub(step),
            tip_y.saturating_add(step),
            step.saturating_mul(2).saturating_add(1),
            1,
            color,
        );
    }
}

fn glow_style(glow: ConnectorGlow) -> (u32, u32, u32, u32) {
    match glow {
        ConnectorGlow::Off => (0, 0, 0, 0),
        ConnectorGlow::Active => (ACTIVE_GLOW_COLOR, 6, ACTIVE_CORE_COLOR, 2),
        ConnectorGlow::AfterglowStrong => (AFTERGLOW_STRONG_COLOR, 4, AFTERGLOW_STRONG_COLOR, 2),
        ConnectorGlow::AfterglowMedium => (AFTERGLOW_MEDIUM_COLOR, 4, AFTERGLOW_MEDIUM_COLOR, 2),
        ConnectorGlow::AfterglowWeak => (AFTERGLOW_WEAK_COLOR, 2, AFTERGLOW_WEAK_COLOR, 2),
        ConnectorGlow::Error => (ERROR_GLOW_COLOR, 6, ERROR_CORE_COLOR, 2),
    }
}

fn draw_outline(writer: &mut TaskWriter, x: u32, y: u32, width: u32, height: u32, color: u32) {
    writer.fill_rect(x, y, width, 2, color);
    writer.fill_rect(x, y + height.saturating_sub(2), width, 2, color);
    writer.fill_rect(x, y, 2, height, color);
    writer.fill_rect(x + width.saturating_sub(2), y, 2, height, color);
}

fn center_x(rect: &graphics::Region) -> u32 {
    rect.x + rect.width / 2
}

fn labelled_connector(
    id: ConnectorId,
    source: ModuleId,
    target: ModuleId,
    label: &'static str,
    points: [Point; 4],
    len: usize,
    render_mode: LabelRenderMode,
    label_segment_index: usize,
    label_side: LabelSide,
    label_side_offset: u32,
    label_along_offset: u32,
) -> LabelledConnectorLayout {
    let label_rect = compute_label_rect(
        label,
        render_mode,
        &points,
        label_segment_index,
        label_side,
        label_side_offset,
        label_along_offset,
    );
    LabelledConnectorLayout {
        id,
        source,
        target,
        label,
        render_mode,
        label_segment_index,
        label_side,
        label_side_offset,
        label_along_offset,
        label_rect,
        points,
        len,
    }
}

fn compute_label_rect(
    text: &str,
    render_mode: LabelRenderMode,
    points: &[Point; 4],
    segment_index: usize,
    side: LabelSide,
    side_offset: u32,
    along_offset: u32,
) -> graphics::Region {
    match render_mode {
        LabelRenderMode::ExternalLabel => {
            source_side_label_rect(text, points, segment_index, side, side_offset, along_offset)
        }
        LabelRenderMode::InlineCutoutLabel => {
            inline_cutout_label_rect(text, points, segment_index, along_offset)
        }
    }
}

fn source_side_label_rect(
    text: &str,
    points: &[Point; 4],
    segment_index: usize,
    side: LabelSide,
    side_offset: u32,
    along_offset: u32,
) -> graphics::Region {
    let start = points[segment_index];
    let end = points[segment_index + 1];
    let width = (text.len() as u32) * 8;
    let height = 8;

    let (x, y) = if start.x == end.x {
        let anchor_y = if start.y <= end.y {
            start.y.saturating_add(along_offset)
        } else {
            start.y.saturating_sub(height + along_offset)
        };
        let x = match side {
            LabelSide::Left => start.x.saturating_sub(width + side_offset),
            LabelSide::Right => start.x.saturating_add(side_offset),
            LabelSide::Above | LabelSide::Below => start.x.saturating_add(side_offset),
        };
        (x, anchor_y)
    } else {
        let x = if start.x <= end.x {
            start.x.saturating_add(along_offset)
        } else {
            start.x.saturating_sub(width + along_offset)
        };
        let y = match side {
            LabelSide::Above => start.y.saturating_sub(height + side_offset / 2),
            LabelSide::Below => start.y.saturating_add(side_offset / 2),
            LabelSide::Left | LabelSide::Right => start.y.saturating_sub(height + side_offset / 2),
        };
        (x, y)
    };

    graphics::Region::new(x, y, width, height)
}

fn inline_cutout_label_rect(
    text: &str,
    points: &[Point; 4],
    segment_index: usize,
    along_offset: u32,
) -> graphics::Region {
    let start = points[segment_index];
    let end = points[segment_index + 1];
    let width = (text.len() as u32) * 8;
    let height = 8;

    let (x, y) = if start.x == end.x {
        let anchor_y = if start.y <= end.y {
            start.y.saturating_add(along_offset)
        } else {
            start.y.saturating_sub(height + along_offset)
        };
        (start.x.saturating_sub(width / 2), anchor_y)
    } else {
        let anchor_x = if start.x <= end.x {
            start.x.saturating_add(along_offset)
        } else {
            start.x.saturating_sub(width + along_offset)
        };
        (anchor_x, start.y.saturating_sub(height / 2))
    };

    graphics::Region::new(x, y, width, height)
}

fn resolve_label_rects(
    connectors: &mut [LabelledConnectorLayout; LAYOUT_CONNECTOR_COUNT],
    protected_areas: &[graphics::Region],
) {
    let mut order = core::array::from_fn::<_, LAYOUT_CONNECTOR_COUNT, _>(|index| index);
    order.sort_by_key(|&index| {
        (
            label_overlap_group(connectors[index].id),
            preferred_label_anchor_key(&connectors[index]),
            index,
        )
    });

    let mut placed = [false; LAYOUT_CONNECTOR_COUNT];
    for index in order {
        let base_side = connectors[index].label_side_offset;
        let base_along = connectors[index].label_along_offset;
        let mut resolved = None;

        'search: for side_step in 0..LABEL_STAGGER_MAX_SIDE_STEPS {
            let side_offset =
                base_side.saturating_add((side_step as u32) * LABEL_STAGGER_SECONDARY_STEP);
            for along_step in 0..LABEL_STAGGER_MAX_ALONG_STEPS {
                let along_offset =
                    base_along.saturating_add((along_step as u32) * LABEL_STAGGER_STEP);
                let candidate = compute_label_rect(
                    connectors[index].label,
                    connectors[index].render_mode,
                    &connectors[index].points,
                    connectors[index].label_segment_index,
                    connectors[index].label_side,
                    side_offset,
                    along_offset,
                );
                if label_rect_is_available(candidate, index, connectors, &placed, protected_areas) {
                    resolved = Some((candidate, side_offset, along_offset));
                    break 'search;
                }
            }
        }

        if let Some((label_rect, side_offset, along_offset)) = resolved {
            connectors[index].label_rect = label_rect;
            connectors[index].label_side_offset = side_offset;
            connectors[index].label_along_offset = along_offset;
        }
        placed[index] = true;
    }
}

fn label_overlap_group(id: ConnectorId) -> LabelOverlapGroup {
    match id {
        ConnectorId::OsToTransferRingWrite | ConnectorId::OsToEventRingRead => {
            LabelOverlapGroup::OsDown
        }
        ConnectorId::OsToXhciDoorbell => LabelOverlapGroup::OsRight,
        ConnectorId::XhciToTransferRingRead
        | ConnectorId::XhciToReportDmaWrite
        | ConnectorId::XhciToEventRingWrite => LabelOverlapGroup::XhciDown,
        ConnectorId::XhciToKeyboardUsbPoll => LabelOverlapGroup::XhciRight,
        ConnectorId::XhciToOsInterrupt => LabelOverlapGroup::XhciLeft,
    }
}

fn preferred_label_anchor_key(connector: &LabelledConnectorLayout) -> u32 {
    let start = connector.points[connector.label_segment_index];
    let end = connector.points[connector.label_segment_index + 1];
    if start.x == end.x {
        start.y.min(end.y)
    } else {
        start.x.min(end.x)
    }
}

fn label_rect_is_available(
    candidate: graphics::Region,
    current_index: usize,
    connectors: &[LabelledConnectorLayout; LAYOUT_CONNECTOR_COUNT],
    placed: &[bool; LAYOUT_CONNECTOR_COUNT],
    protected_areas: &[graphics::Region],
) -> bool {
    let padded = expand_region(candidate, LABEL_CLEARANCE);
    if protected_areas
        .iter()
        .copied()
        .map(|region| expand_region(region, LABEL_CLEARANCE))
        .any(|region| regions_overlap(padded, region))
    {
        return false;
    }

    for (index, connector) in connectors.iter().enumerate() {
        if index != current_index && placed[index] {
            if regions_overlap(padded, expand_region(connector.label_rect, LABEL_CLEARANCE)) {
                return false;
            }
        }

        if index == current_index {
            continue;
        }

        for segment in connector.points[..connector.len].windows(2) {
            if segment_crosses_box(segment[0], segment[1], padded) {
                return false;
            }
        }
    }

    true
}

fn expand_region(region: graphics::Region, padding: u32) -> graphics::Region {
    graphics::Region::new(
        region.x.saturating_sub(padding),
        region.y.saturating_sub(padding),
        region.width.saturating_add(padding.saturating_mul(2)),
        region.height.saturating_add(padding.saturating_mul(2)),
    )
}

fn regions_overlap(left: graphics::Region, right: graphics::Region) -> bool {
    let left_x1 = left.x + left.width;
    let left_y1 = left.y + left.height;
    let right_x1 = right.x + right.width;
    let right_y1 = right.y + right.height;

    left.x < right_x1 && right.x < left_x1 && left.y < right_y1 && right.y < left_y1
}

fn segment_crosses_box(start: Point, end: Point, rect: graphics::Region) -> bool {
    let left = rect.x;
    let right = rect.x + rect.width;
    let top = rect.y;
    let bottom = rect.y + rect.height;

    if start.x == end.x {
        let x = start.x;
        let y0 = start.y.min(end.y);
        let y1 = start.y.max(end.y);
        x > left && x < right && y0 < bottom && y1 > top
    } else if start.y == end.y {
        let y = start.y;
        let x0 = start.x.min(end.x);
        let x1 = start.x.max(end.x);
        y > top && y < bottom && x0 < right && x1 > left
    } else {
        false
    }
}

fn border_for_module(state: ModuleState) -> u32 {
    match state {
        ModuleState::Idle => BORDER_COLOR,
        ModuleState::Active => ACTIVE_COLOR,
        ModuleState::Complete => SUCCESS_COLOR,
        ModuleState::Error => ERROR_COLOR,
    }
}

fn draw_dim_text(writer: &mut TaskWriter, x: u32, y: u32, text: &str) {
    writer.set_color(DIM_TEXT_COLOR);
    writer.draw_string_at(x, y, text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics;
    use crate::usb::xhci::ring::{ConsumerRingSnapshot, ProducerRingSnapshot, RingSlotSnapshot};
    use crate::usb::xhci::trb::trb_type;

    fn test_snapshot() -> InputTraceSnapshot {
        InputTraceSnapshot {
            status: TraceStatusSnapshot {
                enabled: true,
                active_keyboard_present: true,
                ..TraceStatusSnapshot::default()
            },
            connectors: default_connectors(),
            modules: default_modules(),
            ..InputTraceSnapshot::default()
        }
    }

    fn fake_path() -> KeyboardInputPathSnapshot {
        let mut interrupt_slots = [RingSlotSnapshot::empty(0); 32];
        interrupt_slots[0] = RingSlotSnapshot {
            index: 0,
            occupied: true,
            trb_type: 1,
            cycle_bit: true,
            is_enqueue: true,
            is_reclaim: false,
            is_dequeue: false,
            is_recent: false,
            is_error: false,
            is_link: false,
        };
        interrupt_slots[31] = RingSlotSnapshot {
            index: 31,
            occupied: true,
            trb_type: trb_type::LINK,
            cycle_bit: true,
            is_enqueue: false,
            is_reclaim: false,
            is_dequeue: false,
            is_recent: false,
            is_error: false,
            is_link: true,
        };
        let mut event_slots = [RingSlotSnapshot::empty(0); 256];
        event_slots[0] = RingSlotSnapshot {
            index: 0,
            occupied: true,
            trb_type: 32,
            cycle_bit: true,
            is_enqueue: false,
            is_reclaim: false,
            is_dequeue: true,
            is_recent: false,
            is_error: false,
            is_link: false,
        };
        KeyboardInputPathSnapshot {
            last_report: [0; 8],
            interrupt_ring: ProducerRingSnapshot {
                phys_addr: 0x1000,
                capacity: 31,
                enqueue_index: 0,
                reclaim_index: 0,
                outstanding: 1,
                producer_cycle_state: true,
                slot_count: 32,
                slots: interrupt_slots,
            },
            report_buffer_len: 8,
            pending_trb_pointer: Some(0x1000),
            event_ring: ConsumerRingSnapshot {
                phys_addr: 0x2000,
                trb_count: 256,
                dequeue_index: 1,
                dequeue_pointer: 0x2010,
                consumer_cycle_state: true,
                slot_count: 256,
                slots: event_slots,
            },
            event_overflowed: false,
        }
    }

    #[test_case]
    fn test_disabled_trace_is_noop() {
        reset_for_test();
        record_transfer_queued(3, 2, 0x2000);
        let snapshot = snapshot();
        assert_eq!(snapshot.status.stored_records, 0);
        assert_eq!(snapshot.latest_transfer, None);
    }

    #[test_case]
    fn test_usb_cycle_records_successful_trace() {
        reset_for_test();
        set_enabled(true);
        record_transfer_queued(3, 2, 0x2000);
        record_transfer_doorbell(3, 2, 0x2000);
        record_interrupt_notify();
        record_transfer_event(3, 2, 0x2000, CompletionCode::Success, 0);
        record_report_ready([1; 8], 8);

        let snapshot = snapshot();
        let record = snapshot.latest_transfer.expect("latest trace");
        assert_eq!(snapshot.status.stored_records, 1);
        assert_eq!(record.report, [1; 8]);
        assert_eq!(record.report_bytes, 8);
        assert!(record.transfer_ring_write_at.is_some());
        assert!(record.doorbell_at.is_some());
        assert!(record.transfer_ring_read_at.is_some());
        assert!(record.event_ring_write_at.is_some());
        assert!(record.interrupt_notify_at.is_some());
        assert!(record.report_dma_write_at.is_some());
        assert_eq!(snapshot.latest_stage, Some(DiagramStage::ReportDmaWrite));
    }

    #[test_case]
    fn test_committed_report_stays_visible_until_next_report_ready() {
        reset_for_test();
        set_enabled(true);

        record_transfer_queued(3, 2, 0x2000);
        record_transfer_doorbell(3, 2, 0x2000);
        record_transfer_event(3, 2, 0x2000, CompletionCode::Success, 0);
        record_report_ready([0x11; 8], 8);

        let first = snapshot();
        assert_eq!(first.last_committed_report.bytes, [0x11; 8]);
        assert_eq!(first.last_committed_report.len, 8);

        record_transfer_queued(3, 2, 0x2010);
        let second = snapshot();
        assert_eq!(second.last_committed_report.bytes, [0x11; 8]);
        assert_eq!(second.last_committed_report.len, 8);

        record_transfer_doorbell(3, 2, 0x2010);
        record_transfer_event(3, 2, 0x2010, CompletionCode::Success, 0);
        record_report_ready([0x22; 8], 8);

        let third = snapshot();
        assert_eq!(third.last_committed_report.bytes, [0x22; 8]);
        assert_eq!(third.last_committed_report.len, 8);
    }

    #[test_case]
    fn test_connector_glow_progresses_across_usb_stages() {
        reset_for_test();
        set_enabled(true);

        record_transfer_queued(3, 2, 0x2000);
        let first = snapshot();
        assert_eq!(
            connector_glow(&first, ConnectorId::OsToTransferRingWrite),
            ConnectorGlow::Active
        );

        record_transfer_doorbell(3, 2, 0x2000);
        let second = snapshot();
        assert_eq!(
            connector_glow(&second, ConnectorId::OsToXhciDoorbell),
            ConnectorGlow::Active
        );
        assert_eq!(
            connector_glow(&second, ConnectorId::XhciToTransferRingRead),
            ConnectorGlow::Active
        );

        record_transfer_event(3, 2, 0x2000, CompletionCode::Success, 0);
        let third = snapshot();
        assert_eq!(
            connector_glow(&third, ConnectorId::XhciToEventRingWrite),
            ConnectorGlow::Active
        );

        record_interrupt_notify();
        let fourth = snapshot();
        assert_eq!(
            connector_glow(&fourth, ConnectorId::XhciToOsInterrupt),
            ConnectorGlow::Active
        );

        record_event_ring_os_read(3, 2);
        let fifth = snapshot();
        assert_eq!(
            connector_glow(&fifth, ConnectorId::OsToEventRingRead),
            ConnectorGlow::Active
        );

        record_report_ready([1; 8], 8);
        let fifth = snapshot();
        assert_eq!(
            connector_glow(&fifth, ConnectorId::XhciToReportDmaWrite),
            ConnectorGlow::Active
        );
        assert_eq!(
            connector_glow(&fifth, ConnectorId::XhciToKeyboardUsbPoll),
            ConnectorGlow::Active
        );
    }

    #[test_case]
    fn test_event_ring_read_does_not_create_extra_highlight_state() {
        reset_for_test();
        set_enabled(true);

        record_transfer_queued(3, 2, 0x2000);
        record_transfer_doorbell(3, 2, 0x2000);
        record_transfer_event(3, 2, 0x2000, CompletionCode::Success, 0);
        record_event_ring_os_read(3, 2);

        let snapshot = snapshot();
        assert_eq!(
            connector_glow(&snapshot, ConnectorId::OsToEventRingRead),
            ConnectorGlow::Active
        );
        assert_eq!(
            module_state(&snapshot, ModuleId::TransferRing),
            ModuleState::Complete
        );
        assert_eq!(
            module_state(&snapshot, ModuleId::EventRing),
            ModuleState::Active
        );
    }

    #[test_case]
    fn test_poll_connector_pulses_when_enabled_and_keyboard_present() {
        reset_for_test();
        set_enabled(true);

        let mut snapshot = test_snapshot();
        snapshot_into(&mut snapshot);
        assert_ne!(
            connector_glow(&snapshot, ConnectorId::XhciToKeyboardUsbPoll),
            ConnectorGlow::Off
        );
    }

    #[test_case]
    fn test_transfer_failure_marks_event_ring_error() {
        reset_for_test();
        set_enabled(true);
        record_transfer_queued(5, 2, 0x3000);
        record_transfer_doorbell(5, 2, 0x3000);
        record_interrupt_notify();
        record_transfer_failure(5, 2, 0x3000, CompletionCode::UsbTransactionError, 8);

        let snapshot = snapshot();
        assert_eq!(snapshot.latest_stage, Some(DiagramStage::Error));
        assert_eq!(
            connector_glow(&snapshot, ConnectorId::XhciToEventRingWrite),
            ConnectorGlow::Error
        );
        assert_eq!(
            module_state(&snapshot, ModuleId::EventRing),
            ModuleState::Error
        );
    }

    #[test_case]
    fn test_layout_boxes_do_not_overlap() {
        let layout = controller_diagram_layout(640, 720);
        let boxes = [
            layout.os_box,
            layout.xhci_box,
            layout.keyboard_box,
            layout.transfer_box,
            layout.report_box,
            layout.event_box,
        ];

        for (index, left) in boxes.iter().enumerate() {
            for right in boxes.iter().skip(index + 1) {
                assert!(!regions_overlap(*left, *right));
            }
        }
    }

    #[test_case]
    fn test_layout_parallel_os_xhci_lines_are_disjoint() {
        let layout = controller_diagram_layout(640, 720);
        let doorbell = layout
            .connectors
            .iter()
            .find(|connector| connector.id == ConnectorId::OsToXhciDoorbell)
            .expect("doorbell line");
        let interrupt = layout
            .connectors
            .iter()
            .find(|connector| connector.id == ConnectorId::XhciToOsInterrupt)
            .expect("interrupt line");

        assert_eq!(doorbell.len, 2);
        assert_eq!(interrupt.len, 2);
        assert_ne!(doorbell.points[0].y, interrupt.points[0].y);
    }

    #[test_case]
    fn test_layout_labels_stay_near_their_own_source_side_segment() {
        let layout = controller_diagram_layout(640, 720);

        for connector in &layout.connectors {
            let center = region_center(connector.label_rect);
            let own = point_to_segment_distance(
                center,
                connector.points[connector.label_segment_index],
                connector.points[connector.label_segment_index + 1],
            );

            for other in &layout.connectors {
                if connector.id == other.id {
                    continue;
                }
                let other_distance = min_distance_to_connector(center, other);
                assert!(
                    own < other_distance,
                    "label for {:?} is not closest to its own source-side segment",
                    connector.id
                );
            }
        }
    }

    #[test_case]
    fn test_external_label_positions_match_expected_sides() {
        let layout = controller_diagram_layout(640, 720);

        let write = find_connector(&layout, ConnectorId::OsToTransferRingWrite);
        let poll = find_connector(&layout, ConnectorId::XhciToKeyboardUsbPoll);
        let doorbell = find_connector(&layout, ConnectorId::OsToXhciDoorbell);
        let interrupt = find_connector(&layout, ConnectorId::XhciToOsInterrupt);

        assert_eq!(write.render_mode, LabelRenderMode::ExternalLabel);
        assert_eq!(doorbell.render_mode, LabelRenderMode::ExternalLabel);
        assert_eq!(poll.render_mode, LabelRenderMode::ExternalLabel);
        assert_eq!(interrupt.render_mode, LabelRenderMode::ExternalLabel);

        assert!(
            write.label_rect.x + write.label_rect.width
                <= write.points[write.label_segment_index].x
        );
        assert!(
            doorbell.label_rect.x
                < doorbell.points[0].x + (doorbell.points[1].x - doorbell.points[0].x) / 2
        );
        assert!(poll.label_rect.x < poll.points[0].x + (poll.points[1].x - poll.points[0].x) / 2);
        assert!(
            interrupt.label_rect.x + interrupt.label_rect.width
                > interrupt.points[1].x + (interrupt.points[0].x - interrupt.points[1].x) / 2
        );
    }

    #[test_case]
    fn test_inline_label_modes_and_positions_match_expected_segments() {
        let layout = controller_diagram_layout(640, 720);
        let fetch = find_connector(&layout, ConnectorId::XhciToTransferRingRead);
        let dma_write = find_connector(&layout, ConnectorId::XhciToReportDmaWrite);
        let event_write = find_connector(&layout, ConnectorId::XhciToEventRingWrite);
        let read = find_connector(&layout, ConnectorId::OsToEventRingRead);

        for connector in [fetch, dma_write, event_write, read] {
            assert_eq!(connector.render_mode, LabelRenderMode::InlineCutoutLabel);
            assert_eq!(connector.label_segment_index, 0);

            let background = connector_label_background_rect(connector);
            let x = connector.points[0].x;
            assert!(background.x <= x);
            assert!(background.x + background.width >= x);

            let y0 = connector.points[0].y.min(connector.points[1].y);
            let y1 = connector.points[0].y.max(connector.points[1].y);
            let center_y = connector.label_rect.y + connector.label_rect.height / 2;
            assert!(center_y >= y0);
            assert!(center_y <= y1);
        }
    }

    #[test_case]
    fn test_dma_write_path_is_orthogonal_and_avoids_report_text_areas() {
        let layout = controller_diagram_layout(640, 720);
        let dma_write = find_connector(&layout, ConnectorId::XhciToReportDmaWrite);

        assert_eq!(dma_write.len, 4);
        for segment in dma_write.points[..dma_write.len].windows(2) {
            assert!(
                segment[0].x == segment[1].x || segment[0].y == segment[1].y,
                "dma write path must stay orthogonal"
            );
            assert!(
                !segment_crosses_box(segment[0], segment[1], layout.report_status_area),
                "dma write crossed report status area"
            );
            assert!(
                !segment_crosses_box(segment[0], segment[1], layout.report_bytes_area),
                "dma write crossed report bytes area"
            );
        }
    }

    #[test_case]
    fn test_xhci_down_labels_do_not_overlap_and_stay_on_segment_zero() {
        let layout = controller_diagram_layout(640, 720);
        let fetch = find_connector(&layout, ConnectorId::XhciToTransferRingRead);
        let dma_write = find_connector(&layout, ConnectorId::XhciToReportDmaWrite);
        let event_write = find_connector(&layout, ConnectorId::XhciToEventRingWrite);

        assert_eq!(fetch.label_segment_index, 0);
        assert_eq!(dma_write.label_segment_index, 0);
        assert_eq!(event_write.label_segment_index, 0);

        assert!(!regions_overlap(fetch.label_rect, dma_write.label_rect));
        assert!(!regions_overlap(fetch.label_rect, event_write.label_rect));
        assert!(!regions_overlap(
            dma_write.label_rect,
            event_write.label_rect
        ));
    }

    #[test_case]
    fn test_layout_labels_avoid_all_module_boxes() {
        let layout = controller_diagram_layout(640, 720);
        let boxes = [
            layout.os_box,
            layout.xhci_box,
            layout.keyboard_box,
            layout.transfer_box,
            layout.report_box,
            layout.event_box,
        ];

        for connector in &layout.connectors {
            for rect in boxes {
                assert!(
                    !regions_overlap(connector.label_rect, rect),
                    "label for {:?} overlapped a module box",
                    connector.id
                );
            }
        }
    }

    #[test_case]
    fn test_connector_arrow_modes_match_diagram_rules() {
        assert_eq!(
            connector_arrow_mode(ConnectorId::OsToTransferRingWrite),
            ConnectorArrowMode::SingleToTarget
        );
        assert_eq!(
            connector_arrow_mode(ConnectorId::OsToXhciDoorbell),
            ConnectorArrowMode::SingleToTarget
        );
        assert_eq!(
            connector_arrow_mode(ConnectorId::XhciToTransferRingRead),
            ConnectorArrowMode::SingleToTarget
        );
        assert_eq!(
            connector_arrow_mode(ConnectorId::XhciToReportDmaWrite),
            ConnectorArrowMode::SingleToTarget
        );
        assert_eq!(
            connector_arrow_mode(ConnectorId::XhciToEventRingWrite),
            ConnectorArrowMode::SingleToTarget
        );
        assert_eq!(
            connector_arrow_mode(ConnectorId::XhciToOsInterrupt),
            ConnectorArrowMode::SingleToTarget
        );
        assert_eq!(
            connector_arrow_mode(ConnectorId::OsToEventRingRead),
            ConnectorArrowMode::SingleToTarget
        );
        assert_eq!(
            connector_arrow_mode(ConnectorId::XhciToKeyboardUsbPoll),
            ConnectorArrowMode::Bidirectional
        );
    }

    #[test_case]
    fn test_inline_label_cutout_backgrounds_do_not_overlap() {
        let layout = controller_diagram_layout(640, 720);
        let fetch = connector_label_background_rect(find_connector(
            &layout,
            ConnectorId::XhciToTransferRingRead,
        ));
        let dma_write = connector_label_background_rect(find_connector(
            &layout,
            ConnectorId::XhciToReportDmaWrite,
        ));
        let event_write = connector_label_background_rect(find_connector(
            &layout,
            ConnectorId::XhciToEventRingWrite,
        ));

        assert!(!regions_overlap(fetch, dma_write));
        assert!(!regions_overlap(fetch, event_write));
        assert!(!regions_overlap(dma_write, event_write));
    }

    #[test_case]
    fn test_layout_connectors_do_not_cross_unrelated_boxes() {
        let layout = controller_diagram_layout(640, 720);
        let boxes = [
            (ModuleId::Os, layout.os_box),
            (ModuleId::Xhci, layout.xhci_box),
            (ModuleId::KeyboardUsbDevice, layout.keyboard_box),
            (ModuleId::TransferRing, layout.transfer_box),
            (ModuleId::ReportBuffer, layout.report_box),
            (ModuleId::EventRing, layout.event_box),
        ];

        for connector in &layout.connectors {
            for segment in connector.points[..connector.len].windows(2) {
                let start = segment[0];
                let end = segment[1];
                for (module, rect) in boxes {
                    if module == connector.source || module == connector.target {
                        continue;
                    }
                    assert!(
                        !segment_crosses_box(start, end, rect),
                        "connector {:?} crossed {:?}",
                        connector.id,
                        module
                    );
                }
            }
        }
    }

    #[test_case]
    fn test_layout_connectors_do_not_cross_other_connector_labels() {
        let layout = controller_diagram_layout(640, 720);

        for connector in &layout.connectors {
            for segment in connector.points[..connector.len].windows(2) {
                let start = segment[0];
                let end = segment[1];
                for other in &layout.connectors {
                    if connector.id == other.id {
                        continue;
                    }
                    assert!(
                        !segment_crosses_box(start, end, other.label_rect),
                        "connector {:?} crossed label for {:?}",
                        connector.id,
                        other.id
                    );
                }
            }
        }
    }

    #[test_case]
    fn test_layout_connectors_avoid_report_payload_text_areas() {
        let layout = controller_diagram_layout(640, 720);

        for connector in &layout.connectors {
            for segment in connector.points[..connector.len].windows(2) {
                let start = segment[0];
                let end = segment[1];
                assert!(
                    !segment_crosses_box(start, end, layout.report_status_area),
                    "connector {:?} crossed report status area",
                    connector.id
                );
                assert!(
                    !segment_crosses_box(start, end, layout.report_bytes_area),
                    "connector {:?} crossed report bytes area",
                    connector.id
                );
            }
        }
    }

    #[test_case]
    fn test_ring_cells_use_ring_snapshots() {
        let mut snapshot = test_snapshot();
        snapshot.keyboard_path = Some(Box::new(fake_path()));
        set_connector_glow(
            &mut snapshot.connectors,
            ConnectorId::OsToTransferRingWrite,
            ConnectorGlow::Active,
        );
        set_connector_glow(
            &mut snapshot.connectors,
            ConnectorId::XhciToEventRingWrite,
            ConnectorGlow::Active,
        );

        let path = snapshot.keyboard_path.as_deref().expect("path");
        let record = TraceRecord {
            id: 1,
            report: [0; 8],
            report_bytes: 8,
            transfer_ring_write_at: None,
            doorbell_at: None,
            transfer_ring_read_at: None,
            report_dma_write_at: None,
            event_ring_write_at: Some(current_timestamp()),
            interrupt_notify_at: None,
            event_ring_os_read_at: Some(current_timestamp()),
            transfer_event: Some(TransferEventSnapshot {
                slot_id: 3,
                endpoint_id: 2,
                trb_pointer: 0x1000,
                completion_code: CompletionCode::Success,
                transfer_length: 0,
            }),
            transfer_failure: false,
        };
        let transfer = transfer_ring_cells(&snapshot, path, Some(record));
        let event = event_ring_cells(&snapshot, path, Some(record));

        assert!(transfer.iter().any(|cell| cell.enqueue));
        assert!(event.iter().any(|cell| cell.dequeue));
        assert!(transfer.iter().any(|cell| cell.occupied));
        assert!(event.iter().any(|cell| cell.occupied));
        assert!(!transfer.iter().any(|cell| cell.error));
        assert!(!event.iter().any(|cell| cell.error));
    }

    #[test_case]
    fn test_link_trb_uses_normal_occupied_colors() {
        let link_cell = RingDiagramCell {
            occupied: true,
            link: true,
            enqueue: false,
            reclaim: false,
            dequeue: false,
            error: false,
        };

        assert_eq!(ring_cell_fill_color(link_cell), SLOT_FILLED_COLOR);
        assert_eq!(ring_cell_border_color(link_cell), BORDER_COLOR);
    }

    #[test_case]
    fn test_transfer_ring_hides_link_trb_slot_from_visible_cells() {
        let path = fake_path();
        let cells = transfer_ring_cells(&test_snapshot(), &path, None);
        let visible = transfer_ring_visible_slot_count(&path, &cells);

        assert_eq!(visible, path.interrupt_ring.capacity);
        assert_eq!(visible, 31);
        assert!(!cells[..visible].iter().any(|cell| cell.link));
        assert!(!cells[..visible].iter().any(|cell| cell.error));
        assert!(
            cells[visible..]
                .iter()
                .all(|cell| !cell.occupied && !cell.link)
        );
    }

    fn find_connector(
        layout: &ControllerDiagramLayout,
        id: ConnectorId,
    ) -> &LabelledConnectorLayout {
        layout
            .connectors
            .iter()
            .find(|connector| connector.id == id)
            .expect("connector")
    }

    fn region_center(rect: graphics::Region) -> Point {
        Point {
            x: rect.x + rect.width / 2,
            y: rect.y + rect.height / 2,
        }
    }

    fn min_distance_to_connector(point: Point, connector: &LabelledConnectorLayout) -> u32 {
        connector.points[..connector.len]
            .windows(2)
            .map(|segment| point_to_segment_distance(point, segment[0], segment[1]))
            .min()
            .unwrap_or(u32::MAX)
    }

    fn point_to_segment_distance(point: Point, start: Point, end: Point) -> u32 {
        if start.x == end.x {
            let y0 = start.y.min(end.y);
            let y1 = start.y.max(end.y);
            let dx = point.x.abs_diff(start.x);
            let dy = if point.y < y0 {
                y0 - point.y
            } else if point.y > y1 {
                point.y - y1
            } else {
                0
            };
            dx + dy
        } else if start.y == end.y {
            let x0 = start.x.min(end.x);
            let x1 = start.x.max(end.x);
            let dy = point.y.abs_diff(start.y);
            let dx = if point.x < x0 {
                x0 - point.x
            } else if point.x > x1 {
                point.x - x1
            } else {
                0
            };
            dx + dy
        } else {
            u32::MAX
        }
    }
}
