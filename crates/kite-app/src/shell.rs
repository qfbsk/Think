use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use alacritty_terminal::index::{Column, Line, Point};
use glyphon::FontSystem;
use glyphon::cosmic_text::{
    AttrsList, Buffer, BufferLine, Color, Cursor, Metrics, Shaping, Weight, Wrap,
};
use kite_core::display_path;
use unicode_segmentation::UnicodeSegmentation;

use crate::banner;
use crate::terminal::{MouseReport, Run, Terminal};
use crate::text::{attrs, byte_to_char, char_to_byte, make_label, measure};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Output,
    Error,
    Echo,
}

/// One character of a screen row: what to draw, in which colour, bold or not.
type Cell = (char, Color, bool);

const OS_ICON: char = '\u{f17a}';
const GIT_BRANCH_ICON: char = '\u{f418}';
#[cfg(windows)]
pub(crate) const NO_WINDOW: u32 = 0x0800_0000;

/// The prompt of one input line, split into the parts that get their own
/// colors. Byte ranges are offsets into the line's own text.
#[derive(Clone, Debug)]
struct Prompt {
    text: String,
    identity: std::ops::Range<usize>,
    path: std::ops::Range<usize>,
    git: std::ops::Range<usize>,
    branch: std::ops::Range<usize>,
    arrow: std::ops::Range<usize>,
}

impl Prompt {
    fn len(&self) -> usize {
        self.arrow.end
    }

    fn chars(&self) -> usize {
        self.text.chars().count()
    }

    fn paint(&self, list: &mut AttrsList, family: &'static str) {
        let Some(prompt) = theme::prompt() else {
            list.add_span(0..self.len(), &attrs(family, theme::text_dim()));
            return;
        };
        if !self.identity.is_empty() {
            list.add_span(self.identity.clone(), &attrs(family, theme::accent()));
        }
        list.add_span(self.path.clone(), &attrs(family, prompt.path));
        if !self.git.is_empty() {
            list.add_span(self.git.clone(), &attrs(family, theme::accent()));
            list.add_span(
                self.branch.clone(),
                &attrs(family, theme::text()).weight(Weight::BOLD),
            );
        }
        list.add_span(self.arrow.clone(), &attrs(family, prompt.arrow));
    }
}

pub enum ShellEffect {
    Open(PathBuf),
    Retheme,
    Restart,
    Miku,
}

/// A cell of the terminal grid. `line` counts from the top of the viewport, so
/// a negative line is scrollback above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridPos {
    pub line: i32,
    pub column: usize,
}

impl GridPos {
    fn point(self) -> Point {
        Point::new(Line(self.line), Column(self.column))
    }

    fn report(self, press: bool, drag: bool) -> MouseReport {
        MouseReport {
            button: 0,
            press,
            drag,
            line: self.line,
            column: self.column,
        }
    }
}

/// A key press, already stripped of everything kite's own line editor would
/// have done with it. While a child holds the screen these are the only keys
/// that mean anything, and they go to it as VT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalKey {
    Text(String),
    Enter,
    Backspace,
    Delete,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Function(u8),
}

pub struct Shell {
    pub cwd: PathBuf,
    pub input: String,
    pub input_cursor: usize,
    pub input_selection: Option<(usize, usize)>,
    pub buffer: Buffer,
    /// Which row of the view holds the input line. It is the bottom row while
    /// the session runs normally, but a full screen program can hand the
    /// cursor back somewhere higher up.
    input_row: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    /// The child's screen. While it runs, this is what the tab shows.
    terminal: Terminal,
    family: &'static str,
    font_size: f32,
    line_height: f32,
    view_width: f32,
    view_height: f32,
    prompt: Prompt,
    branch: Option<String>,
    identity: String,
}

