use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::graphics::{Region, TaskWriter};

pub const CELL_WIDTH_PX: u32 = 8;
pub const LINE_HEIGHT_PX: u32 = 10;
pub const MAX_HISTORY_LINES: usize = 512;

const STATUS_BG_COLOR: u32 = 0x00202020;
const SEPARATOR_COLOR: u32 = 0x00404040;
const TEXT_COLOR: u32 = 0xFFFFFFFF;
const PROMPT_COLOR: u32 = 0x0000FF00;
const CLEAR_COLOR: u32 = 0x00000000;
const HISTORY_TOP_Y: u32 = LINE_HEIGHT_PX * 2;
const SEPARATOR_Y: u32 = LINE_HEIGHT_PX + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalStatus<'a> {
    pub title: &'a str,
    pub fps: Option<u64>,
    pub uptime_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalFrame<'a> {
    pub status: TerminalStatus<'a>,
    pub prompt: &'a str,
    pub input_line: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalHistory {
    lines: VecDeque<String>,
    scrollback_offset: usize,
}

impl TerminalHistory {
    pub fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            scrollback_offset: 0,
        }
    }

    pub fn append_line(&mut self, line: &str, columns: usize) {
        let appended = wrapped_line_count(line, columns);
        self.lines.push_back(String::from(line));

        if self.scrollback_offset > 0 {
            self.scrollback_offset = self.scrollback_offset.saturating_add(appended);
        }

        while self.lines.len() > MAX_HISTORY_LINES {
            let removed = self
                .lines
                .pop_front()
                .expect("history length must match stored lines");
            self.scrollback_offset = self
                .scrollback_offset
                .saturating_sub(wrapped_line_count(&removed, columns));
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.scrollback_offset = 0;
    }

    pub fn set_scrollback_offset(&mut self, lines: usize) {
        self.scrollback_offset = lines;
    }

    pub fn scrollback_offset(&self) -> usize {
        self.scrollback_offset
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn visible_line_count(&self, visible_rows: usize, columns: usize) -> usize {
        self.visible_lines(visible_rows, columns).len()
    }

    pub fn visible_lines(&self, visible_rows: usize, columns: usize) -> Vec<String> {
        let wrapped = self.wrapped_lines(columns);
        let offset = self.clamped_scrollback_offset(visible_rows, wrapped.len());
        let end = wrapped.len().saturating_sub(offset);
        let start = end.saturating_sub(visible_rows);
        wrapped[start..end].to_vec()
    }

    fn wrapped_lines(&self, columns: usize) -> Vec<String> {
        let mut wrapped = Vec::new();
        for line in &self.lines {
            wrap_line_into(line, columns, &mut wrapped);
        }
        wrapped
    }

    fn clamped_scrollback_offset(&self, visible_rows: usize, total_rows: usize) -> usize {
        let hidden_rows = total_rows.saturating_sub(visible_rows);
        self.scrollback_offset.min(hidden_rows)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalLayout {
    width_px: u32,
    height_px: u32,
    columns: usize,
    rows: usize,
    history_rows: usize,
}

impl TerminalLayout {
    pub(crate) fn new(width_px: u32, height_px: u32) -> Self {
        Self::from_viewport(width_px, height_px)
    }

    pub(crate) fn from_viewport(width_px: u32, height_px: u32) -> Self {
        let columns = (width_px / CELL_WIDTH_PX) as usize;
        let rows = (height_px / LINE_HEIGHT_PX) as usize;
        let history_rows = rows.saturating_sub(3);

        Self {
            width_px,
            height_px,
            columns,
            rows,
            history_rows,
        }
    }

    pub(crate) fn resize_viewport(&mut self, width_px: u32, height_px: u32) {
        *self = Self::from_viewport(width_px, height_px);
    }

    pub(crate) fn width_px(&self) -> u32 {
        self.width_px
    }

    pub(crate) fn height_px(&self) -> u32 {
        self.height_px
    }

    pub(crate) fn columns(&self) -> usize {
        self.columns
    }

    pub(crate) fn history_rows(&self) -> usize {
        self.history_rows
    }

    pub(crate) fn can_render(&self) -> bool {
        self.width_px >= CELL_WIDTH_PX && self.rows >= 3
    }

    pub(crate) fn status_region(&self) -> Region {
        Region::new(0, 0, self.width_px, self.height_px.min(HISTORY_TOP_Y))
    }

    pub(crate) fn body_region(&self) -> Region {
        let y = HISTORY_TOP_Y.min(self.height_px);
        Region::new(0, y, self.width_px, self.height_px.saturating_sub(y))
    }

    pub(crate) fn history_line_y(&self, row: usize) -> u32 {
        HISTORY_TOP_Y + (row as u32) * LINE_HEIGHT_PX
    }

    pub(crate) fn prompt_row(&self, visible_history_lines: usize) -> usize {
        2 + visible_history_lines.min(self.history_rows)
    }

    pub(crate) fn prompt_y(&self, visible_history_lines: usize) -> u32 {
        (self.prompt_row(visible_history_lines) as u32) * LINE_HEIGHT_PX
    }

    pub(crate) fn prompt_line_region(&self, row: usize) -> Region {
        Region::new(
            0,
            (row as u32) * LINE_HEIGHT_PX,
            self.width_px,
            LINE_HEIGHT_PX,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptSnapshot {
    pub(crate) row: usize,
    pub(crate) prompt_display: String,
    pub(crate) prompt_cols: usize,
    pub(crate) input_x_px: u32,
    pub(crate) input_y_px: u32,
    pub(crate) visible_input: String,
    pub(crate) hidden_cols: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptDiff {
    AppendChar { x: u32, y: u32, ch: char },
    EraseChar { x: u32, y: u32 },
    RedrawLine,
    CoveredByBodyRedraw,
}

pub(crate) fn capture_prompt_snapshot(
    layout: &TerminalLayout,
    history: &TerminalHistory,
    prompt: &str,
    input_line: &str,
) -> PromptSnapshot {
    let prompt_display = prefix_to_fit(prompt, layout.columns);
    let prompt_cols = text_columns(prompt_display);
    let visible_history_lines = history.visible_line_count(layout.history_rows, layout.columns);
    let row = layout.prompt_row(visible_history_lines);
    let input_y_px = layout.prompt_y(visible_history_lines);
    let input_x_px = (prompt_cols as u32) * CELL_WIDTH_PX;
    let available_cols = layout.columns.saturating_sub(prompt_cols);
    let visible_input = if available_cols == 0 {
        ""
    } else {
        tail_to_fit(input_line, available_cols)
    };

    PromptSnapshot {
        row,
        prompt_display: String::from(prompt_display),
        prompt_cols,
        input_x_px,
        input_y_px,
        visible_input: String::from(visible_input),
        hidden_cols: text_columns(input_line).saturating_sub(text_columns(visible_input)),
    }
}

pub(crate) fn diff_prompt(
    previous: Option<&PromptSnapshot>,
    next: &PromptSnapshot,
    body_redraw: bool,
) -> Option<PromptDiff> {
    if body_redraw {
        return Some(PromptDiff::CoveredByBodyRedraw);
    }

    let Some(previous) = previous else {
        return Some(PromptDiff::RedrawLine);
    };

    if previous == next {
        return None;
    }

    if previous.row != next.row
        || previous.prompt_display != next.prompt_display
        || previous.prompt_cols != next.prompt_cols
        || previous.hidden_cols != next.hidden_cols
    {
        return Some(PromptDiff::RedrawLine);
    }

    if next.visible_input.len() == previous.visible_input.len() + 1
        && next
            .visible_input
            .starts_with(previous.visible_input.as_str())
    {
        let ch = next.visible_input.as_bytes()[previous.visible_input.len()] as char;
        return Some(PromptDiff::AppendChar {
            x: next.input_x_px + (previous.visible_input.len() as u32) * CELL_WIDTH_PX,
            y: next.input_y_px,
            ch,
        });
    }

    if previous.visible_input.len() == next.visible_input.len() + 1
        && previous
            .visible_input
            .starts_with(next.visible_input.as_str())
    {
        return Some(PromptDiff::EraseChar {
            x: next.input_x_px + (next.visible_input.len() as u32) * CELL_WIDTH_PX,
            y: next.input_y_px,
        });
    }

    Some(PromptDiff::RedrawLine)
}

pub(crate) fn render_status_region(
    writer: &mut TaskWriter,
    layout: &TerminalLayout,
    status: &TerminalStatus<'_>,
    status_scratch: &mut String,
) {
    let region = layout.status_region();
    writer.fill_rect(region.x, region.y, region.width, region.height, CLEAR_COLOR);
    draw_status_contents(writer, layout, status, status_scratch);
}

pub(crate) fn render_body_region(
    writer: &mut TaskWriter,
    layout: &TerminalLayout,
    history: &TerminalHistory,
    prompt: &PromptSnapshot,
) {
    let region = layout.body_region();
    writer.fill_rect(region.x, region.y, region.width, region.height, CLEAR_COLOR);
    if !layout.can_render() {
        return;
    }
    draw_history_contents(writer, layout, history);
    draw_prompt_contents(writer, prompt);
}

pub(crate) fn clear_prompt_line(writer: &mut TaskWriter, layout: &TerminalLayout, row: usize) {
    let region = layout.prompt_line_region(row);
    writer.fill_rect(region.x, region.y, region.width, region.height, CLEAR_COLOR);
}

pub(crate) fn draw_prompt_contents(writer: &mut TaskWriter, prompt: &PromptSnapshot) {
    writer.set_color(PROMPT_COLOR);
    writer.draw_string_at(0, prompt.input_y_px, prompt.prompt_display.as_str());

    if prompt.visible_input.is_empty() {
        return;
    }

    writer.set_color(TEXT_COLOR);
    writer.draw_string_at(
        prompt.input_x_px,
        prompt.input_y_px,
        prompt.visible_input.as_str(),
    );
}

fn draw_status_contents(
    writer: &mut TaskWriter,
    layout: &TerminalLayout,
    status: &TerminalStatus<'_>,
    status_scratch: &mut String,
) {
    if !layout.can_render() {
        return;
    }

    writer.fill_rect(0, 0, layout.width_px, LINE_HEIGHT_PX, STATUS_BG_COLOR);
    writer.fill_rect(0, SEPARATOR_Y, layout.width_px, 1, SEPARATOR_COLOR);

    status_scratch.clear();
    let _ = write!(
        status_scratch,
        "FPS: {}  Uptime: {}",
        format_optional_u64(status.fps),
        format_uptime(status.uptime_ms),
    );

    let metrics = tail_to_fit(status_scratch, layout.columns);
    let metrics_cols = text_columns(metrics);
    let metrics_x = (layout.columns.saturating_sub(metrics_cols) as u32) * CELL_WIDTH_PX;

    writer.set_color(TEXT_COLOR);
    writer.draw_string_at(metrics_x, 0, metrics);

    let title = prefix_to_fit(status.title, layout.columns);
    if text_columns(title) <= layout.columns.saturating_sub(metrics_cols) {
        writer.draw_string_at(0, 0, title);
    }
}

fn draw_history_contents(
    writer: &mut TaskWriter,
    layout: &TerminalLayout,
    history: &TerminalHistory,
) {
    writer.set_color(TEXT_COLOR);
    for (row, line) in history
        .visible_lines(layout.history_rows, layout.columns)
        .into_iter()
        .enumerate()
    {
        writer.draw_string_at(0, layout.history_line_y(row), &line);
    }
}

pub struct TerminalRenderer {
    layout: TerminalLayout,
    status_scratch: String,
}

impl TerminalRenderer {
    pub fn new(width_px: u32, height_px: u32) -> Self {
        Self {
            layout: TerminalLayout::new(width_px, height_px),
            status_scratch: String::with_capacity(64),
        }
    }

    pub fn columns(&self) -> usize {
        self.layout.columns()
    }

    pub fn history_rows(&self) -> usize {
        self.layout.history_rows()
    }

    pub fn render(
        &mut self,
        writer: &mut TaskWriter,
        history: &TerminalHistory,
        frame: &TerminalFrame<'_>,
    ) {
        writer.clear(CLEAR_COLOR);

        if !self.layout.can_render() {
            return;
        }

        draw_status_contents(
            writer,
            &self.layout,
            &frame.status,
            &mut self.status_scratch,
        );
        draw_history_contents(writer, &self.layout, history);
        let prompt = capture_prompt_snapshot(&self.layout, history, frame.prompt, frame.input_line);
        draw_prompt_contents(writer, &prompt);
    }
}

fn format_optional_u64(value: Option<u64>) -> OptionValue<'static> {
    match value {
        Some(value) => OptionValue::Number(value),
        None => OptionValue::Placeholder("-"),
    }
}

fn format_uptime(value: Option<u64>) -> UptimeValue {
    match value {
        Some(ms) => UptimeValue::Millis(ms),
        None => UptimeValue::Placeholder("-"),
    }
}

enum OptionValue<'a> {
    Number(u64),
    Placeholder(&'a str),
}

impl core::fmt::Display for OptionValue<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Number(value) => write!(f, "{}", value),
            Self::Placeholder(value) => f.write_str(value),
        }
    }
}

enum UptimeValue {
    Millis(u64),
    Placeholder(&'static str),
}

impl core::fmt::Display for UptimeValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Millis(ms) => write!(f, "{}.{:03}s", ms / 1000, ms % 1000),
            Self::Placeholder(value) => f.write_str(value),
        }
    }
}

fn text_columns(text: &str) -> usize {
    text.chars().count()
}

fn prefix_to_fit(text: &str, max_columns: usize) -> &str {
    if max_columns == 0 {
        return "";
    }

    match text.char_indices().nth(max_columns) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

fn tail_to_fit(text: &str, max_columns: usize) -> &str {
    if max_columns == 0 {
        return "";
    }

    let start = match text.char_indices().rev().nth(max_columns) {
        Some((idx, ch)) => idx + ch.len_utf8(),
        None => 0,
    };
    &text[start..]
}

fn wrapped_line_count(line: &str, columns: usize) -> usize {
    let columns = columns.max(1);
    if line.is_empty() {
        1
    } else {
        line.as_bytes().chunks(columns).count()
    }
}

fn wrap_line_into(line: &str, columns: usize, wrapped: &mut Vec<String>) {
    let columns = columns.max(1);
    if line.is_empty() {
        wrapped.push(String::new());
        return;
    }

    for chunk in line.as_bytes().chunks(columns) {
        wrapped.push(String::from_utf8_lossy(chunk).into_owned());
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::graphics::buffer::{DrawCommand, WriterBuffer};
    use crate::sync::BlockingMutex;

    use super::*;

    fn test_buffer(region: Region) -> Arc<BlockingMutex<WriterBuffer>> {
        Arc::new(BlockingMutex::new(WriterBuffer::new(region)))
    }

    fn flushed_commands(
        writer: &mut TaskWriter,
        buffer: &Arc<BlockingMutex<WriterBuffer>>,
    ) -> Vec<DrawCommand> {
        writer.flush();
        buffer.lock().commands().iter().cloned().collect()
    }

    #[test_case]
    fn test_terminal_history_append_line_adds_single_line() {
        let mut history = TerminalHistory::new();
        history.append_line("hello", 8);

        assert_eq!(history.len(), 1);
        assert_eq!(history.visible_lines(1, 8), vec![String::from("hello")]);
    }

    #[test_case]
    fn test_terminal_history_append_line_preserves_empty_line() {
        let mut history = TerminalHistory::new();
        history.append_line("", 8);

        assert_eq!(history.len(), 1);
        assert_eq!(history.visible_lines(1, 8), vec![String::from("")]);
    }

    #[test_case]
    fn test_terminal_history_append_line_does_not_add_extra_line_for_exact_width() {
        let mut history = TerminalHistory::new();
        history.append_line("rust", 4);

        assert_eq!(history.len(), 1);
        assert_eq!(history.visible_lines(4, 4), vec![String::from("rust")]);
    }

    #[test_case]
    fn test_terminal_history_append_line_wraps_multiple_rows() {
        let mut history = TerminalHistory::new();
        history.append_line("abcdefghijkl", 4);

        assert_eq!(history.len(), 1);
        assert_eq!(
            history.visible_lines(4, 4),
            vec![
                String::from("abcd"),
                String::from("efgh"),
                String::from("ijkl")
            ]
        );
    }

    #[test_case]
    fn test_terminal_history_discards_oldest_lines_when_capacity_exceeded() {
        let mut history = TerminalHistory::new();
        for index in 0..(MAX_HISTORY_LINES + 1) {
            let mut line = String::new();
            let _ = write!(line, "line{:03}", index);
            history.append_line(&line, 8);
        }

        assert_eq!(history.len(), MAX_HISTORY_LINES);
        let visible = history.visible_lines(MAX_HISTORY_LINES, 8);
        assert_eq!(visible.first().map(String::as_str), Some("line001"));
        assert_eq!(visible.last().map(String::as_str), Some("line512"));
    }

    #[test_case]
    fn test_terminal_history_clear_resets_lines_and_scrollback() {
        let mut history = TerminalHistory::new();
        history.append_line("hello", 8);
        history.set_scrollback_offset(3);

        history.clear();

        assert_eq!(history.len(), 0);
        assert_eq!(history.scrollback_offset(), 0);
        assert!(history.visible_lines(5, 8).is_empty());
    }

    #[test_case]
    fn test_terminal_history_visible_line_count_matches_visible_window() {
        let mut history = TerminalHistory::new();
        for line in ["0", "1", "2", "3"] {
            history.append_line(line, 8);
        }

        assert_eq!(history.visible_line_count(2, 8), 2);

        history.set_scrollback_offset(99);
        assert_eq!(history.visible_line_count(2, 8), 2);
    }

    #[test_case]
    fn test_terminal_history_visible_lines_clamp_scrollback_offset() {
        let mut history = TerminalHistory::new();
        for line in ["0", "1", "2", "3", "4"] {
            history.append_line(line, 8);
        }
        history.set_scrollback_offset(99);

        assert_eq!(
            history.visible_lines(2, 8),
            vec![String::from("0"), String::from("1")]
        );
    }

    #[test_case]
    fn test_terminal_history_appending_during_scrollback_keeps_visible_window() {
        let mut history = TerminalHistory::new();
        for line in ["aa", "bb", "cc", "dd"] {
            history.append_line(line, 2);
        }
        history.set_scrollback_offset(1);
        let before = history.visible_lines(2, 2);

        history.append_line("eeff", 2);
        let after = history.visible_lines(2, 2);

        assert_eq!(before, vec![String::from("bb"), String::from("cc")]);
        assert_eq!(after, before);
        assert_eq!(history.scrollback_offset(), 3);
    }

    #[test_case]
    fn test_terminal_history_rewraps_logical_lines_for_new_width() {
        let mut history = TerminalHistory::new();
        history.append_line("abcdefgh", 8);

        assert_eq!(history.visible_lines(4, 8), vec![String::from("abcdefgh")]);
        assert_eq!(
            history.visible_lines(4, 4),
            vec![String::from("abcd"), String::from("efgh")]
        );
    }

    #[test_case]
    fn test_terminal_renderer_reports_columns_and_history_rows() {
        let renderer = TerminalRenderer::new(1024, 768);

        assert_eq!(renderer.columns(), 128);
        assert_eq!(renderer.history_rows(), 73);
    }

    #[test_case]
    fn test_terminal_renderer_small_viewport_only_clears() {
        let buffer = test_buffer(Region::new(0, 0, 4, 20));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), TEXT_COLOR);
        let mut renderer = TerminalRenderer::new(4, 20);
        let history = TerminalHistory::new();
        let frame = TerminalFrame {
            status: TerminalStatus {
                title: "vitrOS",
                fps: Some(60),
                uptime_ms: Some(1234),
            },
            prompt: "> ",
            input_line: "help",
        };

        let commands = flushed_commands(&mut writer, &buffer);
        assert!(commands.is_empty());

        renderer.render(&mut writer, &history, &frame);
        let commands = flushed_commands(&mut writer, &buffer);

        assert_eq!(commands.len(), 1);
        assert!(matches!(
            commands[0],
            DrawCommand::Clear { color: CLEAR_COLOR }
        ));
    }

    #[test_case]
    fn test_terminal_renderer_renders_status_history_and_prompt() {
        let width_px = 160;
        let height_px = 50;
        let buffer = test_buffer(Region::new(0, 0, width_px, height_px));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), TEXT_COLOR);
        let mut renderer = TerminalRenderer::new(width_px, height_px);
        let mut history = TerminalHistory::new();
        history.append_line("alpha", renderer.columns());
        history.append_line("beta", renderer.columns());
        let frame = TerminalFrame {
            status: TerminalStatus {
                title: "vitrOS",
                fps: Some(60),
                uptime_ms: Some(12_345),
            },
            prompt: "> ",
            input_line: "echo hello",
        };

        renderer.render(&mut writer, &history, &frame);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(matches!(
            commands[0],
            DrawCommand::Clear { color: CLEAR_COLOR }
        ));
        assert!(matches!(
            commands[1],
            DrawCommand::FillRect {
                x: 0,
                y: 0,
                width: 160,
                height: 10,
                color: STATUS_BG_COLOR,
            }
        ));
        assert!(matches!(
            commands[2],
            DrawCommand::FillRect {
                x: 0,
                y: SEPARATOR_Y,
                width: 160,
                height: 1,
                color: SEPARATOR_COLOR,
            }
        ));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 20,
                    text,
                    color: TEXT_COLOR,
                } if text == "alpha"
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 30,
                    text,
                    color: TEXT_COLOR,
                } if text == "beta"
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 40,
                    text,
                    color: PROMPT_COLOR,
                } if text == "> "
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 16,
                    y: 40,
                    text,
                    color: TEXT_COLOR,
                } if text == "echo hello"
            )
        }));
    }

    #[test_case]
    fn test_terminal_renderer_places_prompt_immediately_after_last_visible_history_line() {
        let width_px = 160;
        let height_px = 50;
        let buffer = test_buffer(Region::new(0, 0, width_px, height_px));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), TEXT_COLOR);
        let mut renderer = TerminalRenderer::new(width_px, height_px);
        let mut history = TerminalHistory::new();
        history.append_line("alpha", renderer.columns());
        let frame = TerminalFrame {
            status: TerminalStatus {
                title: "vitrOS",
                fps: Some(60),
                uptime_ms: Some(12_345),
            },
            prompt: "> ",
            input_line: "echo hello",
        };

        renderer.render(&mut writer, &history, &frame);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 30,
                    text,
                    color: PROMPT_COLOR,
                } if text == "> "
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 16,
                    y: 30,
                    text,
                    color: TEXT_COLOR,
                } if text == "echo hello"
            )
        }));
    }

    #[test_case]
    fn test_terminal_renderer_drops_title_when_metrics_would_overlap() {
        let width_px = 80;
        let height_px = 50;
        let buffer = test_buffer(Region::new(0, 0, width_px, height_px));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), TEXT_COLOR);
        let mut renderer = TerminalRenderer::new(width_px, height_px);
        let history = TerminalHistory::new();
        let frame = TerminalFrame {
            status: TerminalStatus {
                title: "title",
                fps: Some(60),
                uptime_ms: Some(12_345),
            },
            prompt: "> ",
            input_line: "",
        };

        renderer.render(&mut writer, &history, &frame);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(!commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 0,
                    text,
                    color: TEXT_COLOR,
                } if text == "title"
            )
        }));
    }

    #[test_case]
    fn test_terminal_renderer_shows_tail_of_long_input_line() {
        let width_px = 80;
        let height_px = 50;
        let buffer = test_buffer(Region::new(0, 0, width_px, height_px));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), TEXT_COLOR);
        let mut renderer = TerminalRenderer::new(width_px, height_px);
        let history = TerminalHistory::new();
        let frame = TerminalFrame {
            status: TerminalStatus {
                title: "v",
                fps: None,
                uptime_ms: None,
            },
            prompt: "> ",
            input_line: "abcdefghi",
        };

        renderer.render(&mut writer, &history, &frame);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 16,
                    y: 20,
                    text,
                    color: TEXT_COLOR,
                } if text == "cdefghi"
            )
        }));
    }

    #[test_case]
    fn test_prompt_diff_appends_single_char_when_window_is_stable() {
        let layout = TerminalLayout::new(160, 50);
        let history = TerminalHistory::new();
        let previous = capture_prompt_snapshot(&layout, &history, "> ", "ab");
        let next = capture_prompt_snapshot(&layout, &history, "> ", "abc");

        assert_eq!(
            diff_prompt(Some(&previous), &next, false),
            Some(PromptDiff::AppendChar {
                x: 32,
                y: 20,
                ch: 'c',
            })
        );
    }

    #[test_case]
    fn test_prompt_diff_falls_back_to_redraw_when_tail_window_shifts() {
        let layout = TerminalLayout::new(80, 50);
        let history = TerminalHistory::new();
        let previous = capture_prompt_snapshot(&layout, &history, "> ", "abcdefg");
        let next = capture_prompt_snapshot(&layout, &history, "> ", "abcdefgh");

        assert_eq!(
            diff_prompt(Some(&previous), &next, false),
            Some(PromptDiff::RedrawLine)
        );
    }
}
