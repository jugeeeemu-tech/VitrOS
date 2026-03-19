use alloc::format;
use alloc::string::String;

use crate::graphics::{self, TaskWriter};
use crate::hpet;
use crate::input::{self, PolledKeyEvent};
use crate::shell::commands::CommandEffect;
#[cfg(feature = "visualize-input")]
use crate::shell::commands::{VisualizationAction, VisualizationCommand, VisualizationTarget};
use crate::shell::line_editor::{LineEditResult, LineEditor, line_edit_command_from_text_input};
use crate::shell::terminal::{
    CELL_WIDTH_PX, LINE_HEIGHT_PX, PromptDiff, PromptSnapshot, TerminalHistory, TerminalLayout,
    TerminalStatus, capture_prompt_snapshot, clear_prompt_block, clear_prompt_cursor, diff_prompt,
    draw_prompt_contents, draw_prompt_cursor, render_body_region, render_status_region,
};
use crate::shell::{CommandExecutor, CommandOutcome};
use crate::text_input::TextInputEngine;

const CURSOR_BLINK_INTERVAL_MS: u64 = 500;
const FRAME_INTERVAL_MS: u64 = 16;
const STATUS_UPDATE_INTERVAL_MS: u64 = 1000;
const SHELL_TITLE: &str = "vitrOS kernel shell";
const PROMPT: &str = "> ";

const INITIAL_MESSAGES: [&str; 2] = [SHELL_TITLE, "run 'help' to list built-in commands"];

pub extern "C" fn shell_task() -> ! {
    crate::info!("[Shell] Started");

    let (screen_width, screen_height) = graphics::compositor::screen_size();
    let region = graphics::Region::new(0, 0, screen_width, screen_height);
    let buffer =
        graphics::compositor::register_writer(region).expect("Failed to register shell writer");
    let mut writer = TaskWriter::new(buffer, 0xFFFF_FFFF);

    let mut environment = KernelRuntimeEnvironment;
    let mut runtime = ShellRuntime::new(
        screen_width,
        screen_height,
        environment.frame_count(),
        environment.uptime_ms(),
    );

    loop {
        runtime.tick(&mut environment, &mut writer);
        crate::sched::sleep_ms(FRAME_INTERVAL_MS);
    }
}

trait RuntimeEnvironment {
    fn poll_key_event(&mut self) -> Option<PolledKeyEvent>;
    fn frame_count(&self) -> u64;
    fn uptime_ms(&self) -> Option<u64>;
}

struct KernelRuntimeEnvironment;

impl RuntimeEnvironment for KernelRuntimeEnvironment {
    fn poll_key_event(&mut self) -> Option<PolledKeyEvent> {
        input::poll_key_event_internal()
    }

    fn frame_count(&self) -> u64 {
        graphics::compositor::frame_count()
    }

    fn uptime_ms(&self) -> Option<u64> {
        hpet::is_available().then(hpet::elapsed_ms)
    }
}

struct ShellRuntime {
    screen_width_px: u32,
    screen_height_px: u32,
    layout: TerminalLayout,
    history: TerminalHistory,
    line_editor: LineEditor,
    text_input: TextInputEngine,
    command_executor: CommandExecutor,
    status_sampler: StatusSampler,
    cursor_blink: CursorBlinkState,
    render_state: RenderState,
    status_scratch: String,
}

impl ShellRuntime {
    fn new(
        width_px: u32,
        height_px: u32,
        initial_frame_count: u64,
        initial_uptime_ms: Option<u64>,
    ) -> Self {
        let layout = TerminalLayout::new(width_px, height_px);
        let mut history = TerminalHistory::new();
        for line in INITIAL_MESSAGES {
            history.append_line(line, layout.columns());
        }

        Self {
            screen_width_px: width_px,
            screen_height_px: height_px,
            layout,
            history,
            line_editor: LineEditor::new(),
            text_input: TextInputEngine::default(),
            command_executor: CommandExecutor::new(),
            status_sampler: StatusSampler::new(initial_frame_count, initial_uptime_ms),
            cursor_blink: CursorBlinkState::new(current_time_ms(
                initial_frame_count,
                initial_uptime_ms,
            )),
            render_state: RenderState::new(),
            status_scratch: String::with_capacity(64),
        }
    }