/// Kills the whole process tree. The helper is a console program, so without
/// `CREATE_NO_WINDOW` it flashes a console and steals kite's focus.
fn kill_tree(pid: u32) -> bool {
    let pid = pid.to_string();
    let mut command = Command::new("taskkill");
    command
        .args(["/PID", &pid, "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(NO_WINDOW);
    }
    command.status().is_ok()
}

impl Shell {
    pub fn new(
        font_system: &mut FontSystem,
        family: &'static str,
        font_size: f32,
        view_width: f32,
        view_height: f32,
    ) -> Self {
        let cwd = kite_core::start_dir();
        let branch = git_branch(&cwd);
        let identity = identity_label();
        let prompt = build_prompt(&cwd, branch.as_deref(), &identity);
        let line_height = (font_size * theme::LINE_HEIGHT_RATIO).ceil();
        let cell = cell_width(font_system, family, font_size);
        let buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
        let mut shell = Self {
            cwd,
            input: String::new(),
            input_cursor: 0,
            input_selection: None,
            buffer,
            input_row: 0,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            terminal: Terminal::new(
                (view_width.max(1.0), view_height.max(1.0)),
                (cell, line_height),
            ),
            family,
            font_size,
            line_height,
            view_width: view_width.max(1.0),
            view_height: view_height.max(1.0),
            prompt,
            branch,
            identity,
        };
        for line in banner::lines() {
            let cells = colored(&line.text, &line.spans);
            shell.write_cells(&cells, true);
        }
        shell.emit_input();
        shell.rebuild_buffer(font_system);
        shell
    }

    pub fn set_viewport(&mut self, font_system: &mut FontSystem, width: f32, height: f32) {
        let width = width.max(1.0);
        let height = height.max(1.0);
        if (width - self.view_width).abs() < 0.01 && (height - self.view_height).abs() < 0.01 {
            return;
        }
        self.view_width = width;
        self.view_height = height;
        let cell = self.terminal.cell().0;
        self.terminal
            .resize((width, height), (cell, self.line_height));
        self.rebuild_buffer(font_system);
    }

    pub fn set_font_size(&mut self, font_system: &mut FontSystem, font_size: f32) {
        if (font_size - self.font_size).abs() < 0.01 {
            return;
        }
        self.font_size = font_size;
        self.line_height = (font_size * theme::LINE_HEIGHT_RATIO).ceil();
        let cell = cell_width(font_system, self.family, font_size);
        self.terminal.resize(
            (self.view_width, self.view_height),
            (cell, self.line_height),
        );
        self.rebuild_buffer(font_system);
    }

    pub fn retheme(&mut self, font_system: &mut FontSystem) {
        self.rebuild_buffer(font_system);
    }

    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    pub fn shape(&mut self, font_system: &mut FontSystem) {
        self.buffer.shape_until_scroll(font_system, true);
    }

    pub fn is_running(&self) -> bool {
        self.terminal.running()
    }

    fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
        self.refresh_branch();
    }

    /// Re-reads HEAD so a branch switched from an external program shows up.
    fn refresh_branch(&mut self) {
        self.branch = git_branch(&self.cwd);
        let prompt = build_prompt(&self.cwd, self.branch.as_deref(), &self.identity);
        self.prompt = prompt;
    }

    pub fn insert_text(&mut self, font_system: &mut FontSystem, text: &str) {
        self.delete_selection();
        let byte = char_to_byte(&self.input, self.input_cursor);
        self.input.insert_str(byte, text);
        self.input_cursor += text.chars().count();
        self.input_selection = None;
        self.refresh_prompt(font_system);
    }

    pub fn backspace(&mut self, font_system: &mut FontSystem) {
        if self.delete_selection() {
            self.refresh_prompt(font_system);
            return;
        }
        if let Some(previous) = grapheme_prev(&self.input, self.input_cursor) {
            let from = char_to_byte(&self.input, previous);
            let to = char_to_byte(&self.input, self.input_cursor);
            self.input.replace_range(from..to, "");
            self.input_cursor = previous;
            self.refresh_prompt(font_system);
        }
    }

    pub fn delete_forward(&mut self, font_system: &mut FontSystem) {
        if self.delete_selection() {
            self.refresh_prompt(font_system);
            return;
        }
        if let Some(next) = grapheme_next(&self.input, self.input_cursor) {
            let from = char_to_byte(&self.input, self.input_cursor);
            let to = char_to_byte(&self.input, next);
            self.input.replace_range(from..to, "");
            self.refresh_prompt(font_system);
        }
    }

    pub fn move_left(&mut self, font_system: &mut FontSystem) {
        if let Some((start, _)) = self.input_selection.take() {
            self.input_cursor = start;
        } else if let Some(previous) = grapheme_prev(&self.input, self.input_cursor) {
            self.input_cursor = previous;
        }
        self.refresh_prompt(font_system);
    }

    pub fn move_right(&mut self, font_system: &mut FontSystem) {
        if let Some((_, end)) = self.input_selection.take() {
            self.input_cursor = end;
        } else if let Some(next) = grapheme_next(&self.input, self.input_cursor) {
            self.input_cursor = next;
        }
        self.refresh_prompt(font_system);
    }

    pub fn move_word_left(&mut self, font_system: &mut FontSystem) {
        self.input_selection = None;
        let chars: Vec<char> = self.input.chars().collect();
        let mut ch = self.input_cursor.min(chars.len());
        while ch > 0 && !is_word_char(chars[ch - 1]) {
            ch -= 1;
        }
        while ch > 0 && is_word_char(chars[ch - 1]) {
            ch -= 1;
        }
        self.input_cursor = ch;
        self.refresh_prompt(font_system);
    }

    pub fn move_word_right(&mut self, font_system: &mut FontSystem) {
        self.input_selection = None;
        let chars: Vec<char> = self.input.chars().collect();
        let mut ch = self.input_cursor.min(chars.len());
        if ch < chars.len() {
            if is_word_char(chars[ch]) {
                while ch < chars.len() && is_word_char(chars[ch]) {
                    ch += 1;
                }
            } else {
                while ch < chars.len() && !is_word_char(chars[ch]) {
                    ch += 1;
                }
            }
        }
        self.input_cursor = ch;
        self.refresh_prompt(font_system);
    }

    pub fn move_line_start(&mut self, font_system: &mut FontSystem) {
        self.input_selection = None;
        self.input_cursor = 0;
        self.refresh_prompt(font_system);
    }

    pub fn move_line_end(&mut self, font_system: &mut FontSystem) {
        self.input_selection = None;
        self.input_cursor = self.input.chars().count();
        self.refresh_prompt(font_system);
    }

    /// Everything on the grid, scrollback included.
    pub fn select_all(&mut self, font_system: &mut FontSystem) {
        self.input_selection = None;
        self.refresh_prompt(font_system);
        let rows = self.terminal.rows() as i32;
        let last_column = self.terminal.columns().saturating_sub(1);
        let top = Line(self.terminal.top_line() - self.terminal.history());
        self.terminal.select(
            Point::new(top, Column(0)),
            Point::new(Line(rows - 1), Column(last_column)),
        );
    }

    pub fn selected_text(&self) -> Option<String> {
        if let Some(text) = self.terminal.selected_text() {
            return Some(text);
        }
        let (start, end) = self.input_selection?;
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let chars: Vec<char> = self.input.chars().collect();
        let start = start.min(chars.len());
        let end = end.min(chars.len());
        Some(chars[start..end].iter().collect())
    }

    /// The grid cell under a point of the shell's panel. Rows above the
    /// viewport are scrollback, so the line can be negative.
    pub fn position_at(&self, x: f32, y: f32) -> Option<GridPos> {
        let (cell, height) = self.terminal.cell();
        let row = (y / height.max(f32::EPSILON)).floor() as i32;
        if row < 0 || row >= self.terminal.rows() as i32 {
            return None;
        }
        let column = (x / cell.max(f32::EPSILON)).floor() as usize;
        Some(GridPos {
            line: self.terminal.top_line() + row,
            column: column.min(self.terminal.columns().saturating_sub(1)),
        })
    }

    /// Which character of the input line a point sits on, if it sits on it.
    fn input_position(&mut self, font_system: &mut FontSystem, x: f32, y: f32) -> Option<usize> {
        if self.owns_screen() {
            return None;
        }
        self.buffer.shape_until_scroll(font_system, true);
        let row = self.input_row;
        let cursor = self.buffer.hit(x, y)?;
        if cursor.line != row {
            return None;
        }
        let text = self.buffer.lines.get(row)?.text();
        Some(byte_to_char(text, cursor.index).saturating_sub(self.input_line_chars()))
    }

    pub fn mouse_press(&mut self, font_system: &mut FontSystem, x: f32, y: f32) -> Option<GridPos> {
        let position = self.position_at(x, y)?;
        if self.terminal.mouse_reporting() {
            self.terminal.send_mouse(position.report(false, false));
            return None;
        }
        if let Some(ch) = self.input_position(font_system, x, y) {
            self.input_cursor = ch.min(self.input.chars().count());
            self.input_selection = None;
            self.refresh_prompt(font_system);
        }
        self.terminal.select(position.point(), position.point());
        Some(position)
    }

    pub fn mouse_drag(&mut self, anchor: GridPos, cursor: GridPos) {
        if self.terminal.mouse_reporting() {
            self.terminal.send_mouse(cursor.report(true, true));
            return;
        }
        self.input_selection = None;
        self.terminal.select(anchor.point(), cursor.point());
    }

    /// Letting go of the mouse is a report like any other: a child that asked
    /// for the mouse wants to hear the button come up.
    pub fn mouse_release(&mut self, x: f32, y: f32) {
        if let Some(position) = self.position_at(x, y) {
            self.terminal.send_mouse(position.report(false, false));
        }
    }

    pub fn cut_selection(&mut self, font_system: &mut FontSystem) -> Option<String> {
        let text = self.selected_text()?;
        if self.delete_selection() {
            self.refresh_prompt(font_system);
        }
        Some(text)
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.input_selection else {
            return false;
        };
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let from = char_to_byte(&self.input, start);
        let to = char_to_byte(&self.input, end);
        self.input.replace_range(from..to, "");
        self.input_cursor = start;
        self.input_selection = None;
        true
    }

    pub fn history_up(&mut self, font_system: &mut FontSystem) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => {
                self.history_draft = self.input.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_index = Some(index);
        self.input = self.history[index].clone();
        self.input_cursor = self.input.chars().count();
        self.input_selection = None;
        self.refresh_prompt(font_system);
    }

    pub fn history_down(&mut self, font_system: &mut FontSystem) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.input = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.input = std::mem::take(&mut self.history_draft);
        }
        self.input_cursor = self.input.chars().count();
        self.input_selection = None;
        self.refresh_prompt(font_system);
    }

    pub fn submit(&mut self, font_system: &mut FontSystem) -> Option<ShellEffect> {
        self.input_selection = None;
        self.terminal.clear_selection();
        self.terminal.scroll_to_bottom();
        let command = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        self.history_index = None;
        self.history_draft.clear();

        let prompt = self.prompt.clone();
        self.push_echo(prompt, &command);

        let trimmed = command.trim();
        if trimmed.is_empty() {
            self.rebuild_buffer(font_system);
            return None;
        }
        if self.history.last().map(String::as_str) != Some(trimmed) {
            self.history.push(trimmed.to_string());
        }

        let args = split_args(trimmed);
        if args.is_empty() {
            self.rebuild_buffer(font_system);
            return None;
        }
        let mut effect = None;
        if !self.run_builtin(&args, &mut effect) {
            self.spawn_external(&args);
        }
        self.rebuild_buffer(font_system);
        effect
    }

    /// Take what the child wrote. While it runs the tab *is* its screen; once
    /// it is gone that screen becomes history, the way a terminal window's
    /// scrollback does.
    pub fn poll(&mut self, font_system: &mut FontSystem) -> bool {
        if !self.terminal.running() {
            return false;
        }
        let changed = self.terminal.poll();
        let finished = self.terminal.take_exit();
        if self.terminal.running() {
            if changed {
                self.rebuild_buffer(font_system);
            }
            return changed;
        }
        // The child's screen is already part of the grid; all that is left is
        // to take back a row of our own below it.
        self.terminal.feed(b"\r\n");
        self.refresh_branch();
        if let Some(Some(status)) = finished {
            if !status.success() {
                let code = status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "?".to_string());
                self.push_line(format!("[exit {code}]"), LineKind::Error);
            }
        }
        self.emit_input();
        self.rebuild_buffer(font_system);
        true
    }

    /// Ctrl+C belongs to the console, not to kite: the byte goes into the pty
    /// and the child's own line discipline answers for it.
    pub fn interrupt(&mut self) -> bool {
        if !self.terminal.running() {
            return false;
        }
        self.terminal.send(&[0x03]);
        true
    }

    pub fn kill_job(&mut self) {
        if let Some(pid) = self.terminal.pid() {
            kill_tree(pid);
        }
        self.terminal.kill();
    }

    pub fn print_error(&mut self, font_system: &mut FontSystem, message: String) {
        self.push_line(message, LineKind::Error);
        self.rebuild_buffer(font_system);
    }

    /// One wheel notch. `x`/`y` are where the notch happened, because a child
    /// that asked for the mouse wants to hear about it rather than be scrolled.
    pub fn scroll_by(&mut self, font_system: &mut FontSystem, x: f32, y: f32, delta: f32) {
        if delta.abs() < f32::EPSILON {
            return;
        }
        let lines = (delta / self.line_height).round() as i32;
        if self.terminal.mouse_reporting() {
            if let Some(position) = self.position_at(x, y) {
                let button = if lines > 0 { 65 } else { 64 };
                for press in [true, false] {
                    self.terminal.send_mouse(MouseReport {
                        button,
                        press,
                        drag: false,
                        line: position.line,
                        column: position.column,
                    });
                }
            }
            return;
        }
        self.terminal.scroll_by(lines);
        self.rebuild_buffer(font_system);
    }

    pub fn cursor_rect(&self) -> Option<(f32, f32, f32)> {
        if self.owns_screen() {
            let (column, line) = self.terminal.cursor()?;
            let row = line - self.terminal.top_line();
            if row < 0 || row >= self.terminal.rows() as i32 {
                return None;
            }
            let (cell, height) = self.terminal.cell();
            return Some((column as f32 * cell, row as f32 * height, height));
        }
        let line = self.input_row;
        let byte = self.input_line_bytes() + char_to_byte(&self.input, self.input_cursor);
        let cursor = Cursor::new(line, byte);
        for run in self.buffer.layout_runs() {
            if run.line_i != line {
                continue;
            }
            if let Some(x) = run.cursor_position(&cursor) {
                return Some((x, run.line_top, run.line_height));
            }
        }
        None
    }

    /// Highlighted rectangles, in panel coordinates. The grid is a fixed cell
    /// matrix, so a selection needs no text layout to be drawn.
    pub fn selection_spans(&self) -> Vec<(f32, f32, f32, f32)> {
        let (cell, height) = self.terminal.cell();
        if let Some((start, end)) = self.terminal.selection() {
            if start == end {
                return Vec::new();
            }
            let top = self.terminal.top_line();
            let bottom = top + self.terminal.rows() as i32 - 1;
            let last_column = self.terminal.columns();
            let mut spans = Vec::new();
            for row in start.line.0.max(top)..=end.line.0.min(bottom) {
                let from = if row == start.line.0 {
                    start.column.0
                } else {
                    0
                };
                let to = if row == end.line.0 {
                    end.column.0 + 1
                } else {
                    last_column
                };
                if to > from {
                    spans.push((
                        from as f32 * cell,
                        (row - top) as f32 * height,
                        (to - from) as f32 * cell,
                        height,
                    ));
                }
            }
            return spans;
        }

        let Some((start, end)) = self.input_selection else {
            return Vec::new();
        };
        let line = self.input_row;
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let start_byte = self.input_line_bytes() + char_to_byte(&self.input, start);
        let end_byte = self.input_line_bytes() + char_to_byte(&self.input, end);
        let start_cursor = Cursor::new(line, start_byte);
        let end_cursor = Cursor::new(line, end_byte);
        let mut spans = Vec::new();
        for run in self.buffer.layout_runs() {
            if run.line_i != line {
                continue;
            }
            for (x, width) in run.highlight(start_cursor, end_cursor) {
                spans.push((x, run.line_top, width, run.line_height));
            }
        }
        spans
    }

    /// Where the caret is on the screen, in rows and columns.
    pub fn cursor_line_column(&self) -> (usize, usize) {
        let (column, row) = self.terminal.cursor_point();
        (row + 1, column + 1)
    }

    fn refresh_prompt(&mut self, font_system: &mut FontSystem) {
        if self.owns_screen() {
            return;
        }
        self.terminal.scroll_to_bottom();
        self.terminal.clear_selection();
        self.emit_input();
        self.rebuild_buffer(font_system);
    }

    /// The grid is the only surface: kite writes its prompt into it and the
    /// child writes its screen into it, so one rebuild covers both. Whatever
    /// the view is looking at, live or scrollback, comes from the grid.
    fn rebuild_buffer(&mut self, font_system: &mut FontSystem) {
        let mut buffer = Buffer::new_empty(Metrics::new(self.font_size, self.line_height));
        buffer.set_wrap(Wrap::None);
        buffer.set_tab_width(4);
        buffer.set_size(Some(self.view_width), Some(self.view_height));
        let base = attrs(self.family, theme::text());
        let top = self.terminal.top_line();
        for row in 0..self.terminal.rows() as i32 {
            let mut text = String::new();
            let mut list = AttrsList::new(&base);
            for Run {
                text: run,
                color,
                bold,
            } in self.terminal.runs(top + row)
            {
                let start = char_to_byte(&text, text.chars().count());
                text.push_str(&run);
                let end = char_to_byte(&text, text.chars().count());
                let attrs = match color {
                    Some(color) => attrs(self.family, color),
                    None => attrs(self.family, theme::text()),
                };
                let styled;
                list.add_span(
                    start..end,
                    if bold {
                        styled = attrs.weight(Weight::BOLD);
                        &styled
                    } else {
                        &attrs
                    },
                );
            }
            buffer.lines.push(BufferLine::new(
                text,
                glyphon::cosmic_text::LineEnding::Lf,
                list,
                Shaping::Advanced,
            ));
        }
        self.buffer = buffer;
        self.buffer.shape_until_scroll(font_system, true);
    }

    /// While a child holds the pseudo console the viewport is its screen: kite
    /// draws no prompt line and edits no input line underneath it.
    fn owns_screen(&self) -> bool {
        self.terminal.running()
    }

    fn input_line_bytes(&self) -> usize {
        if self.owns_screen() {
            0
        } else {
            self.prompt.len()
        }
    }

    fn input_line_chars(&self) -> usize {
        if self.owns_screen() {
            0
        } else {
            self.prompt.chars()
        }
    }

    fn prompt_line(&self) -> (String, AttrsList) {
        let prompt_bytes = self.input_line_bytes();
        let text = if self.owns_screen() {
            self.input.clone()
        } else {
            format!("{}{}", self.prompt.text, self.input)
        };
        let base = attrs(
            self.family,
            theme::prompt().map_or(theme::text(), |prompt| prompt.input),
        );
        let mut list = AttrsList::new(&base);
        if !self.owns_screen() {
            self.prompt.paint(&mut list, self.family);
            add_syntax_spans(&mut list, self.family, &self.input, prompt_bytes);
        }
        (text, list)
    }

    /// One row of output, with the input line put back underneath it. The grid
    /// holds the colours, so there is no second copy of the history to keep in
    /// step.
    fn push_line(&mut self, text: String, kind: LineKind) {
        let color = match kind {
            LineKind::Error => theme::red(),
            LineKind::Echo => theme::text_dim(),
            LineKind::Output => theme::text(),
        };
        self.erase_input();
        let cells: Vec<Cell> = text.chars().map(|ch| (ch, color, false)).collect();
        self.write_cells(&cells, true);
        self.emit_input();
    }

    /// Enter closes the input row. What the user typed is already on screen,
    /// prompt and syntax colours and all, so the echo needs no second copy.
    fn push_echo(&mut self, _prompt: Prompt, _command: &str) {
        self.terminal.feed(b"\x1b[999C\r\n");
    }

    /// Take the input line off the screen. Only its own row: whatever sits
    /// below belongs to the session, and a full screen program can hand the
    /// cursor back with the session still underneath it.
    fn erase_input(&mut self) {
        self.terminal.feed(b"\r\x1b[K");
    }

    /// The prompt and what has been typed, on the current row, with the caret
    /// left where the line editor says it is. A command longer than the row
    /// scrolls to the left instead of wrapping, so the row stays one row.
    fn emit_input(&mut self) {
        let (text, list) = self.prompt_line();
        let mut cells = cells_of(&text, &list);
        let columns = self.terminal.columns().max(2);
        let caret = (self.input_line_chars() + self.input_cursor).min(cells.len());
        let hidden = caret.saturating_sub(columns - 2);
        let shown: Vec<Cell> = cells.drain(hidden..).take(columns - 1).collect();
        let tail = shown.len().saturating_sub(caret - hidden);
        self.terminal.feed(b"\r\x1b[K");
        self.write_cells(&shown, false);
        if tail > 0 {
            self.terminal.feed(format!("\x1b[{tail}D").as_bytes());
        }
        self.input_row = (self.terminal.cursor_line() - self.terminal.top_line()).max(0) as usize;
    }

    /// Cells out as SGR runs. `newline` ends the row, which is what makes the
    /// grid scroll once the viewport is full.
    fn write_cells(&mut self, cells: &[Cell], newline: bool) {
        let mut bytes: Vec<u8> = Vec::new();
        let mut index = 0;
        while index < cells.len() {
            let (_, color, bold) = cells[index];
            let mut run = String::new();
            while index < cells.len() && cells[index].1 == color && cells[index].2 == bold {
                run.push(cells[index].0);
                index += 1;
            }
            bytes.extend(sgr(color, bold));
            bytes.extend(run.as_bytes());
        }
        bytes.extend(b"\x1b[0m");
        if newline {
            bytes.extend(b"\r\n");
        }
        self.terminal.feed(&bytes);
    }

    fn run_builtin(&mut self, args: &[String], effect: &mut Option<ShellEffect>) -> bool {
        let name = args[0].to_lowercase();
        let rest = &args[1..];
        match name.as_str() {
            "help" => {
                for line in [
                    "kite built-ins:",
                    "  cd [dir]        change directory",
                    "  pwd             print working directory",
                    "  ls [dir]        list a directory      (dir)",
                    "  cp <src> <dst>  copy a file           (copy)",
                    "  mv <src> <dst>  move or rename        (move, ren)",
                    "  rm <path...>    move to the Recycle Bin",
                    "                    (del, erase, rd, rmdir)",
                    "  mkdir <dir>     create directories    (md)",
                    "  touch <file>    create an empty file  (ni)",
                    "  echo <text>     print text",
                    "  kkk <file>      open a file in a new tab",
                    "  theme <name>    switch theme (index, neo)",
                    "  cls / clear     clear the screen",
                    "  restart         start a new shell, other tabs untouched",
                    "",
                    "Anything else runs as an external program.",
                ] {
                    self.push_line(line.to_string(), LineKind::Output);
                }
                true
            }
            "clear" | "cls" => {
                // The same two sequences a shell's Clear-Host sends: wipe the
                // view, then the scrollback it scrolled into.
                self.terminal.feed(b"\x1b[2J\x1b[3J\x1b[H");
                self.emit_input();
                true
            }
            "miku" => {
                *effect = Some(ShellEffect::Miku);
                true
            }
            "restart" => {
                *effect = Some(ShellEffect::Restart);
                true
            }
            "theme" => {
                let Some(name) = rest.first() else {
                    self.push_line("theme: expected index or neo".to_string(), LineKind::Error);
                    return true;
                };
                match theme::set_preset(name) {
                    Ok(applied) => {
                        self.push_line(format!("theme: {applied}"), LineKind::Output);
                        *effect = Some(ShellEffect::Retheme);
                    }
                    Err(error) => self.push_line(format!("theme: {error}"), LineKind::Error),
                }
                true
            }
            "pwd" => {
                self.push_line(display_path(&self.cwd), LineKind::Output);
                true
            }
            "cd" => {
                let target = rest
                    .first()
                    .map_or_else(|| home_dir(), |arg| resolve_path(&self.cwd, arg));
                match fs::canonicalize(&target) {
                    Ok(path) if path.is_dir() => self.set_cwd(path),
                    Ok(path) => self.push_line(
                        format!("cd: not a directory: {}", path.display()),
                        LineKind::Error,
                    ),
                    Err(error) => {
                        self.push_line(format!("cd: {error}"), LineKind::Error);
                    }
                }
                true
            }
            "ls" | "dir" => {
                let target = rest
                    .first()
                    .map_or_else(|| self.cwd.clone(), |arg| resolve_path(&self.cwd, arg));
                match list_directory(&target) {
                    Ok(entries) => {
                        if entries.is_empty() {
                            self.push_line("(empty)".to_string(), LineKind::Echo);
                        }
                        for entry in entries {
                            self.push_line(entry, LineKind::Output);
                        }
                    }
                    Err(error) => self.push_line(format!("{name}: {error}"), LineKind::Error),
                }
                true
            }
            "cp" | "copy" => {
                if rest.len() != 2 {
                    self.push_line(format!("{name}: expected <src> <dst>"), LineKind::Error);
                    return true;
                }
                let src = resolve_path(&self.cwd, &rest[0]);
                let dst = resolve_path(&self.cwd, &rest[1]);
                let dst = if dst.is_dir() {
                    src.file_name().map(|name| dst.join(name)).unwrap_or(dst)
                } else {
                    dst
                };
                match fs::copy(&src, &dst) {
                    Ok(_) => {}
                    Err(error) => self.push_line(format!("{name}: {error}"), LineKind::Error),
                }
                true
            }
            "mv" | "move" | "ren" => {
                if rest.len() != 2 {
                    self.push_line(format!("{name}: expected <src> <dst>"), LineKind::Error);
                    return true;
                }
                let src = resolve_path(&self.cwd, &rest[0]);
                let dst = resolve_path(&self.cwd, &rest[1]);
                let dst = if dst.is_dir() {
                    src.file_name().map(|name| dst.join(name)).unwrap_or(dst)
                } else {
                    dst
                };
                match fs::rename(&src, &dst) {
                    Ok(()) => {}
                    Err(error) => self.push_line(format!("{name}: {error}"), LineKind::Error),
                }
                true
            }
            "rm" | "del" | "erase" | "rd" | "rmdir" => {
                if rest.is_empty() {
                    self.push_line(
                        format!("{name}: expected at least one path"),
                        LineKind::Error,
                    );
                    return true;
                }
                for arg in rest {
                    let path = resolve_path(&self.cwd, arg);
                    match trash::delete(&path) {
                        Ok(()) => {}
                        Err(error) => self.push_line(
                            format!("{name}: {}: {error}", path.display()),
                            LineKind::Error,
                        ),
                    }
                }
                true
            }
            "mkdir" | "md" => {
                if rest.is_empty() {
                    self.push_line(format!("{name}: expected a directory"), LineKind::Error);
                }
                for arg in rest {
                    let path = resolve_path(&self.cwd, arg);
                    if let Err(error) = fs::create_dir_all(&path) {
                        self.push_line(format!("{name}: {error}"), LineKind::Error);
                    }
                }
                true
            }
            "touch" | "ni" => {
                if rest.is_empty() {
                    self.push_line(format!("{name}: expected a file"), LineKind::Error);
                }
                for arg in rest {
                    let path = resolve_path(&self.cwd, arg);
                    if let Err(error) = fs::OpenOptions::new().create(true).append(true).open(&path)
                    {
                        self.push_line(format!("{name}: {error}"), LineKind::Error);
                    }
                }
                true
            }
            "echo" => {
                self.push_line(rest.join(" "), LineKind::Output);
                true
            }
            "kkk" => {
                let Some(arg) = rest.first() else {
                    self.push_line("kkk: expected a file".to_string(), LineKind::Error);
                    return true;
                };
                let path = resolve_path(&self.cwd, arg);
                match fs::canonicalize(&path) {
                    Ok(path) if path.is_file() => *effect = Some(ShellEffect::Open(path)),
                    Ok(path) => self.push_line(
                        format!("kkk: not a file: {}", path.display()),
                        LineKind::Error,
                    ),
                    Err(error) => self.push_line(format!("kkk: {error}"), LineKind::Error),
                }
                true
            }
            _ => false,
        }
    }

    /// Hand the command a pseudo console: it gets a screen, a cursor of its
    /// own, and the line discipline that echoes what it asks for.
    fn spawn_external(&mut self, args: &[String]) {
        let program = command_path(&self.cwd, &args[0]);
        if let Err(error) = self.terminal.start(&program, &args[1..], &self.cwd) {
            self.push_line(format!("{}: {error}", args[0]), LineKind::Error);
        }
    }

    /// Keys while the child holds the screen. None of them reach kite's line
    /// editor: the child echoes itself, as it does in every other terminal.
    pub fn feed_key(&mut self, key: TerminalKey) {
        let bytes = match key {
            TerminalKey::Text(text) => text.into_bytes(),
            TerminalKey::Enter => b"\r".to_vec(),
            TerminalKey::Backspace => vec![0x7f],
            TerminalKey::Delete => b"\x1b[3~".to_vec(),
            TerminalKey::Tab => b"\t".to_vec(),
            TerminalKey::Escape => vec![0x1b],
            TerminalKey::Up => b"\x1b[A".to_vec(),
            TerminalKey::Down => b"\x1b[B".to_vec(),
            TerminalKey::Right => b"\x1b[C".to_vec(),
            TerminalKey::Left => b"\x1b[D".to_vec(),
            TerminalKey::Home => b"\x1b[H".to_vec(),
            TerminalKey::End => b"\x1b[F".to_vec(),
            TerminalKey::PageUp => b"\x1b[5~".to_vec(),
            TerminalKey::PageDown => b"\x1b[6~".to_vec(),
            TerminalKey::Function(number) => function_key(number),
        };
        self.terminal.send(&bytes);
    }
}

