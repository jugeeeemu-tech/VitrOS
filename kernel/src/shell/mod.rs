//! Shell building blocks.

pub mod commands;
pub mod line_editor;

pub use self::commands::{CommandEffect, CommandExecutor, CommandOutcome};
pub use self::line_editor::{
    LineEditCommand, LineEditResult, LineEditor, MAX_LINE_LEN, line_edit_command_from_key_event,
};
