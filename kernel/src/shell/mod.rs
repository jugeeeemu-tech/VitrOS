//! Shell building blocks.

pub mod commands;
pub mod line_editor;
pub mod runtime;
pub mod terminal;

pub use self::commands::{CommandEffect, CommandExecutor, CommandOutcome};
pub use self::line_editor::{
    LineEditCommand, LineEditResult, LineEditor, MAX_LINE_LEN, line_edit_command_from_key_event,
};
pub use self::runtime::shell_task;
pub use self::terminal::{
    CELL_WIDTH_PX, LINE_HEIGHT_PX, MAX_HISTORY_LINES, TerminalFrame, TerminalHistory,
    TerminalRenderer, TerminalStatus,
};