/// F1-F4 are SS3, F5-F12 are the CSI codes the xterm table assigns them.
fn function_key(number: u8) -> Vec<u8> {
    const CSI: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
    match number {
        1..=4 => vec![0x1b, b'O', b'P' + number - 1],
        5..=12 => format!("\x1b[{}~", CSI[(number - 5) as usize]).into_bytes(),
        _ => Vec::new(),
    }
}

/// A cell's colour as SGR. Truecolour keeps the exact values from `theme.toml`
/// instead of rounding them to the sixteen ANSI slots.
fn sgr(color: Color, bold: bool) -> Vec<u8> {
    let (r, g, b, _) = color.as_rgba_tuple();
    if bold {
        format!("\x1b[1;38;2;{r};{g};{b}m").into_bytes()
    } else {
        format!("\x1b[38;2;{r};{g};{b}m").into_bytes()
    }
}

/// Flatten a styled line into one entry per character, so the row can be
/// clipped to the grid without splitting a span's bookkeeping.
fn cells_of(text: &str, list: &AttrsList) -> Vec<Cell> {
    text.char_indices()
        .map(|(byte, ch)| {
            let attrs = list.get_span(byte);
            let color = attrs.color_opt.unwrap_or_else(theme::text);
            (ch, color, attrs.weight == Weight::BOLD)
        })
        .collect()
}

