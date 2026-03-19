use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::io::without_interrupts;
use crate::sync::wait_queue::WaitQueue;
use crate::warn;
use lazy_static::lazy_static;
use spin::Mutex;

const KEY_EVENT_QUEUE_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Digit0,
    Enter,
    Escape,
    Backspace,
    Tab,
    Space,
    Minus,
    Equal,
    LeftBracket,
    RightBracket,
    Backslash,
    Semicolon,
    Apostrophe,
    Grave,
    Comma,
    Period,
    Slash,
    CapsLock,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Insert,
    Home,
    PageUp,
    Delete,
    End,
    PageDown,
    ArrowRight,
    ArrowLeft,
    ArrowDown,
    ArrowUp,
    LeftCtrl,
    LeftShift,
    LeftAlt,
    LeftGui,
    RightCtrl,
    RightShift,
    RightAlt,
    RightGui,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KeyModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub gui: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventKind {
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub kind: KeyEventKind,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueuedKeyEvent {
    event: KeyEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PolledKeyEvent {
    pub event: KeyEvent,
}

lazy_static! {
    static ref KEY_EVENT_QUEUE: Mutex<VecDeque<QueuedKeyEvent>> = Mutex::new(VecDeque::new());
}

static KEY_EVENT_WAIT: WaitQueue = WaitQueue::new();
static KEY_EVENT_SIGNAL: AtomicBool = AtomicBool::new(false);

pub fn poll_key_event() -> Option<KeyEvent> {
    poll_key_event_internal().map(|entry| entry.event)
}

pub fn read_key_event() -> KeyEvent {
    loop {
        if let Some(entry) = poll_key_event_internal() {
            return entry.event;
        }
        wait_for_queue(&KEY_EVENT_SIGNAL, &KEY_EVENT_WAIT);
    }
}

pub(crate) fn push_key_event(event: KeyEvent) {
    push_queue(
        &KEY_EVENT_QUEUE,
        &KEY_EVENT_SIGNAL,
        &KEY_EVENT_WAIT,
        KEY_EVENT_QUEUE_CAPACITY,
        QueuedKeyEvent { event },
        "key event",
    );
}

pub(crate) fn poll_key_event_internal() -> Option<PolledKeyEvent> {
    pop_queue(&KEY_EVENT_QUEUE, &KEY_EVENT_SIGNAL)
        .map(|entry| PolledKeyEvent { event: entry.event })
}

fn wait_for_queue(signal: &AtomicBool, wait_queue: &WaitQueue) {
    loop {
        if signal.swap(false, Ordering::AcqRel) {
            return;
        }
        wait_queue.wait();
    }
}

fn push_queue<T: Copy>(
    queue: &Mutex<VecDeque<T>>,
    signal: &AtomicBool,
    wait_queue: &WaitQueue,
    capacity: usize,
    value: T,
    queue_name: &'static str,
) -> Option<usize> {
    let result = without_interrupts(|| {
        let mut queue = queue.lock();
        if queue.len() >= capacity {
            return None;
        }
        queue.push_back(value);
        Some(queue.len())
    });

    let Some(len) = result else {
        warn!("[Input] Dropping {} because queue is full", queue_name);
        return None;
    };

    signal.store(true, Ordering::Release);
    wait_queue.wake_one();
    Some(len)
}

fn pop_queue<T: Copy>(queue: &Mutex<VecDeque<T>>, signal: &AtomicBool) -> Option<T> {
    without_interrupts(|| {
        let mut queue = queue.lock();
        let value = queue.pop_front();
        signal.store(!queue.is_empty(), Ordering::Release);
        value
    })
}

#[cfg(test)]
fn reset_for_test() {
    without_interrupts(|| {
        KEY_EVENT_QUEUE.lock().clear();
    });
    KEY_EVENT_SIGNAL.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::{
        KEY_EVENT_QUEUE_CAPACITY, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, poll_key_event,
        push_key_event, reset_for_test,
    };

    fn key_event_with_kind(code: KeyCode, shift: bool, kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code,
            kind,
            modifiers: KeyModifiers {
                shift,
                ..KeyModifiers::default()
            },
        }
    }

    fn key_event(code: KeyCode, shift: bool) -> KeyEvent {
        key_event_with_kind(code, shift, KeyEventKind::Press)
    }

    fn push_test_key_event(event: KeyEvent) {
        push_key_event(event);
    }

    #[test_case]
    fn test_poll_key_event_returns_enqueued_event() {
        reset_for_test();
        let event = key_event(KeyCode::B, false);
        push_test_key_event(event);
        assert_eq!(poll_key_event(), Some(event));
    }

    #[test_case]
    fn test_push_key_event_enqueues_only_key_events() {
        reset_for_test();
        let event = key_event(KeyCode::C, true);
        push_test_key_event(event);
        assert_eq!(poll_key_event(), Some(event));
        assert_eq!(poll_key_event(), None);
    }

    #[test_case]
    fn test_queue_overflow_drops_new_events() {
        reset_for_test();
        for _ in 0..(KEY_EVENT_QUEUE_CAPACITY + 4) {
            push_key_event(key_event(KeyCode::D, false));
        }

        let mut count = 0;
        while poll_key_event().is_some() {
            count += 1;
        }

        assert_eq!(count, KEY_EVENT_QUEUE_CAPACITY);
    }
}