    fn tick<E: RuntimeEnvironment>(&mut self, environment: &mut E, writer: &mut TaskWriter) {
        self.sync_layout();
        self.drain_input(environment);
        self.sync_layout();
        let frame_count = environment.frame_count();
        let uptime_ms = environment.uptime_ms();
        let current_time_ms = current_time_ms(frame_count, uptime_ms);
        self.apply_pending_cursor_reset(current_time_ms);
        let cursor_visible = self.cursor_blink.is_visible(current_time_ms);
        let (status, status_changed) = self.terminal_status(frame_count, uptime_ms);
        if status_changed {
            self.render_state.status_dirty = true;
        }
        if self.render_state.last_cursor_visible != Some(cursor_visible) {
            self.render_state.cursor_dirty = true;
        }

        if !self.render_state.any_dirty() {
            return;
        }

        self.render(writer, &status, cursor_visible);
    }

    fn drain_input<E: RuntimeEnvironment>(&mut self, environment: &mut E) {
        while let Some(event) = environment.poll_key_event() {
            self.process_key_event(event);
        }
    }

    fn process_key_event(&mut self, input_event: PolledKeyEvent) {
        let event = input_event.event;
        let text_input_event = self.text_input.translate_key_event(event);

        let Some(command) = line_edit_command_from_text_input(text_input_event) else {
            return;
        };

        match self.line_editor.apply(command) {
            LineEditResult::Ignored => {}
            LineEditResult::LineChanged => {
                if !self.render_state.body_dirty {
                    self.render_state.prompt_dirty = true;
                }
                self.render_state.cursor_reset_pending = true;
            }
            LineEditResult::LineCommitted(line) => {
                self.handle_committed_line(line);
                self.render_state.cursor_reset_pending = true;
            }
        }
    }

    fn handle_committed_line(&mut self, line: String) {
        if line.is_empty() {
            return;
        }

        self.history
            .append_line(&format!("{}{}", PROMPT, line), self.layout.columns());
        let outcome = self.command_executor.execute_line(&line);
        self.apply_command_outcome(outcome);
    }

    fn apply_command_outcome(&mut self, outcome: CommandOutcome) {
        for effect in outcome.effects {
            match effect {
                CommandEffect::AppendLines(lines) => {
                    for line in lines {
                        self.history.append_line(&line, self.layout.columns());
                    }
                    self.render_state.body_dirty = true;
                    self.render_state.prompt_dirty = false;
                }
                CommandEffect::ClearOutput => {
                    self.history.clear();
                    self.render_state.body_dirty = true;
                    self.render_state.prompt_dirty = false;
                }
                #[cfg(feature = "visualize-input")]
                CommandEffect::Visualization(command) => {
                    self.apply_visualization_command(command);
                }
            }
        }
    }

    fn sync_layout(&mut self) {
        let target_width = self.target_viewport_width();
        if self.layout.width_px() == target_width
            && self.layout.height_px() == self.screen_height_px
        {
            return;
        }

        self.layout
            .resize_viewport(target_width, self.screen_height_px);
        self.render_state.last_status = None;
        self.render_state.last_prompt = None;
        self.render_state.last_cursor_visible = None;
        self.render_state.status_dirty = true;
        self.render_state.body_dirty = true;
        self.render_state.prompt_dirty = false;
        self.render_state.cursor_dirty = false;
    }

    fn target_viewport_width(&self) -> u32 {
        #[cfg(feature = "visualize-input")]
        {
            if crate::input_trace::is_enabled() {
                return crate::input_trace::shell_viewport_width(self.screen_width_px);
            }
        }

        self.screen_width_px
    }

