use crate::input::{KeyCode, KeyEvent, KeyEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardLayout {
    Us101,
}

impl KeyboardLayout {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Us101 => "US",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputEvent {
    Insert(char),
    Backspace,
    Commit,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextInputEngine {
    layout: KeyboardLayout,
}

impl TextInputEngine {
    pub const fn new(layout: KeyboardLayout) -> Self {
        Self { layout }
    }

    pub const fn layout(&self) -> KeyboardLayout {
        self.layout
    }

    pub fn translate_key_event(&mut self, event: KeyEvent) -> TextInputEvent {
        if event.kind != KeyEventKind::Press {
            return TextInputEvent::Unsupported;
        }

        match self.layout {
            KeyboardLayout::Us101 => translate_us101_press(event),
        }
    }
}

impl Default for TextInputEngine {
    fn default() -> Self {
        Self::new(KeyboardLayout::Us101)
    }
}

fn translate_us101_press(event: KeyEvent) -> TextInputEvent {
    let shifted = event.modifiers.shift;
    match event.code {
        KeyCode::A => TextInputEvent::Insert(if shifted { 'A' } else { 'a' }),
        KeyCode::B => TextInputEvent::Insert(if shifted { 'B' } else { 'b' }),
        KeyCode::C => TextInputEvent::Insert(if shifted { 'C' } else { 'c' }),
        KeyCode::D => TextInputEvent::Insert(if shifted { 'D' } else { 'd' }),
        KeyCode::E => TextInputEvent::Insert(if shifted { 'E' } else { 'e' }),
        KeyCode::F => TextInputEvent::Insert(if shifted { 'F' } else { 'f' }),
        KeyCode::G => TextInputEvent::Insert(if shifted { 'G' } else { 'g' }),
        KeyCode::H => TextInputEvent::Insert(if shifted { 'H' } else { 'h' }),
        KeyCode::I => TextInputEvent::Insert(if shifted { 'I' } else { 'i' }),
        KeyCode::J => TextInputEvent::Insert(if shifted { 'J' } else { 'j' }),
        KeyCode::K => TextInputEvent::Insert(if shifted { 'K' } else { 'k' }),
        KeyCode::L => TextInputEvent::Insert(if shifted { 'L' } else { 'l' }),
        KeyCode::M => TextInputEvent::Insert(if shifted { 'M' } else { 'm' }),
        KeyCode::N => TextInputEvent::Insert(if shifted { 'N' } else { 'n' }),
        KeyCode::O => TextInputEvent::Insert(if shifted { 'O' } else { 'o' }),
        KeyCode::P => TextInputEvent::Insert(if shifted { 'P' } else { 'p' }),
        KeyCode::Q => TextInputEvent::Insert(if shifted { 'Q' } else { 'q' }),
        KeyCode::R => TextInputEvent::Insert(if shifted { 'R' } else { 'r' }),
        KeyCode::S => TextInputEvent::Insert(if shifted { 'S' } else { 's' }),
        KeyCode::T => TextInputEvent::Insert(if shifted { 'T' } else { 't' }),
        KeyCode::U => TextInputEvent::Insert(if shifted { 'U' } else { 'u' }),
        KeyCode::V => TextInputEvent::Insert(if shifted { 'V' } else { 'v' }),
        KeyCode::W => TextInputEvent::Insert(if shifted { 'W' } else { 'w' }),
        KeyCode::X => TextInputEvent::Insert(if shifted { 'X' } else { 'x' }),
        KeyCode::Y => TextInputEvent::Insert(if shifted { 'Y' } else { 'y' }),
        KeyCode::Z => TextInputEvent::Insert(if shifted { 'Z' } else { 'z' }),
        KeyCode::Digit1 => TextInputEvent::Insert(if shifted { '!' } else { '1' }),
        KeyCode::Digit2 => TextInputEvent::Insert(if shifted { '@' } else { '2' }),
        KeyCode::Digit3 => TextInputEvent::Insert(if shifted { '#' } else { '3' }),
        KeyCode::Digit4 => TextInputEvent::Insert(if shifted { '$' } else { '4' }),
        KeyCode::Digit5 => TextInputEvent::Insert(if shifted { '%' } else { '5' }),
        KeyCode::Digit6 => TextInputEvent::Insert(if shifted { '^' } else { '6' }),
        KeyCode::Digit7 => TextInputEvent::Insert(if shifted { '&' } else { '7' }),
        KeyCode::Digit8 => TextInputEvent::Insert(if shifted { '*' } else { '8' }),
        KeyCode::Digit9 => TextInputEvent::Insert(if shifted { '(' } else { '9' }),
        KeyCode::Digit0 => TextInputEvent::Insert(if shifted { ')' } else { '0' }),
        KeyCode::Space => TextInputEvent::Insert(' '),
        KeyCode::Minus => TextInputEvent::Insert(if shifted { '_' } else { '-' }),
        KeyCode::Equal => TextInputEvent::Insert(if shifted { '+' } else { '=' }),
        KeyCode::LeftBracket => TextInputEvent::Insert(if shifted { '{' } else { '[' }),
        KeyCode::RightBracket => TextInputEvent::Insert(if shifted { '}' } else { ']' }),
        KeyCode::Backslash => TextInputEvent::Insert(if shifted { '|' } else { '\\' }),
        KeyCode::Semicolon => TextInputEvent::Insert(if shifted { ':' } else { ';' }),
        KeyCode::Apostrophe => TextInputEvent::Insert(if shifted { '"' } else { '\'' }),
        KeyCode::Grave => TextInputEvent::Insert(if shifted { '~' } else { '`' }),
        KeyCode::Comma => TextInputEvent::Insert(if shifted { '<' } else { ',' }),
        KeyCode::Period => TextInputEvent::Insert(if shifted { '>' } else { '.' }),
        KeyCode::Slash => TextInputEvent::Insert(if shifted { '?' } else { '/' }),
        KeyCode::Backspace => TextInputEvent::Backspace,
        KeyCode::Enter => TextInputEvent::Commit,
        _ => TextInputEvent::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyboardLayout, TextInputEngine, TextInputEvent};
    use crate::input::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

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
    fn test_keyboard_layout_label_uses_short_name() {
        assert_eq!(KeyboardLayout::Us101.as_str(), "US");
    }

    #[test_case]
    fn test_translate_key_event_maps_ascii_for_us_layout() {
        let mut engine = TextInputEngine::new(KeyboardLayout::Us101);

        assert_eq!(
            engine.translate_key_event(key_event(KeyCode::A, false)),
            TextInputEvent::Insert('a')
        );
        assert_eq!(
            engine.translate_key_event(key_event(KeyCode::Digit1, true)),
            TextInputEvent::Insert('!')
        );
        assert_eq!(
            engine.translate_key_event(key_event(KeyCode::Space, false)),
            TextInputEvent::Insert(' ')
        );
    }

    #[test_case]
    fn test_translate_key_event_maps_backspace_and_commit() {
        let mut engine = TextInputEngine::default();

        assert_eq!(
            engine.translate_key_event(key_event(KeyCode::Backspace, false)),
            TextInputEvent::Backspace
        );
        assert_eq!(
            engine.translate_key_event(key_event(KeyCode::Enter, false)),
            TextInputEvent::Commit
        );
    }

    #[test_case]
    fn test_translate_key_event_returns_unsupported_for_release_and_navigation_keys() {
        let mut engine = TextInputEngine::default();

        assert_eq!(
            engine.translate_key_event(key_event_with_kind(
                KeyCode::A,
                false,
                KeyEventKind::Release
            )),
            TextInputEvent::Unsupported
        );
        assert_eq!(
            engine.translate_key_event(key_event(KeyCode::ArrowLeft, false)),
            TextInputEvent::Unsupported
        );
        assert_eq!(
            engine.translate_key_event(key_event(KeyCode::Tab, false)),
            TextInputEvent::Unsupported
        );
    }
}
