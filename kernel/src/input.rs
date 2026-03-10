use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::io::without_interrupts;
use crate::sync::wait_queue::WaitQueue;
use crate::warn;
use lazy_static::lazy_static;
use spin::Mutex;

const KEY_EVENT_QUEUE_CAPACITY: usize = 256;
const CHAR_QUEUE_CAPACITY: usize = 256;

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

lazy_static! {
    static ref KEY_EVENT_QUEUE: Mutex<VecDeque<KeyEvent>> = Mutex::new(VecDeque::new());
    static ref CHAR_QUEUE: Mutex<VecDeque<char>> = Mutex::new(VecDeque::new());
}

static KEY_EVENT_WAIT: WaitQueue = WaitQueue::new();
static KEY_EVENT_SIGNAL: AtomicBool = AtomicBool::new(false);
static CHAR_WAIT: WaitQueue = WaitQueue::new();
static CHAR_SIGNAL: AtomicBool = AtomicBool::new(false);

pub fn poll_key_event() -> Option<KeyEvent> {
    pop_queue(&KEY_EVENT_QUEUE, &KEY_EVENT_SIGNAL)
}

pub fn read_key_event() -> KeyEvent {
    loop {
        if let Some(event) = poll_key_event() {
            return event;
        }
        wait_for_queue(&KEY_EVENT_SIGNAL, &KEY_EVENT_WAIT);
    }
}

pub fn getchar() -> char {
    loop {
        if let Some(ch) = poll_char() {
            return ch;
        }
        wait_for_queue(&CHAR_SIGNAL, &CHAR_WAIT);
    }
}

pub fn key_event_to_ascii(event: KeyEvent) -> Option<char> {
    if event.kind != KeyEventKind::Press {
        return None;
    }

    match key_event_to_char(event) {
        Some(ch @ ' '..='~') => Some(ch),
        _ => None,
    }
}

pub(crate) fn push_key_event(event: KeyEvent) {
    push_queue(
        &KEY_EVENT_QUEUE,
        &KEY_EVENT_SIGNAL,
        &KEY_EVENT_WAIT,
        KEY_EVENT_QUEUE_CAPACITY,
        event,
        "key event",
    );

    if event.kind == KeyEventKind::Press {
        if let Some(ch) = key_event_to_char(event) {
            push_queue(
                &CHAR_QUEUE,
                &CHAR_SIGNAL,
                &CHAR_WAIT,
                CHAR_QUEUE_CAPACITY,
                ch,
                "character",
            );
        }
    }
}

fn poll_char() -> Option<char> {
    pop_queue(&CHAR_QUEUE, &CHAR_SIGNAL)
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
) {
    let pushed = without_interrupts(|| {
        let mut queue = queue.lock();
        if queue.len() >= capacity {
            return false;
        }
        queue.push_back(value);
        true
    });

    if !pushed {
        warn!("[Input] Dropping {} because queue is full", queue_name);
        return;
    }

    signal.store(true, Ordering::Release);
    wait_queue.wake_one();
}

fn pop_queue<T: Copy>(queue: &Mutex<VecDeque<T>>, signal: &AtomicBool) -> Option<T> {
    without_interrupts(|| {
        let mut queue = queue.lock();
        let value = queue.pop_front();
        signal.store(!queue.is_empty(), Ordering::Release);
        value
    })
}