/// The same, from plain byte-range colour spans.
fn colored(text: &str, spans: &[(std::ops::Range<usize>, Color)]) -> Vec<Cell> {
    let mut cells: Vec<Cell> = text.chars().map(|ch| (ch, theme::text(), false)).collect();
    for (range, color) in spans {
        let start = byte_to_char(text, range.start);
        let end = byte_to_char(text, range.end.min(text.len()));
        for cell in &mut cells[start..end] {
            cell.1 = *color;
        }
    }
    cells
}

/// One terminal cell is one glyph of the body font, measured rather than
/// guessed so the grid and the pixels agree at any size.
fn cell_width(font_system: &mut FontSystem, family: &'static str, font_size: f32) -> f32 {
    const SAMPLES: usize = 32;
    let probe = make_label(
        font_system,
        &attrs(family, theme::text()),
        font_size,
        &"0".repeat(SAMPLES),
    );
    let (width, _) = measure(&probe);
    (width / SAMPLES as f32).max(1.0)
}

fn list_directory(path: &Path) -> std::io::Result<Vec<String>> {
    if path.is_file() {
        return Ok(vec![
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        ]);
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() {
            name.push('/');
        }
        entries.push((!file_type.is_dir(), name.to_lowercase(), name));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    Ok(entries.into_iter().map(|(_, _, name)| name).collect())
}

/// `CreateProcess` looks a relative program name up in the directory of the
/// *calling* process, which is where kite was launched from, not this tab's
/// `cwd`. So a token that spells a path is made absolute here first; a bare
/// name is left alone and keeps the system PATH search.
fn command_path(cwd: &Path, token: &str) -> PathBuf {
    if token.contains('/') || token.contains('\\') {
        resolve_path(cwd, token)
    } else {
        PathBuf::from(token)
    }
}

pub fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let value = value.trim_matches('"');
    if value == "~" {
        return home_dir();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home_dir().join(rest);
    }
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\"))
}

