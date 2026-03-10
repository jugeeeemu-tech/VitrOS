use alloc::string::String;

use crate::input::{self, KeyCode, KeyEvent, KeyEventKind};

pub const MAX_LINE_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEditCommand {
    Insert(char),
    Backspace,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineEditResult {
    Ignored,
    LineChanged,
    LineCommitted(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineEditor {
    buffer: [u8; MAX_LINE_LEN],
    len: usize,
}

impl LineEditor {
    pub const fn new() -> Self {
        Self {
            buffer: [0; MAX_LINE_LEN],
            len: 0,
        }
    }

    pub fn current_line(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.len])
            .expect("line editor buffer must remain valid ASCII")
    }

    pub fn apply(&mut self, command: LineEditCommand) -> LineEditResult {
        match command {
            LineEditCommand::Insert(ch) => self.insert(ch),
            LineEditCommand::Backspace => self.backspace(),
            LineEditCommand::Commit => self.commit(),
        }
    }

    fn insert(&mut self, ch: char) -> LineEditResult {
        if !ch.is_ascii() || self.len >= MAX_LINE_LEN {
            return LineEditResult::Ignored;
        }

        self.buffer[self.len] = ch as u8;
        self.len += 1;
        LineEditResult::LineChanged
    }

    fn backspace(&mut self) -> LineEditResult {
        if self.len == 0 {
            return LineEditResult::Ignored;
        }

        self.len -= 1;
        self.buffer[self.len] = 0;
        LineEditResult::LineChanged
    }

    fn commit(&mut self) -> LineEditResult {
        let committed = String::from(self.current_line());
        self.buffer[..self.len].fill(0);
        self.len = 0;
        LineEditResult::LineCommitted(committed)
    }
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

pub fn line_edit_command_from_key_event(event: KeyEvent) -> Option<LineEditCommand> {
    if event.kind != KeyEventKind::Press {
        return None;
    }

    match event.code {
        KeyCode::Backspace => Some(LineEditCommand::Backspace),
        KeyCode::Enter => Some(LineEditCommand::Commit),
        _ => input::key_event_to_ascii(event).map(LineEditCommand::Insert),
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::{
        LineEditCommand, LineEditResult, LineEditor, MAX_LINE_LEN, line_edit_command_from_key_event,
    };
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
    fn test_line_edit_command_from_key_event_maps_enter_and_backspace() {
        assert_eq!(
            line_edit_command_from_key_event(key_event(KeyCode::Backspace, false)),
            Some(LineEditCommand::Backspace)
        );
        assert_eq!(
            line_edit_command_from_key_event(key_event(KeyCode::Enter, false)),
            Some(LineEditCommand::Commit)
        );
    }

    #[test_case]
    fn test_line_edit_command_from_key_event_maps_printable_ascii() {
        assert_eq!(
            line_edit_command_from_key_event(key_event(KeyCode::A, false)),
            Some(LineEditCommand::Insert('a'))
        );
        assert_eq!(
            line_edit_command_from_key_event(key_event(KeyCode::Digit1, true)),
            Some(LineEditCommand::Insert('!'))
        );
    }

    #[test_case]
    fn test_line_edit_command_from_key_event_ignores_unsupported_keys() {
        assert_eq!(
            line_edit_command_from_key_event(key_event(KeyCode::Tab, false)),
            None
        );
        assert_eq!(
            line_edit_command_from_key_event(key_event(KeyCode::ArrowLeft, false)),
            None
        );
        assert_eq!(
            line_edit_command_from_key_event(key_event_with_kind(
                KeyCode::A,
                false,
                KeyEventKind::Release,
            )),
            None
        );
    }

    #[test_case]
    fn test_line_editor_updates_current_line_with_ascii_input() {
        let mut editor = LineEditor::new();

        assert_eq!(
            editor.apply(LineEditCommand::Insert('h')),
            LineEditResult::LineChanged
        );
        assert_eq!(
            editor.apply(LineEditCommand::Insert('i')),
            LineEditResult::LineChanged
        );
        assert_eq!(editor.current_line(), "hi");
    }

    #[test_case]
    fn test_line_editor_ignores_non_ascii_insert() {
        let mut editor = LineEditor::new();

        assert_eq!(
            editor.apply(LineEditCommand::Insert('é')),
            LineEditResult::Ignored
        );
        assert_eq!(editor.current_line(), "");
    }

    #[test_case]
    fn test_line_editor_backspace_removes_last_character() {
        let mut editor = LineEditor::new();
        editor.apply(LineEditCommand::Insert('a'));
        editor.apply(LineEditCommand::Insert('b'));

        assert_eq!(
            editor.apply(LineEditCommand::Backspace),
            LineEditResult::LineChanged
        );
        assert_eq!(editor.current_line(), "a");
    }

    #[test_case]
    fn test_line_editor_backspace_on_empty_line_is_ignored() {
        let mut editor = LineEditor::new();

        assert_eq!(
            editor.apply(LineEditCommand::Backspace),
            LineEditResult::Ignored
        );
        assert_eq!(editor.current_line(), "");
    }

    #[test_case]
    fn test_line_editor_commit_returns_current_line_and_clears_buffer() {
        let mut editor = LineEditor::new();
        editor.apply(LineEditCommand::Insert('o'));
        editor.apply(LineEditCommand::Insert('s'));

        assert_eq!(
            editor.apply(LineEditCommand::Commit),
            LineEditResult::LineCommitted(String::from("os"))
        );
        assert_eq!(editor.current_line(), "");
    }

    #[test_case]
    fn test_line_editor_commit_returns_empty_string_for_empty_line() {
        let mut editor = LineEditor::new();

        assert_eq!(
            editor.apply(LineEditCommand::Commit),
            LineEditResult::LineCommitted(String::new())
        );
    }

    #[test_case]
    fn test_line_editor_ignores_input_beyond_maximum_length() {
        let mut editor = LineEditor::new();

        for _ in 0..MAX_LINE_LEN {
            assert_eq!(
                editor.apply(LineEditCommand::Insert('x')),
                LineEditResult::LineChanged
            );
        }

        assert_eq!(editor.current_line().len(), MAX_LINE_LEN);
        assert_eq!(
            editor.apply(LineEditCommand::Insert('y')),
            LineEditResult::Ignored
        );
        assert!(editor.current_line().chars().all(|ch| ch == 'x'));
    }

    #[test_case]
    fn test_line_editor_accepts_new_input_after_commit() {
        let mut editor = LineEditor::new();
        editor.apply(LineEditCommand::Insert('a'));

        assert_eq!(
            editor.apply(LineEditCommand::Commit),
            LineEditResult::LineCommitted(String::from("a"))
        );
        assert_eq!(
            editor.apply(LineEditCommand::Insert('b')),
            LineEditResult::LineChanged
        );
        assert_eq!(editor.current_line(), "b");
    }
}