fn key_event_to_char(event: KeyEvent) -> Option<char> {
    let shifted = event.modifiers.shift;
    match event.code {
        KeyCode::A => Some(if shifted { 'A' } else { 'a' }),
        KeyCode::B => Some(if shifted { 'B' } else { 'b' }),
        KeyCode::C => Some(if shifted { 'C' } else { 'c' }),
        KeyCode::D => Some(if shifted { 'D' } else { 'd' }),
        KeyCode::E => Some(if shifted { 'E' } else { 'e' }),
        KeyCode::F => Some(if shifted { 'F' } else { 'f' }),
        KeyCode::G => Some(if shifted { 'G' } else { 'g' }),
        KeyCode::H => Some(if shifted { 'H' } else { 'h' }),
        KeyCode::I => Some(if shifted { 'I' } else { 'i' }),
        KeyCode::J => Some(if shifted { 'J' } else { 'j' }),
        KeyCode::K => Some(if shifted { 'K' } else { 'k' }),
        KeyCode::L => Some(if shifted { 'L' } else { 'l' }),
        KeyCode::M => Some(if shifted { 'M' } else { 'm' }),
        KeyCode::N => Some(if shifted { 'N' } else { 'n' }),
        KeyCode::O => Some(if shifted { 'O' } else { 'o' }),
        KeyCode::P => Some(if shifted { 'P' } else { 'p' }),
        KeyCode::Q => Some(if shifted { 'Q' } else { 'q' }),
        KeyCode::R => Some(if shifted { 'R' } else { 'r' }),
        KeyCode::S => Some(if shifted { 'S' } else { 's' }),
        KeyCode::T => Some(if shifted { 'T' } else { 't' }),
        KeyCode::U => Some(if shifted { 'U' } else { 'u' }),
        KeyCode::V => Some(if shifted { 'V' } else { 'v' }),
        KeyCode::W => Some(if shifted { 'W' } else { 'w' }),
        KeyCode::X => Some(if shifted { 'X' } else { 'x' }),
        KeyCode::Y => Some(if shifted { 'Y' } else { 'y' }),
        KeyCode::Z => Some(if shifted { 'Z' } else { 'z' }),
        KeyCode::Digit1 => Some(if shifted { '!' } else { '1' }),
        KeyCode::Digit2 => Some(if shifted { '@' } else { '2' }),
        KeyCode::Digit3 => Some(if shifted { '#' } else { '3' }),
        KeyCode::Digit4 => Some(if shifted { '$' } else { '4' }),
        KeyCode::Digit5 => Some(if shifted { '%' } else { '5' }),
        KeyCode::Digit6 => Some(if shifted { '^' } else { '6' }),
        KeyCode::Digit7 => Some(if shifted { '&' } else { '7' }),
        KeyCode::Digit8 => Some(if shifted { '*' } else { '8' }),
        KeyCode::Digit9 => Some(if shifted { '(' } else { '9' }),
        KeyCode::Digit0 => Some(if shifted { ')' } else { '0' }),
        KeyCode::Enter => Some('\n'),
        KeyCode::Backspace => Some('\x08'),
        KeyCode::Tab => Some('\t'),
        KeyCode::Space => Some(' '),
        KeyCode::Minus => Some(if shifted { '_' } else { '-' }),
        KeyCode::Equal => Some(if shifted { '+' } else { '=' }),
        KeyCode::LeftBracket => Some(if shifted { '{' } else { '[' }),
        KeyCode::RightBracket => Some(if shifted { '}' } else { ']' }),
        KeyCode::Backslash => Some(if shifted { '|' } else { '\\' }),
        KeyCode::Semicolon => Some(if shifted { ':' } else { ';' }),
        KeyCode::Apostrophe => Some(if shifted { '"' } else { '\'' }),
        KeyCode::Grave => Some(if shifted { '~' } else { '`' }),
        KeyCode::Comma => Some(if shifted { '<' } else { ',' }),
        KeyCode::Period => Some(if shifted { '>' } else { '.' }),
        KeyCode::Slash => Some(if shifted { '?' } else { '/' }),
        _ => None,
    }
}

#[cfg(test)]
fn reset_for_test() {
    without_interrupts(|| {
        KEY_EVENT_QUEUE.lock().clear();
        CHAR_QUEUE.lock().clear();
    });
    KEY_EVENT_SIGNAL.store(false, Ordering::Release);
    CHAR_SIGNAL.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::{
        KEY_EVENT_QUEUE_CAPACITY, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        key_event_to_ascii, key_event_to_char, poll_char, poll_key_event, push_key_event,
        reset_for_test,
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

    #[test_case]
    fn test_key_event_to_char_maps_shifted_letters_and_symbols() {
        assert_eq!(key_event_to_char(key_event(KeyCode::A, false)), Some('a'));
        assert_eq!(key_event_to_char(key_event(KeyCode::A, true)), Some('A'));
        assert_eq!(
            key_event_to_char(key_event(KeyCode::Digit1, true)),
            Some('!')
        );
        assert_eq!(
            key_event_to_char(key_event(KeyCode::Slash, true)),
            Some('?')
        );
        assert_eq!(
            key_event_to_char(key_event(KeyCode::Enter, false)),
            Some('\n')
        );
    }

    #[test_case]
    fn test_key_event_to_ascii_accepts_printable_press_events_only() {
        assert_eq!(key_event_to_ascii(key_event(KeyCode::A, false)), Some('a'));
        assert_eq!(
            key_event_to_ascii(key_event(KeyCode::Digit1, true)),
            Some('!')
        );
        assert_eq!(
            key_event_to_ascii(key_event(KeyCode::Space, false)),
            Some(' ')
        );
        assert_eq!(
            key_event_to_ascii(key_event_with_kind(
                KeyCode::A,
                false,
                KeyEventKind::Release
            )),
            None
        );
    }

    #[test_case]
    fn test_key_event_to_ascii_ignores_control_and_navigation_keys() {
        assert_eq!(key_event_to_ascii(key_event(KeyCode::Enter, false)), None);
        assert_eq!(
            key_event_to_ascii(key_event(KeyCode::Backspace, false)),
            None
        );
        assert_eq!(key_event_to_ascii(key_event(KeyCode::Tab, false)), None);
        assert_eq!(
            key_event_to_ascii(key_event(KeyCode::ArrowLeft, false)),
            None
        );
    }

    #[test_case]
    fn test_poll_key_event_returns_enqueued_event() {
        reset_for_test();
        let event = key_event(KeyCode::B, false);
        push_key_event(event);
        assert_eq!(poll_key_event(), Some(event));
    }

    #[test_case]
    fn test_push_key_event_enqueues_character_for_press() {
        reset_for_test();
        push_key_event(key_event(KeyCode::C, true));
        let _ = poll_key_event();
        assert_eq!(poll_char(), Some('C'));
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