/// `` + `user@host`, the segment that opens every prompt line.
fn identity_label() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let host = std::env::var("COMPUTERNAME").unwrap_or_default();
    if user.is_empty() || host.is_empty() {
        return String::new();
    }
    format!("{OS_ICON} {user}@{host}")
}

/// `  ⑂  branch > ` when the directory is inside a work tree, `> ` when it is
/// not. The ranges are byte offsets into the prompt text.
fn build_prompt(cwd: &Path, branch: Option<&str>, identity: &str) -> Prompt {
    let mut text = String::new();
    let identity_start = text.len();
    text.push_str(identity);
    if !identity.is_empty() {
        text.push_str("  ");
    }
    let identity = identity_start..(identity_start + identity.len());
    let path_start = text.len();
    text.push_str(&display_path(cwd));
    let path_end = text.len();
    let mut git = path_end..path_end;
    let mut name = path_end..path_end;
    if let Some(branch) = branch {
        let start = text.len();
        text.push_str("  ");
        text.push(GIT_BRANCH_ICON);
        text.push_str("  ");
        let name_start = text.len();
        text.push_str(branch);
        name = name_start..text.len();
        text.push(' ');
        git = start..text.len();
    }
    let arrow_start = text.len();
    text.push_str("> ");
    let arrow = arrow_start..text.len();
    Prompt {
        text,
        identity,
        path: path_start..path_end,
        git,
        branch: name,
        arrow,
    }
}

