use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};

use glyphon::FontSystem;
use glyphon::cosmic_text::{
    AttrsList, Buffer, BufferLine, Color, Cursor, Metrics, Scroll, Shaping, Weight, Wrap,
};
use kite_core::display_path;
use unicode_segmentation::UnicodeSegmentation;

use crate::banner;
use crate::text::{attrs, byte_to_char, char_to_byte, force_relayout};
use crate::theme;

const SCROLLBACK_LIMIT: usize = 4000;
const OS_ICON: char = '\u{f17a}';
const GIT_BRANCH_ICON: char = '\u{f418}';
#[cfg(windows)]
pub(crate) const NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Output,
    Error,
    Echo,
}

#[derive(Clone, Debug)]
pub struct ShellLine {
    pub text: String,
    pub kind: LineKind,
    prompt: Option<Prompt>,
    spans: Vec<(std::ops::Range<usize>, Color)>,
}

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPos {
    pub line: usize,
    pub ch: usize,
}

struct Job {
    child: Child,
    rx: Receiver<(bool, String)>,
    stdin: Option<Sender<Vec<u8>>>,
}

pub struct Shell {
    pub cwd: PathBuf,
    pub lines: Vec<ShellLine>,
    pub input: String,
    pub input_cursor: usize,
    pub input_selection: Option<(usize, usize)>,
    pub buffer: Buffer,
    view_selection: Option<(TextPos, TextPos)>,
    follow: bool,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    running: Option<Job>,
    family: &'static str,
    font_size: f32,
    line_height: f32,
    view_width: f32,
    view_height: f32,
    prompt: Prompt,
    branch: Option<String>,
    identity: String,
    /// Set once the running child writes something: a child that speaks is the
    /// one holding the console, so kite keeps out of its way.
    child_spoke: bool,
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
        let buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
        let mut shell = Self {
            cwd,
            lines: Vec::new(),
            input: String::new(),
            input_cursor: 0,
            input_selection: None,
            buffer,
            view_selection: None,
            follow: true,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            running: None,
            family,
            font_size,
            line_height,
            view_width: view_width.max(1.0),
            view_height: view_height.max(1.0),
            prompt,
            branch,
            identity,
            child_spoke: false,
        };
        for line in banner::lines() {
            shell.lines.push(ShellLine {
                text: line.text,
                kind: LineKind::Output,
                prompt: None,
                spans: line.spans,
            });
        }
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
        self.rebuild_buffer(font_system);
    }

    pub fn set_font_size(&mut self, font_system: &mut FontSystem, font_size: f32) {
        if (font_size - self.font_size).abs() < 0.01 {
            return;
        }
        self.font_size = font_size;
        self.line_height = (font_size * theme::LINE_HEIGHT_RATIO).ceil();
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
        self.running.is_some()
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

    pub fn select_all(&mut self, font_system: &mut FontSystem) {
        self.input_selection = Some((0, self.input.chars().count()));
        self.input_cursor = self.input.chars().count();
        self.refresh_prompt(font_system);
    }

    pub fn selected_text(&self) -> Option<String> {
        if let Some((first, second)) = self.view_selection {
            let (start, end) = if (first.line, first.ch) <= (second.line, second.ch) {
                (first, second)
            } else {
                (second, first)
            };
            if start == end {
                return None;
            }
            let last = self.buffer.lines.len().saturating_sub(1);
            let mut out = String::new();
            for line in start.line..=end.line.min(last) {
                let chars: Vec<char> = self
                    .buffer
                    .lines
                    .get(line)
                    .map(|value| value.text().chars().collect())
                    .unwrap_or_default();
                let from = if line == start.line { start.ch } else { 0 }.min(chars.len());
                let to = if line == end.line {
                    end.ch
                } else {
                    chars.len()
                }
                .max(from);
                out.extend(chars[from..to.min(chars.len())].iter());
                if line != end.line {
                    out.push('\n');
                }
            }
            return Some(out);
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

    pub fn position_at(&mut self, font_system: &mut FontSystem, x: f32, y: f32) -> Option<TextPos> {
        self.buffer.shape_until_scroll(font_system, true);
        let cursor = self.buffer.hit(x, y)?;
        let line = cursor.line.min(self.buffer.lines.len().saturating_sub(1));
        let text = self
            .buffer
            .lines
            .get(line)
            .map(|value| value.text())
            .unwrap_or("");
        Some(TextPos {
            line,
            ch: byte_to_char(text, cursor.index),
        })
    }

    pub fn mouse_press(&mut self, font_system: &mut FontSystem, x: f32, y: f32) -> Option<TextPos> {
        let position = self.position_at(font_system, x, y)?;
        self.view_selection = Some((position, position));
        if position.line == self.buffer.lines.len().saturating_sub(1) {
            self.input_cursor = position
                .ch
                .saturating_sub(self.input_line_chars())
                .min(self.input.chars().count());
            self.input_selection = None;
            self.refresh_prompt(font_system);
        }
        Some(position)
    }

    pub fn mouse_drag(&mut self, anchor: TextPos, cursor: TextPos) {
        self.view_selection = Some((anchor, cursor));
        let last = self.buffer.lines.len().saturating_sub(1);
        if anchor.line != last || cursor.line != last {
            self.input_selection = None;
            return;
        }
        let len = self.input.chars().count();
        let anchor = anchor.ch.saturating_sub(self.input_line_chars()).min(len);
        let cursor = cursor.ch.saturating_sub(self.input_line_chars()).min(len);
        self.input_cursor = cursor;
        self.input_selection = if anchor == cursor {
            None
        } else {
            Some((anchor, cursor))
        };
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
        self.view_selection = None;
        self.follow = true;
        let command = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        self.history_index = None;
        self.history_draft.clear();

        if self.running.is_some() {
            self.send_input(&command);
            self.rebuild_buffer(font_system);
            return None;
        }

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

    pub fn poll(&mut self, font_system: &mut FontSystem) -> bool {
        let mut pending = Vec::new();
        let mut finished = None;
        if let Some(job) = &mut self.running {
            while let Ok(message) = job.rx.try_recv() {
                pending.push(message);
            }
            match job.child.try_wait() {
                Ok(Some(status)) => finished = Some(Some(status)),
                Ok(None) => {}
                Err(_) => finished = Some(None),
            }
        }
        let changed = !pending.is_empty() || finished.is_some();
        if !pending.is_empty() {
            self.child_spoke = true;
        }
        for (is_error, text) in pending {
            self.push_line(
                text,
                if is_error {
                    LineKind::Error
                } else {
                    LineKind::Output
                },
            );
        }
        if finished.is_some() {
            let mut remaining = Vec::new();
            if let Some(job) = &mut self.running {
                while let Ok((is_error, text)) = job.rx.try_recv() {
                    remaining.push((is_error, text));
                }
            }
            for (is_error, text) in remaining {
                self.push_line(
                    text,
                    if is_error {
                        LineKind::Error
                    } else {
                        LineKind::Output
                    },
                );
            }
            self.running = None;
            self.child_spoke = false;
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
        }
        if changed {
            self.rebuild_buffer(font_system);
        }
        changed
    }

    pub fn interrupt(&mut self, font_system: &mut FontSystem) -> bool {
        let Some(job) = &mut self.running else {
            return false;
        };
        let killed = kill_tree(job.child.id());
        if !killed {
            let _ = job.child.kill();
        }
        let _ = job.child.try_wait();
        self.running = None;
        self.child_spoke = false;
        self.push_line("^C".to_string(), LineKind::Error);
        self.rebuild_buffer(font_system);
        true
    }

    pub fn kill_job(&mut self) {
        if let Some(job) = &mut self.running {
            kill_tree(job.child.id());
            let _ = job.child.kill();
            let _ = job.child.try_wait();
            self.running = None;
            self.child_spoke = false;
        }
    }

    pub fn print_error(&mut self, font_system: &mut FontSystem, message: String) {
        self.push_line(message, LineKind::Error);
        self.rebuild_buffer(font_system);
    }

    pub fn scroll_by(&mut self, font_system: &mut FontSystem, delta: f32) {
        if delta.abs() < f32::EPSILON {
            return;
        }
        let scroll = self.buffer.scroll();
        self.buffer.set_scroll(Scroll {
            line: scroll.line,
            vertical: scroll.vertical + delta,
            horizontal: 0.0,
        });
        self.buffer.shape_until_scroll(font_system, true);
        self.follow = self.at_bottom();
    }

    fn at_bottom(&self) -> bool {
        let last = self.buffer.lines.len().saturating_sub(1);
        let mut lowest = None;
        for run in self.buffer.layout_runs() {
            lowest = Some(run.line_i);
        }
        lowest.is_none_or(|line| line >= last)
    }

    pub fn cursor_rect(&self) -> Option<(f32, f32, f32)> {
        let line = self.buffer.lines.len().checked_sub(1)?;
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

    pub fn selection_spans(&self) -> Vec<(f32, f32, f32, f32)> {
        if let Some((first, second)) = self.view_selection {
            let (start, end) = if (first.line, first.ch) <= (second.line, second.ch) {
                (first, second)
            } else {
                (second, first)
            };
            if start == end {
                return Vec::new();
            }
            let last = self.buffer.lines.len().saturating_sub(1);
            let end = TextPos {
                line: end.line.min(last),
                ch: end.ch,
            };
            let start_cursor = self.text_cursor(start);
            let end_cursor = self.text_cursor(end);
            let mut spans = Vec::new();
            for run in self.buffer.layout_runs() {
                if run.line_i < start.line || run.line_i > end.line {
                    continue;
                }
                for (x, width) in run.highlight(start_cursor, end_cursor) {
                    spans.push((x, run.line_top, width, run.line_height));
                }
                if run.glyphs.is_empty() && run.line_i > start.line && run.line_i < end.line {
                    spans.push((0.0, run.line_top, self.font_size * 0.5, run.line_height));
                }
            }
            return spans;
        }

        let Some((start, end)) = self.input_selection else {
            return Vec::new();
        };
        let line = self.buffer.lines.len().saturating_sub(1);
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

    fn text_cursor(&self, pos: TextPos) -> Cursor {
        let text = self
            .buffer
            .lines
            .get(pos.line)
            .map(|value| value.text())
            .unwrap_or("");
        Cursor::new(pos.line, char_to_byte(text, pos.ch))
    }

    pub fn cursor_line_column(&self) -> (usize, usize) {
        (
            self.lines.len() + 1,
            self.input_line_chars() + self.input_cursor + 1,
        )
    }

    fn refresh_prompt(&mut self, font_system: &mut FontSystem) {
        self.follow = true;
        self.view_selection = None;
        self.rebuild_last_line(font_system);
        self.buffer
            .shape_until_cursor(font_system, self.prompt_cursor(), true);
    }

    fn prompt_cursor(&self) -> Cursor {
        let line = self.buffer.lines.len().saturating_sub(1);
        Cursor::new(
            line,
            self.input_line_bytes() + char_to_byte(&self.input, self.input_cursor),
        )
    }

    fn rebuild_last_line(&mut self, font_system: &mut FontSystem) {
        let (text, list) = self.prompt_line();
        let Some(line) = self.buffer.lines.last_mut() else {
            self.rebuild_buffer(font_system);
            return;
        };
        line.set_text(text, glyphon::cosmic_text::LineEnding::Lf, list);
        force_relayout(&mut self.buffer);
    }

    fn rebuild_buffer(&mut self, font_system: &mut FontSystem) {
        let previous = self.buffer.scroll();
        let metrics = Metrics::new(self.font_size, self.line_height);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_tab_width(4);
        buffer.set_size(Some(self.view_width), Some(self.view_height));

        for line in &self.lines {
            let list = match &line.prompt {
                Some(prompt) => {
                    let base = attrs(
                        self.family,
                        theme::prompt().map_or(theme::text(), |input| input.input),
                    );
                    let mut list = AttrsList::new(&base);
                    prompt.paint(&mut list, self.family);
                    add_syntax_spans(
                        &mut list,
                        self.family,
                        &line.text[prompt.len()..],
                        prompt.len(),
                    );
                    list
                }
                None => {
                    let color = match line.kind {
                        LineKind::Error => theme::red(),
                        LineKind::Echo => theme::text_dim(),
                        LineKind::Output => theme::text(),
                    };
                    let base = attrs(self.family, color);
                    let mut list = AttrsList::new(&base);
                    for (range, span_color) in &line.spans {
                        list.add_span(range.clone(), &attrs(self.family, *span_color));
                    }
                    list
                }
            };
            buffer.lines.push(BufferLine::new(
                line.text.clone(),
                glyphon::cosmic_text::LineEnding::Lf,
                list,
                Shaping::Advanced,
            ));
        }

        let (prompt, list) = self.prompt_line();
        buffer.lines.push(BufferLine::new(
            prompt,
            glyphon::cosmic_text::LineEnding::Lf,
            list,
            Shaping::Advanced,
        ));

        self.buffer = buffer;
        if self.follow {
            self.buffer
                .shape_until_cursor(font_system, self.prompt_cursor(), true);
        } else {
            self.buffer.set_scroll(previous);
            self.buffer.shape_until_scroll(font_system, true);
        }
    }

    /// A child that owns the session prints its own prompt, so kite adds
    /// nothing in front of the line and highlights none of what is typed.
    fn owns_session(&self) -> bool {
        self.running.is_some() && self.child_spoke
    }

    fn input_line_bytes(&self) -> usize {
        if self.owns_session() {
            0
        } else {
            self.prompt.len()
        }
    }

    fn input_line_chars(&self) -> usize {
        if self.owns_session() {
            0
        } else {
            self.prompt.chars()
        }
    }

    fn prompt_line(&self) -> (String, AttrsList) {
        let prompt_bytes = self.input_line_bytes();
        let text = if self.owns_session() {
            self.input.clone()
        } else {
            format!("{}{}", self.prompt.text, self.input)
        };
        let base = attrs(
            self.family,
            theme::prompt().map_or(theme::text(), |prompt| prompt.input),
        );
        let mut list = AttrsList::new(&base);
        if !self.owns_session() {
            self.prompt.paint(&mut list, self.family);
            add_syntax_spans(&mut list, self.family, &self.input, prompt_bytes);
        }
        (text, list)
    }

    fn push_line(&mut self, text: String, kind: LineKind) {
        self.lines.push(ShellLine {
            text,
            kind,
            prompt: None,
            spans: Vec::new(),
        });
        self.trim_lines();
    }

    fn push_echo(&mut self, prompt: Prompt, command: &str) {
        let mut text = String::with_capacity(prompt.text.len() + command.len());
        text.push_str(&prompt.text);
        text.push_str(command);
        self.lines.push(ShellLine {
            text,
            kind: LineKind::Echo,
            prompt: Some(prompt),
            spans: Vec::new(),
        });
        self.trim_lines();
    }

    fn trim_lines(&mut self) {
        if self.lines.len() > SCROLLBACK_LIMIT {
            let overflow = self.lines.len() - SCROLLBACK_LIMIT;
            self.lines.drain(0..overflow);
            self.view_selection = None;
        }
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
                    "  ls [dir]        list a directory",
                    "  cp <src> <dst>  copy a file",
                    "  mv <src> <dst>  move or rename",
                    "  rm <path...>    move to the Recycle Bin",
                    "  mkdir <dir>     create directories",
                    "  touch <file>    create an empty file",
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
                self.lines.clear();
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
            "ls" => {
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
                    Err(error) => self.push_line(format!("ls: {error}"), LineKind::Error),
                }
                true
            }
            "cp" => {
                if rest.len() != 2 {
                    self.push_line("cp: expected <src> <dst>".to_string(), LineKind::Error);
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
                    Err(error) => self.push_line(format!("cp: {error}"), LineKind::Error),
                }
                true
            }
            "mv" => {
                if rest.len() != 2 {
                    self.push_line("mv: expected <src> <dst>".to_string(), LineKind::Error);
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
                    Err(error) => self.push_line(format!("mv: {error}"), LineKind::Error),
                }
                true
            }
            "rm" => {
                if rest.is_empty() {
                    self.push_line(
                        "rm: expected at least one path".to_string(),
                        LineKind::Error,
                    );
                    return true;
                }
                for arg in rest {
                    let path = resolve_path(&self.cwd, arg);
                    match trash::delete(&path) {
                        Ok(()) => {}
                        Err(error) => self
                            .push_line(format!("rm: {}: {error}", path.display()), LineKind::Error),
                    }
                }
                true
            }
            "mkdir" => {
                if rest.is_empty() {
                    self.push_line("mkdir: expected a directory".to_string(), LineKind::Error);
                }
                for arg in rest {
                    let path = resolve_path(&self.cwd, arg);
                    if let Err(error) = fs::create_dir_all(&path) {
                        self.push_line(format!("mkdir: {error}"), LineKind::Error);
                    }
                }
                true
            }
            "touch" => {
                for arg in rest {
                    let path = resolve_path(&self.cwd, arg);
                    if let Err(error) = fs::OpenOptions::new().create(true).append(true).open(&path)
                    {
                        self.push_line(format!("touch: {error}"), LineKind::Error);
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

    fn spawn_external(&mut self, args: &[String]) {
        let mut command = Command::new(&args[0]);
        command
            .args(&args[1..])
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(NO_WINDOW);
        }
        match command.spawn() {
            Ok(mut child) => {
                let (tx, rx) = mpsc::channel();
                if let Some(stdout) = child.stdout.take() {
                    spawn_reader(stdout, false, tx.clone());
                }
                if let Some(stderr) = child.stderr.take() {
                    spawn_reader(stderr, true, tx);
                }
                let stdin = child.stdin.take().map(spawn_writer);
                self.child_spoke = false;
                self.running = Some(Job { child, rx, stdin });
            }
            Err(error) => {
                self.push_line(format!("{}: {error}", args[0]), LineKind::Error);
            }
        }
    }

    /// Enter while a job runs feeds its stdin instead of starting a command.
    fn send_input(&mut self, line: &str) {
        let mut bytes = encode_console(line);
        bytes.push(b'\n');
        self.push_line(line.to_string(), LineKind::Output);
        if let Some(stdin) = self.running.as_ref().and_then(|job| job.stdin.as_ref()) {
            let _ = stdin.send(bytes);
        }
    }
}

fn decode_output(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    #[cfg(windows)]
    if let Some(text) = decode_oem(bytes) {
        return text;
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(windows)]
fn decode_oem(bytes: &[u8]) -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetOEMCP() -> u32;
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            input: *const u8,
            input_len: i32,
            output: *mut u16,
            output_len: i32,
        ) -> i32;
    }
    let input_len = i32::try_from(bytes.len()).ok()?;
    let code_page = unsafe { GetOEMCP() };
    let needed = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            input_len,
            std::ptr::null_mut(),
            0,
        )
    };
    if needed <= 0 {
        return None;
    }
    let mut wide = vec![0u16; needed as usize];
    let written = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            input_len,
            wide.as_mut_ptr(),
            needed,
        )
    };
    if written <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&wide[..written as usize]))
}