    fn terminal_status(
        &mut self,
        frame_count: u64,
        uptime_ms: Option<u64>,
    ) -> (TerminalStatus<'static>, bool) {
        let (fps, uptime_ms, changed) = self.status_sampler.update(frame_count, uptime_ms);
        (
            TerminalStatus {
                title: SHELL_TITLE,
                fps,
                uptime_ms,
            },
            changed,
        )
    }

    fn render(
        &mut self,
        writer: &mut TaskWriter,
        status: &TerminalStatus<'static>,
        cursor_visible: bool,
    ) {
        if !self.layout.can_render() {
            writer.fill_rect(0, 0, self.layout.width_px(), self.layout.height_px(), 0);
            writer.flush();
            self.render_state.last_status = Some(RenderedStatus::from(*status));
            self.render_state.last_prompt = None;
            self.render_state.last_cursor_visible = Some(false);
            self.render_state.status_dirty = false;
            self.render_state.body_dirty = false;
            self.render_state.prompt_dirty = false;
            self.render_state.cursor_dirty = false;
            return;
        }

        if self.render_state.status_dirty {
            render_status_region(writer, &self.layout, status, &mut self.status_scratch);
            self.render_state.last_status = Some(RenderedStatus::from(*status));
        }

        let next_prompt = capture_prompt_snapshot(
            &self.layout,
            &self.history,
            PROMPT,
            self.line_editor.current_line(),
        );
        let previous_cursor_visible = self.render_state.last_cursor_visible.unwrap_or(false);

        if self.render_state.body_dirty {
            render_body_region(writer, &self.layout, &self.history, &next_prompt);
            self.render_state.body_dirty = false;
            self.render_state.prompt_dirty = false;
        } else if self.render_state.prompt_dirty {
            if let Some(diff) =
                diff_prompt(self.render_state.last_prompt.as_ref(), &next_prompt, false)
            {
                match diff {
                    PromptDiff::RedrawBody => {
                        render_body_region(writer, &self.layout, &self.history, &next_prompt);
                    }
                    prompt_diff => {
                        if previous_cursor_visible
                            && !matches!(
                                prompt_diff,
                                PromptDiff::RedrawPrompt | PromptDiff::CoveredByBodyRedraw
                            )
                        {
                            if let Some(previous_prompt) = self.render_state.last_prompt.as_ref() {
                                clear_prompt_cursor(writer, previous_prompt);
                            }
                        }
                        self.apply_prompt_diff(writer, &next_prompt, prompt_diff);
                    }
                }
            }
            self.render_state.prompt_dirty = false;
        } else if self.render_state.cursor_dirty {
            match (previous_cursor_visible, cursor_visible) {
                (true, false) => {
                    if let Some(previous_prompt) = self.render_state.last_prompt.as_ref() {
                        clear_prompt_cursor(writer, previous_prompt);
                    }
                }
                _ => {}
            }
        }

        if self.render_state.body_dirty || self.render_state.prompt_dirty {
            unreachable!("dirty flags must be cleared before cursor overlay");
        }

        if self.render_state.last_prompt.as_ref() != Some(&next_prompt)
            || self.render_state.cursor_dirty
        {
            if cursor_visible {
                draw_prompt_cursor(writer, &next_prompt);
            }
        }

        writer.flush();
        self.render_state.last_prompt = Some(next_prompt);
        self.render_state.last_cursor_visible = Some(cursor_visible);
        self.render_state.status_dirty = false;
        self.render_state.cursor_dirty = false;
    }