/// Branch label for the prompt: the HEAD ref name, or a short hash when the
/// work tree is detached.
fn git_branch(cwd: &Path) -> Option<String> {
    let dot_git = cwd
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|entry| entry.exists())?;
    let head = if dot_git.is_dir() {
        dot_git.join("HEAD")
    } else {
        let pointer = fs::read_to_string(&dot_git).ok()?;
        let git_dir = pointer.trim().strip_prefix("gitdir:")?.trim();
        if git_dir.is_empty() {
            return None;
        }
        Path::new(git_dir).join("HEAD")
    };
    let head = fs::read_to_string(head).ok()?;
    match head.trim_end().strip_prefix("ref: ") {
        Some(reference) => reference.rsplit('/').next().map(str::to_string),
        None => head.trim_end().get(..7).map(str::to_string),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenKind {
    Command,
    Flag,
    String,
    Number,
    Argument,
}

fn add_syntax_spans(list: &mut AttrsList, family: &'static str, text: &str, offset: usize) {
    for (range, kind) in command_spans(text) {
        let color = match kind {
            TokenKind::Command => theme::accent(),
            TokenKind::Flag => theme::pink(),
            TokenKind::String => theme::green(),
            TokenKind::Number => theme::yellow(),
            TokenKind::Argument => continue,
        };
        list.add_span(
            offset + range.start..offset + range.end,
            &attrs(family, color),
        );
    }
}

fn command_spans(text: &str) -> Vec<(std::ops::Range<usize>, TokenKind)> {
    let mut spans = Vec::new();
    let mut index = 0;
    let mut first = true;
    while index < text.len() {
        let ch = text[index..].chars().next().unwrap();
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        let start = index;
        let mut quote = None;
        let mut quote_start = 0;
        let mut quoted = Vec::new();
        while index < text.len() {
            let ch = text[index..].chars().next().unwrap();
            if let Some(open) = quote {
                index += ch.len_utf8();
                if ch == open {
                    quoted.push((quote_start, index));
                    quote = None;
                }
                continue;
            }
            if ch == '"' || ch == '\'' {
                quote = Some(ch);
                quote_start = index;
                index += ch.len_utf8();
                continue;
            }
            if ch.is_whitespace() {
                break;
            }
            index += ch.len_utf8();
        }
        if quote.is_some() {
            quoted.push((quote_start, text.len()));
        }
        let end = index;
        let kind = if first {
            TokenKind::Command
        } else {
            classify(&text[start..end])
        };
        let mut cursor = start;
        for (from, to) in quoted {
            if cursor < from {
                spans.push((cursor..from, kind));
            }
            spans.push((from..to, TokenKind::String));
            cursor = to;
        }
        if cursor < end {
            spans.push((cursor..end, kind));
        }
        first = false;
    }
    spans
}

fn classify(word: &str) -> TokenKind {
    if word.len() > 1 && word.starts_with('-') {
        return TokenKind::Flag;
    }
    if !word.is_empty() && word.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        return TokenKind::Number;
    }
    TokenKind::Argument
}