fn spawn_writer(mut writer: ChildStdin) -> Sender<Vec<u8>> {
    let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::channel();
    std::thread::spawn(move || {
        for bytes in rx {
            if writer
                .write_all(&bytes)
                .and_then(|_| writer.flush())
                .is_err()
            {
                break;
            }
        }
    });
    tx
}

/// Bytes a console program expects on its pipe, mirroring `decode_oem`.
#[cfg(windows)]
fn encode_console(text: &str) -> Vec<u8> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetOEMCP() -> u32;
        fn WideCharToMultiByte(
            code_page: u32,
            flags: u32,
            wide: *const u16,
            wide_len: i32,
            multi: *mut u8,
            multi_len: i32,
            default_char: *const u8,
            used_default: *mut bool,
        ) -> i32;
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    let wide_len = i32::try_from(wide.len()).unwrap_or(i32::MAX);
    let code_page = unsafe { GetOEMCP() };
    let needed = unsafe {
        WideCharToMultiByte(
            code_page,
            0,
            wide.as_ptr(),
            wide_len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if needed <= 0 {
        return text.as_bytes().to_vec();
    }
    let mut bytes = vec![0u8; needed as usize];
    let written = unsafe {
        WideCharToMultiByte(
            code_page,
            0,
            wide.as_ptr(),
            wide_len,
            bytes.as_mut_ptr(),
            needed,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if written <= 0 {
        return text.as_bytes().to_vec();
    }
    bytes.truncate(written as usize);
    bytes
}

#[cfg(not(windows))]
fn encode_console(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

fn spawn_reader<R: Read + Send + 'static>(
    reader: R,
    is_error: bool,
    tx: mpsc::Sender<(bool, String)>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) => break,
                Ok(_) => {
                    let text = decode_output(&buffer)
                        .trim_end_matches(['\r', '\n'])
                        .to_string();
                    if tx.send((is_error, text)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
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
        shell.mouse_drag(
            TextPos {
                line: prompt - 2,
                ch: 6,
            },
            TextPos {
                line: prompt - 1,
                ch: 6,
            },
        );
        assert_eq!(shell.selected_text().as_deref(), Some("line\nsecond"));
        assert_eq!(shell.selection_spans().len(), 2);
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
        assert!(shell.lines.is_empty());
        assert_eq!(shell.buffer.lines.len(), 1, "only the prompt is left");
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
    fn a_talking_child_takes_the_prompt_away() {
        let mut fonts = Fonts::load();
        let mut shell = Shell::new(&mut fonts.system, fonts.mono, 16.0, 400.0, 200.0);
        let prompt_bytes = shell.prompt.len();
        shell.input = "cmd /c \"echo spoken && pause\"".to_string();
        assert!(shell.submit(&mut fonts.system).is_none());
        for _ in 0..200 {
            shell.poll(&mut fonts.system);
            if shell.child_spoke {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(shell.child_spoke, "the child never wrote to the pipe");
        assert!(shell.owns_session());
        shell.input = "still typing".to_string();
        shell.refresh_prompt(&mut fonts.system);
        let line = shell.buffer.lines.last().unwrap();
        assert_eq!(line.text(), "still typing", "kite stays out of the way");
        assert_eq!(shell.input_line_bytes(), 0);
        assert_ne!(prompt_bytes, 0);
        shell.kill_job();
        assert!(!shell.owns_session());
    }
}