    fn apply_prompt_diff(
        &self,
        writer: &mut TaskWriter,
        next_prompt: &PromptSnapshot,
        diff: PromptDiff,
    ) {
        match diff {
            PromptDiff::AppendChar { x, y, ch } => {
                writer.set_color(0xFFFF_FFFF);
                writer.draw_char_at(x, y, ch);
            }
            PromptDiff::EraseChar { x, y } => {
                writer.fill_rect(x, y, CELL_WIDTH_PX, LINE_HEIGHT_PX, 0);
            }
            PromptDiff::RedrawPrompt => {
                if let Some(previous) = self.render_state.last_prompt.as_ref() {
                    clear_prompt_block(writer, &self.layout, previous);
                    if previous.top_row != next_prompt.top_row
                        || previous.row_count() != next_prompt.row_count()
                    {
                        clear_prompt_block(writer, &self.layout, next_prompt);
                    }
                } else {
                    clear_prompt_block(writer, &self.layout, next_prompt);
                }
                draw_prompt_contents(writer, next_prompt);
            }
            PromptDiff::RedrawBody => {
                unreachable!("body redraw diffs must be handled before prompt-only diff rendering");
            }
            PromptDiff::CoveredByBodyRedraw => {}
        }
    }

    #[cfg(feature = "visualize-input")]
    fn apply_visualization_command(&mut self, command: VisualizationCommand) {
        match (command.target, command.action) {
            (VisualizationTarget::Input, VisualizationAction::On) => {
                crate::input_trace::set_enabled(true);
            }
            (VisualizationTarget::Input, VisualizationAction::Off) => {
                crate::input_trace::set_enabled(false);
            }
            (VisualizationTarget::Input, VisualizationAction::Clear) => {
                crate::input_trace::clear();
            }
        }
        self.render_state.body_dirty = true;
        self.render_state.prompt_dirty = false;
        self.render_state.cursor_reset_pending = true;
    }