fn split_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for value in line.chars() {
        match value {
            '"' | '\'' => {
                if quote == Some(value) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(value);
                } else {
                    current.push(value);
                }
            }
            value if value.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            value => current.push(value),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn is_word_char(value: char) -> bool {
    value.is_alphanumeric() || value == '_'
}

fn grapheme_prev(text: &str, ch: usize) -> Option<usize> {
    let byte = char_to_byte(text, ch);
    if byte == 0 {
        return None;
    }
    let (previous, _) = text[..byte].grapheme_indices(true).last()?;
    Some(byte_to_char(text, previous))
}

fn grapheme_next(text: &str, ch: usize) -> Option<usize> {
    let byte = char_to_byte(text, ch);
    if byte >= text.len() {
        return None;
    }
    let grapheme = text[byte..].graphemes(true).next()?;
    Some(byte_to_char(text, byte + grapheme.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Fonts;

    #[test]
    fn view_selection_copies_across_output_lines() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        shell.push_line("first line".to_string(), LineKind::Output);
        shell.push_line("second line".to_string(), LineKind::Output);
        shell.rebuild_buffer(&mut fonts.system);

        let prompt = shell.buffer.lines.len() - 1;
        // Grid endpoints name cells, so the last column is the last cell that
        // belongs to the selection.
        shell.mouse_drag(
            GridPos {
                line: prompt as i32 - 2,
                column: 6,
            },
            GridPos {
                line: prompt as i32 - 1,
                column: 5,
            },
        );
        assert_eq!(shell.selected_text().as_deref(), Some("line\nsecond"));
        assert_eq!(shell.selection_spans().len(), 2);
    }

    #[test]
    fn the_caret_follows_the_row_the_prompt_lives_on() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 1200.0, 900.0);
        assert!(
            shell.input_row < shell.terminal.rows() - 1,
            "the prompt sits under the banner, not at the bottom of the screen"
        );
        let caret = shell.cursor_rect().expect("a caret to draw");
        assert_eq!(
            (caret.1 / shell.line_height()).round() as usize,
            shell.input_row,
            "the caret belongs on the prompt row"
        );

        // A program that hands the cursor back somewhere else takes the prompt
        // with it, because the prompt is written wherever the cursor stands.
        shell.terminal.feed(b"\x1b[3;1H");
        shell.refresh_prompt(&mut fonts.system);
        assert_eq!(shell.input_row, 2);
        let moved = shell.cursor_rect().expect("a caret to draw");
        assert_eq!((moved.1 / shell.line_height()).round() as usize, 2);
    }

    #[test]
    fn redrawing_the_input_line_leaves_the_rows_below_alone() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 1200.0, 900.0);
        // Two rows of session below the prompt, then the cursor back above
        // them: that is what leaving the alternate screen hands back.
        shell.terminal.feed(b"\r\nrow one\r\nrow two\r\n\x1b[4A");
        shell.refresh_prompt(&mut fonts.system);
        let shown = shell.terminal.screen_text();
        assert!(
            shown.contains("row one") && shown.contains("row two"),
            "the session below the prompt survived its redraw:\n{shown}"
        );
        assert!(
            shown.contains(shell.prompt.text.trim_end()),
            "the prompt is there too:\n{shown}"
        );
    }

    #[test]
    fn scrolling_up_shows_history_and_takes_the_prompt_away() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        for row in 1..=40 {
            shell.push_line(format!("row {row}"), LineKind::Output);
        }
        shell.rebuild_buffer(&mut fonts.system);
        let rows = shell.terminal.rows();
        assert_eq!(
            shell.buffer.lines.len(),
            rows,
            "the view is always one screen"
        );
        let bottom = shell.buffer.lines[rows - 1].text().to_string();
        assert!(
            bottom.contains(&shell.prompt.text),
            "the prompt sits at the bottom: {bottom}"
        );

        let shown = |shell: &Shell| {
            shell
                .buffer
                .lines
                .iter()
                .map(|line| line.text())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let page = rows as f32 * shell.line_height();
        shell.scroll_by(&mut fonts.system, 0.0, 0.0, -page);
        assert!(shell.terminal.scrolled());
        let back = shown(&shell);
        assert!(back.contains("row 26"), "history came into view: {back}");
        assert!(
            !back.contains("row 40"),
            "the newest line moved away: {back}"
        );
        assert!(
            !back.contains(&shell.prompt.text),
            "the prompt stayed behind: {back}"
        );

        shell.scroll_by(&mut fonts.system, 0.0, 0.0, page);
        assert!(!shell.terminal.scrolled());
        assert!(
            shown(&shell).contains(&shell.prompt.text),
            "the prompt came back with the bottom"
        );
    }

    #[test]
    fn tokenizes_commands_flags_strings_and_numbers() {
        let spans = command_spans("git commit -m \"hello world\" 42 file.txt");
        assert_eq!(spans[0], (0..3, TokenKind::Command));
        assert_eq!(spans[1], (4..10, TokenKind::Argument));
        assert_eq!(spans[2], (11..13, TokenKind::Flag));
        assert_eq!(spans[3], (14..27, TokenKind::String));
        assert_eq!(spans[4], (28..30, TokenKind::Number));
        assert_eq!(spans[5], (31..39, TokenKind::Argument));
    }

    #[test]
    fn the_pwsh_aliases_reach_the_same_builtins() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        shell.set_cwd(fs::canonicalize(dir.path()).unwrap());
        for command in ["ni y.c", "md sub", "dir"] {
            shell.input = command.to_string();
            shell.submit(&mut fonts.system);
        }
        assert!(dir.path().join("y.c").is_file(), "ni is touch");
        assert!(dir.path().join("sub").is_dir(), "md is mkdir");
        let listed = shell.terminal.screen_text();
        assert!(listed.contains("y.c"), "dir is ls: {listed}");
        assert!(listed.contains("sub/"), "dir lists folders: {listed}");
        shell.input = "del y.c".to_string();
        shell.submit(&mut fonts.system);
        assert!(!dir.path().join("y.c").exists(), "del is rm");
        let after = shell.terminal.screen_text();
        assert!(
            !after.contains("del:"),
            "del must not report a missing builtin: {after}"
        );
    }

    #[test]
    fn a_path_named_command_resolves_against_the_shells_directory() {
        let cwd = Path::new("X:\\c");
        assert_eq!(
            command_path(cwd, "003/main.exe"),
            PathBuf::from("X:\\c\\003/main.exe")
        );
        assert_eq!(
            command_path(cwd, r"..\004\main.exe"),
            PathBuf::from(r"X:\c\..\004\main.exe")
        );
        assert_eq!(
            command_path(cwd, "x:/c/003/main.exe"),
            PathBuf::from("x:/c/003/main.exe"),
            "an absolute token stays exactly as typed"
        );
        assert_eq!(
            command_path(cwd, "main.exe"),
            PathBuf::from("main.exe"),
            "a bare name keeps the system PATH search"
        );
    }

    #[test]
    fn unmatched_quotes_stay_string_while_typing() {
        let spans = command_spans("echo \"unfinished");
        assert_eq!(
            spans,
            vec![(0..4, TokenKind::Command), (5..16, TokenKind::String)]
        );
    }

    #[test]
    fn quoted_segments_inside_a_word_keep_their_bounds() {
        let spans = command_spans("cp foo\" bar\"baz out");
        assert_eq!(spans[0], (0..2, TokenKind::Command));
        assert_eq!(spans[1], (3..6, TokenKind::Argument));
        assert_eq!(spans[2], (6..12, TokenKind::String));
        assert_eq!(spans[3], (12..15, TokenKind::Argument));
        assert_eq!(spans[4], (16..19, TokenKind::Argument));
    }

    #[test]
    fn empty_input_has_no_spans() {
        assert!(command_spans("").is_empty());
        assert!(command_spans("   ").is_empty());
    }

    fn fake_repo(head: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        fs::create_dir(&git).unwrap();
        fs::write(git.join("HEAD"), head).unwrap();
        dir
    }

    #[test]
    fn prompt_shows_the_branch_inside_a_repo() {
        let repo = fake_repo("ref: refs/heads/main\n");
        let work = repo.path().join("sub");
        fs::create_dir(&work).unwrap();

        let prompt = build_prompt(&work, git_branch(&work).as_deref(), "");
        assert_eq!(&prompt.text[prompt.path.clone()], display_path(&work));
        assert_eq!(&prompt.text[prompt.git.clone()], "  \u{f418}  main ");
        assert_eq!(&prompt.text[prompt.branch.clone()], "main");
        assert_eq!(&prompt.text[prompt.arrow.clone()], "> ");
    }

    #[test]
    fn prompt_stays_plain_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_branch(dir.path()), None);
        let prompt = build_prompt(dir.path(), None, "");
        assert!(prompt.git.is_empty());
        assert_eq!(
            prompt.text,
            format!("{}> ", display_path(dir.path())),
            "the arrow still closes the prompt"
        );
    }

    #[test]
    fn prompt_opens_with_the_windows_identity() {
        let dir = tempfile::tempdir().unwrap();
        let identity = "\u{f17a} qfbsk@清枫不识客";
        let prompt = build_prompt(dir.path(), None, identity);
        assert_eq!(&prompt.text[prompt.identity.clone()], identity);
        assert_eq!(&prompt.text[prompt.path.clone()], display_path(dir.path()));
        assert_eq!(
            prompt.text,
            format!("{identity}  {}> ", display_path(dir.path()))
        );
    }

    #[test]
    fn detached_head_and_linked_work_trees_label_too() {
        let repo = fake_repo("0d1f2e3a4b5c6d7e8f901234567890123456789a\n");
        let prompt = build_prompt(repo.path(), git_branch(repo.path()).as_deref(), "");
        assert_eq!(&prompt.text[prompt.branch.clone()], "0d1f2e3");

        let common = fake_repo("ref: refs/heads/dev\n");
        let work = tempfile::tempdir().unwrap();
        fs::write(
            work.path().join(".git"),
            format!("gitdir: {}\n", common.path().join(".git").display()),
        )
        .unwrap();
        assert_eq!(git_branch(work.path()).as_deref(), Some("dev"));
    }

    #[test]
    fn cd_into_a_repo_relabels_the_live_prompt() {
        let mut fonts = Fonts::load();
        let repo = fake_repo("ref: refs/heads/main\n");
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        shell.set_cwd(fs::canonicalize(repo.path()).unwrap());
        assert_eq!(Some("main".to_string()), shell.branch);
        assert!(shell.prompt.text.contains("\u{f418}  main > "));
    }

    #[test]
    fn cls_clears_the_screen_and_keeps_the_shell() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        shell.push_line("output".to_string(), LineKind::Output);
        shell.input = "cls".to_string();
        assert!(shell.submit(&mut fonts.system).is_none());
        let screen = shell.terminal.screen_text();
        assert!(!screen.contains("output"), "cls wipes the screen: {screen}");
        assert!(screen.contains('>'), "the prompt is still there: {screen}");
    }

    #[test]
    fn restart_reports_an_effect_instead_of_restarting_itself() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        shell.input = "restart".to_string();
        assert!(matches!(
            shell.submit(&mut fonts.system),
            Some(ShellEffect::Restart)
        ));
    }

    #[test]
    fn a_keystroke_reaches_the_waiting_child() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        // `findstr` with no files copies stdin to stdout, so the only way its
        // text can reach the grid is through the child's keyboard. The command
        // line itself never contains the word, so it cannot echo the match.
        shell.insert_text(&mut fonts.system, "cmd /c findstr .");
        assert!(shell.submit(&mut fonts.system).is_none());

        shell.feed_key(TerminalKey::Text("hello".to_string()));
        shell.feed_key(TerminalKey::Enter);
        let waited = std::time::Instant::now();
        while !shell.terminal.screen_text().contains("hello") && waited.elapsed().as_secs() < 5 {
            shell.poll(&mut fonts.system);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        shell.kill_job();
        assert!(
            shell.terminal.screen_text().contains("hello"),
            "the keystroke never reached the child:\n{}",
            shell.terminal.screen_text()
        );
    }

    #[test]
    fn a_running_child_owns_the_screen() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        assert_ne!(
            shell.prompt.len(),
            0,
            "the prompt is what the child replaces"
        );
        // `pause` waits for a keystroke, which is what a real program does when
        // it asks for input. The child's own line is the word on its own row, so
        // it is not confused with the echoed command line above it.
        shell.insert_text(&mut fonts.system, "cmd /c \"echo spoken && pause\"");
        assert!(shell.submit(&mut fonts.system).is_none());

        let child_row = |shell: &Shell| {
            shell
                .terminal
                .screen_text()
                .lines()
                .position(|row| row.trim() == "spoken")
        };
        let mut spoken = false;
        for _ in 0..200 {
            shell.poll(&mut fonts.system);
            if child_row(&shell).is_some() {
                spoken = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(spoken, "the child's output never reached the grid");
        assert!(shell.owns_screen());
        assert_eq!(
            shell.buffer.lines.len(),
            shell.terminal.rows(),
            "kite draws no prompt line under the child"
        );

        // `pause` waits for a key: the keyboard belongs to the child now.
        shell.feed_key(TerminalKey::Text("x".to_string()));
        for _ in 200..400 {
            shell.poll(&mut fonts.system);
            if !shell.owns_screen() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        shell.kill_job();
        assert!(!shell.owns_screen(), "the child never finished");

        let screen = shell.terminal.screen_text();
        let rows: Vec<&str> = screen
            .lines()
            .filter(|row| !row.trim().is_empty())
            .collect();
        let said = rows
            .iter()
            .position(|row| row.trim() == "spoken")
            .expect("the child's text stayed");
        let prompt = rows
            .iter()
            .rposition(|row| row.contains('>'))
            .expect("kite's prompt came back");
        assert!(
            prompt > said,
            "the prompt must sit below the child's screen:\n{screen}"
        );
    }

    #[test]
    fn the_child_writes_below_the_prompt_it_inherited() {
        // One grid, one cursor: the child continues where the prompt left off,
        // the way it does in a terminal, instead of replacing the screen.
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 1200.0, 600.0);
        shell.insert_text(&mut fonts.system, "cmd /c echo spoken");
        shell.submit(&mut fonts.system);
        for _ in 0..200 {
            shell.poll(&mut fonts.system);
            if !shell.owns_screen() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let screen = shell.terminal.screen_text();
        let rows: Vec<&str> = screen
            .lines()
            .map(|row| row.trim_end())
            .filter(|row| !row.is_empty())
            .collect();
        let echo = rows
            .iter()
            .position(|row| row.contains("cmd /c echo spoken"))
            .unwrap_or_else(|| panic!("the command vanished: {screen}"));
        let said = rows
            .iter()
            .position(|row| row.trim() == "spoken")
            .unwrap_or_else(|| panic!("the child wrote nothing below the prompt: {screen}"));
        assert!(said > echo, "output must land below the prompt:\n{screen}");
    }
}