    fn apply_pending_cursor_reset(&mut self, current_time_ms: u64) {
        if self.render_state.cursor_reset_pending {
            self.cursor_blink.reset(current_time_ms);
            self.render_state.cursor_reset_pending = false;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderedStatus {
    fps: Option<u64>,
    uptime_ms: Option<u64>,
}

impl From<TerminalStatus<'_>> for RenderedStatus {
    fn from(value: TerminalStatus<'_>) -> Self {
        Self {
            fps: value.fps,
            uptime_ms: value.uptime_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderState {
    last_status: Option<RenderedStatus>,
    last_prompt: Option<PromptSnapshot>,
    last_cursor_visible: Option<bool>,
    status_dirty: bool,
    body_dirty: bool,
    prompt_dirty: bool,
    cursor_dirty: bool,
    cursor_reset_pending: bool,
}

impl RenderState {
    fn new() -> Self {
        Self {
            last_status: None,
            last_prompt: None,
            last_cursor_visible: None,
            status_dirty: true,
            body_dirty: true,
            prompt_dirty: false,
            cursor_dirty: false,
            cursor_reset_pending: false,
        }
    }

    fn any_dirty(&self) -> bool {
        self.status_dirty || self.body_dirty || self.prompt_dirty || self.cursor_dirty
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorBlinkState {
    last_reset_ms: u64,
}

impl CursorBlinkState {
    fn new(initial_time_ms: u64) -> Self {
        Self {
            last_reset_ms: initial_time_ms,
        }
    }

    fn reset(&mut self, current_time_ms: u64) {
        self.last_reset_ms = current_time_ms;
    }

    fn is_visible(&self, current_time_ms: u64) -> bool {
        ((current_time_ms.saturating_sub(self.last_reset_ms) / CURSOR_BLINK_INTERVAL_MS) % 2) == 0
    }
}

fn current_time_ms(frame_count: u64, uptime_ms: Option<u64>) -> u64 {
    uptime_ms.unwrap_or(frame_count.saturating_mul(FRAME_INTERVAL_MS))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusSampler {
    last_sample_ms: Option<u64>,
    last_frame_count: u64,
    fps: Option<u64>,
    uptime_ms: Option<u64>,
}

impl StatusSampler {
    fn new(initial_frame_count: u64, initial_uptime_ms: Option<u64>) -> Self {
        Self {
            last_sample_ms: initial_uptime_ms,
            last_frame_count: initial_frame_count,
            fps: None,
            uptime_ms: initial_uptime_ms,
        }
    }

    fn update(
        &mut self,
        current_frame_count: u64,
        current_uptime_ms: Option<u64>,
    ) -> (Option<u64>, Option<u64>, bool) {
        let Some(current_uptime_ms) = current_uptime_ms else {
            let changed = self.fps.is_some() || self.uptime_ms.is_some();
            self.last_sample_ms = None;
            self.last_frame_count = current_frame_count;
            self.fps = None;
            self.uptime_ms = None;
            return (self.fps, self.uptime_ms, changed);
        };

        let Some(last_sample_ms) = self.last_sample_ms else {
            let changed = self.uptime_ms != Some(current_uptime_ms);
            self.last_sample_ms = Some(current_uptime_ms);
            self.last_frame_count = current_frame_count;
            self.uptime_ms = Some(current_uptime_ms);
            return (self.fps, self.uptime_ms, changed);
        };

        let elapsed_ms = current_uptime_ms.saturating_sub(last_sample_ms);
        if elapsed_ms < STATUS_UPDATE_INTERVAL_MS {
            return (self.fps, self.uptime_ms, false);
        }

        let frame_delta = current_frame_count.saturating_sub(self.last_frame_count);
        let next_fps = Some(frame_delta.saturating_mul(1000) / elapsed_ms.max(1));
        let next_uptime_ms = Some(current_uptime_ms);
        let changed = self.fps != next_fps || self.uptime_ms != next_uptime_ms;
        self.fps = next_fps;
        self.uptime_ms = next_uptime_ms;
        self.last_sample_ms = Some(current_uptime_ms);
        self.last_frame_count = current_frame_count;
        (self.fps, self.uptime_ms, changed)
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::VecDeque;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{INITIAL_MESSAGES, PROMPT, RuntimeEnvironment, SHELL_TITLE, ShellRuntime};
    use crate::graphics::Region;
    use crate::graphics::TaskWriter;
    use crate::graphics::buffer::{DrawCommand, WriterBuffer};
    use crate::input::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, PolledKeyEvent};
    #[cfg(feature = "visualize-input")]
    use crate::shell::commands::{VisualizationAction, VisualizationCommand, VisualizationTarget};
    use crate::shell::{CELL_WIDTH_PX, LINE_HEIGHT_PX, MAX_HISTORY_LINES};
    use crate::sync::BlockingMutex;

    #[derive(Default)]
    struct FakeEnvironment {
        events: VecDeque<PolledKeyEvent>,
        frame_count: u64,
        uptime_ms: Option<u64>,
    }

    impl RuntimeEnvironment for FakeEnvironment {
        fn poll_key_event(&mut self) -> Option<PolledKeyEvent> {
            self.events.pop_front()
        }

        fn frame_count(&self) -> u64 {
            self.frame_count
        }

        fn uptime_ms(&self) -> Option<u64> {
            self.uptime_ms
        }
    }

    fn key_event(code: KeyCode) -> PolledKeyEvent {
        let event = raw_key_event(code);
        PolledKeyEvent { event }
    }

    fn raw_key_event(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers: KeyModifiers::default(),
        }
    }

    fn key_event_for_char(ch: char) -> PolledKeyEvent {
        let code = match ch {
            'a' => KeyCode::A,
            'b' => KeyCode::B,
            'c' => KeyCode::C,
            'd' => KeyCode::D,
            'e' => KeyCode::E,
            'f' => KeyCode::F,
            'g' => KeyCode::G,
            'h' => KeyCode::H,
            'i' => KeyCode::I,
            'l' => KeyCode::L,
            'o' => KeyCode::O,
            'r' => KeyCode::R,
            ' ' => KeyCode::Space,
            _ => panic!("unsupported test character: {}", ch),
        };
        key_event(code)
    }

    fn runtime(initial_frame_count: u64, initial_uptime_ms: Option<u64>) -> ShellRuntime {
        runtime_with_size(640, 480, initial_frame_count, initial_uptime_ms)
    }

    fn runtime_with_size(
        width_px: u32,
        height_px: u32,
        initial_frame_count: u64,
        initial_uptime_ms: Option<u64>,
    ) -> ShellRuntime {
        ShellRuntime::new(width_px, height_px, initial_frame_count, initial_uptime_ms)
    }

    fn history_lines(runtime: &ShellRuntime) -> Vec<String> {
        runtime
            .history
            .visible_lines(MAX_HISTORY_LINES, runtime.layout.columns())
    }

    fn test_buffer(region: Region) -> Arc<BlockingMutex<WriterBuffer>> {
        Arc::new(BlockingMutex::new(WriterBuffer::new(region)))
    }

    fn flushed_commands(
        writer: &mut TaskWriter,
        buffer: &Arc<BlockingMutex<WriterBuffer>>,
    ) -> Vec<DrawCommand> {
        writer.flush();
        let mut guard = buffer.lock();
        let commands = guard.commands().iter().cloned().collect();
        guard.clear_commands();
        commands
    }

    #[test_case]
    fn test_runtime_initializes_history_with_boot_messages() {
        let runtime = runtime(0, Some(0));

        assert_eq!(
            history_lines(&runtime),
            INITIAL_MESSAGES
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test_case]
    fn test_drain_input_commits_line_from_multiple_key_events() {
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            events: "echo hello"
                .chars()
                .map(key_event_for_char)
                .chain(core::iter::once(key_event(KeyCode::Enter)))
                .collect(),
            ..FakeEnvironment::default()
        };

        runtime.drain_input(&mut environment);

        assert_eq!(
            history_lines(&runtime),
            vec![
                String::from(SHELL_TITLE),
                String::from("run 'help' to list built-in commands"),
                String::from("> echo hello"),
                String::from("hello"),
            ]
        );
        assert_eq!(runtime.line_editor.current_line(), "");
    }

    #[test_case]
    fn test_empty_enter_does_not_change_history() {
        let mut runtime = runtime(0, Some(0));
        let before = history_lines(&runtime);
        let mut environment = FakeEnvironment {
            events: core::iter::once(key_event(KeyCode::Enter)).collect(),
            ..FakeEnvironment::default()
        };

        runtime.drain_input(&mut environment);

        assert_eq!(history_lines(&runtime), before);
    }

    #[test_case]
    fn test_clear_command_clears_history_including_prompt_echo() {
        let mut runtime = runtime(0, Some(0));
        let mut environment = FakeEnvironment {
            events: "clear"
                .chars()
                .map(key_event_for_char)
                .chain(core::iter::once(key_event(KeyCode::Enter)))
                .collect(),
            ..FakeEnvironment::default()
        };

        runtime.drain_input(&mut environment);

        assert!(history_lines(&runtime).is_empty());
    }

    #[test_case]
    fn test_status_sampler_updates_only_after_interval() {
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        let (initial, initial_changed) =
            runtime.terminal_status(environment.frame_count, environment.uptime_ms);
        assert_eq!(initial.title, SHELL_TITLE);
        assert_eq!(initial.fps, None);
        assert_eq!(initial.uptime_ms, Some(1_000));
        assert!(!initial_changed);

        environment.frame_count = 60;
        environment.uptime_ms = Some(1_999);
        let (before_interval, before_changed) =
            runtime.terminal_status(environment.frame_count, environment.uptime_ms);
        assert_eq!(before_interval.fps, None);
        assert_eq!(before_interval.uptime_ms, Some(1_000));
        assert!(!before_changed);

        environment.frame_count = 130;
        environment.uptime_ms = Some(2_000);
        let (after_interval, after_changed) =
            runtime.terminal_status(environment.frame_count, environment.uptime_ms);
        assert_eq!(after_interval.fps, Some(120));
        assert_eq!(after_interval.uptime_ms, Some(2_000));
        assert!(after_changed);
    }

    #[test_case]
    fn test_tick_skips_render_when_shell_state_is_unchanged() {
        let buffer = test_buffer(Region::new(0, 0, 640, 480));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let first_commands = flushed_commands(&mut writer, &buffer);
        assert!(!first_commands.is_empty());

        runtime.tick(&mut environment, &mut writer);
        let second_commands = flushed_commands(&mut writer, &buffer);
        assert!(second_commands.is_empty());
    }

    #[test_case]
    fn test_tick_appends_single_character_without_full_redraw() {
        let buffer = test_buffer(Region::new(0, 0, 640, 480));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.events.push_back(key_event(KeyCode::H));
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 16,
                    y: 40,
                    width: 2,
                    height,
                    color: 0,
                } if *height == LINE_HEIGHT_PX
            )
        }));
        assert!(matches!(
            commands
                .iter()
                .find(|command| matches!(command, DrawCommand::DrawChar { .. }))
                .expect("append should draw a character"),
            DrawCommand::DrawChar {
                x: 16,
                y: 40,
                ch: b'h',
                ..
            }
        ));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 24,
                    y: 40,
                    width: 2,
                    height,
                    color: 0xFFFF_FFFF,
                } if *height == LINE_HEIGHT_PX
            )
        }));
    }

    #[test_case]
    fn test_tick_backspace_erases_single_prompt_cell() {
        let buffer = test_buffer(Region::new(0, 0, 640, 480));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.events.push_back(key_event(KeyCode::H));
        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.events.push_back(key_event(KeyCode::Backspace));
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 24,
                    y: 40,
                    width: 2,
                    height,
                    color: 0,
                } if *height == LINE_HEIGHT_PX
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 16,
                    y: 40,
                    width,
                    height,
                    color: 0,
                } if *width == CELL_WIDTH_PX && *height == LINE_HEIGHT_PX
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 16,
                    y: 40,
                    width: 2,
                    height,
                    color: 0xFFFF_FFFF,
                } if *height == LINE_HEIGHT_PX
            )
        }));
    }

    #[test_case]
    fn test_tick_redraws_prompt_block_when_wrap_does_not_scroll_history() {
        let buffer = test_buffer(Region::new(0, 0, 80, 50));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime_with_size(80, 50, 10, Some(1_000));
        runtime.history.clear();
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        for ch in "abcdefghi".chars() {
            environment.events.push_back(key_event_for_char(ch));
        }
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 0,
                    y: 20,
                    width: 80,
                    height,
                    color: 0,
                } if *height == LINE_HEIGHT_PX * 2
            )
        }));
        assert!(!commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 0,
                    y: 20,
                    width: 80,
                    height,
                    color: 0,
                } if *height == LINE_HEIGHT_PX * 3
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 20,
                    text,
                    ..
                } if text == "> "
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 16,
                    y: 20,
                    text,
                    ..
                } if text == "abcdefgh"
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 16,
                    y: 30,
                    text,
                    ..
                } if text == "i"
            )
        }));
    }

    #[test_case]
    fn test_tick_redraws_body_when_wrap_scrolls_history() {
        let buffer = test_buffer(Region::new(0, 0, 80, 50));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime_with_size(80, 50, 10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        for ch in "abcdefghi".chars() {
            environment.events.push_back(key_event_for_char(ch));
        }
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 0,
                    y: 20,
                    width: 80,
                    height,
                    color: 0,
                } if *height == LINE_HEIGHT_PX * 3
            )
        }));
        assert!(!commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 20,
                    text,
                    ..
                } if text == "uilt-in co"
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 20,
                    text,
                    ..
                } if text == "mmands"
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 0,
                    y: 30,
                    text,
                    ..
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
                    ..
                } if text == "abcdefgh"
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString {
                    x: 16,
                    y: 40,
                    text,
                    ..
                } if text == "i"
            )
        }));
    }

    #[test_case]
    fn test_tick_commit_redraws_body_without_full_clear() {
        let buffer = test_buffer(Region::new(0, 0, 640, 480));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        for ch in "echo hello".chars() {
            environment.events.push_back(key_event_for_char(ch));
        }
        environment.events.push_back(key_event(KeyCode::Enter));
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Clear { .. }))
        );
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 0,
                    y: 20,
                    width: 640,
                    height: 460,
                    color: 0,
                }
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString { text, .. } if text == "> echo hello"
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawString { text, .. } if text == "hello"
            )
        }));
    }

    #[test_case]
    fn test_tick_status_update_redraws_only_status_region() {
        let buffer = test_buffer(Region::new(0, 0, 640, 480));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.frame_count = 130;
        environment.uptime_ms = Some(2_000);
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 0,
                    y: 0,
                    width: 640,
                    height: 20,
                    color: 0,
                }
            )
        }));
        assert!(!commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 0,
                    y: 20,
                    width: 640,
                    height: 460,
                    ..
                }
            )
        }));
    }

    #[test_case]
    fn test_prompt_constant_matches_expected_prefix() {
        assert_eq!(PROMPT, "> ");
    }

    #[test_case]
    fn test_tick_cursor_blinks_without_redrawing_prompt_text() {
        let buffer = test_buffer(Region::new(0, 0, 640, 480));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.uptime_ms = Some(1_600);
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert_eq!(commands.len(), 1);
        assert!(matches!(
            commands[0],
            DrawCommand::FillRect {
                x: 16,
                y: 40,
                width: 2,
                height,
                color: 0,
            } if height == LINE_HEIGHT_PX
        ));
    }

    #[test_case]
    fn test_tick_key_input_resets_hidden_cursor_to_visible() {
        let buffer = test_buffer(Region::new(0, 0, 640, 480));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.uptime_ms = Some(1_600);
        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.events.push_back(key_event(KeyCode::H));
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::DrawChar {
                    x: 16,
                    y: 40,
                    ch: b'h',
                    ..
                }
            )
        }));
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                DrawCommand::FillRect {
                    x: 24,
                    y: 40,
                    width: 2,
                    height,
                    color: 0xFFFF_FFFF,
                } if *height == LINE_HEIGHT_PX
            )
        }));
    }

    #[test_case]
    fn test_tick_enter_resets_hidden_cursor_to_visible_on_empty_prompt() {
        let buffer = test_buffer(Region::new(0, 0, 640, 480));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);
        let mut runtime = runtime(10, Some(1_000));
        let mut environment = FakeEnvironment {
            frame_count: 10,
            uptime_ms: Some(1_000),
            ..FakeEnvironment::default()
        };

        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.uptime_ms = Some(1_600);
        runtime.tick(&mut environment, &mut writer);
        let _ = flushed_commands(&mut writer, &buffer);

        environment.events.push_back(key_event(KeyCode::Enter));
        runtime.tick(&mut environment, &mut writer);
        let commands = flushed_commands(&mut writer, &buffer);

        assert_eq!(commands.len(), 1);
        assert!(matches!(
            commands[0],
            DrawCommand::FillRect {
                x: 16,
                y: 40,
                width: 2,
                height,
                color: 0xFFFF_FFFF,
            } if height == LINE_HEIGHT_PX
        ));
    }

    #[cfg(feature = "visualize-input")]
    #[test_case]
    fn test_visualization_toggle_changes_shell_viewport_width() {
        crate::input_trace::reset_for_test();

        let mut runtime = runtime_with_size(1024, 768, 0, Some(0));
        assert_eq!(runtime.layout.width_px(), 1024);

        runtime.apply_visualization_command(VisualizationCommand {
            target: VisualizationTarget::Input,
            action: VisualizationAction::On,
        });
        runtime.sync_layout();
        assert_eq!(
            runtime.layout.width_px(),
            crate::input_trace::shell_viewport_width(1024)
        );

        runtime.apply_visualization_command(VisualizationCommand {
            target: VisualizationTarget::Input,
            action: VisualizationAction::Off,
        });
        runtime.sync_layout();
        assert_eq!(runtime.layout.width_px(), 1024);
    }
}
