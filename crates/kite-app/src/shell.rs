use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};

use glyphon::FontSystem;
use glyphon::cosmic_text::{
    AttrsList, Buffer, BufferLine, Color, Cursor, Metrics, Scroll, Shaping, Wrap,
};
use kite_core::display_path;
use unicode_segmentation::UnicodeSegmentation;

use crate::banner;
use crate::text::{attrs, byte_to_char, char_to_byte, force_relayout};
use crate::theme;

const SCROLLBACK_LIMIT: usize = 4000;

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
    prompt_bytes: usize,
    spans: Vec<(std::ops::Range<usize>, Color)>,
}

pub enum ShellEffect {
    Open(PathBuf),
}

struct Job {
    child: Child,
    rx: Receiver<(bool, String)>,
}

pub struct Shell {
    pub cwd: PathBuf,
    pub lines: Vec<ShellLine>,
    pub input: String,
    pub input_cursor: usize,
    pub input_selection: Option<(usize, usize)>,
    pub buffer: Buffer,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    running: Option<Job>,
    family: &'static str,
    font_size: f32,
    line_height: f32,
    view_width: f32,
    view_height: f32,
    prompt_bytes: usize,
    prompt_len: usize,
}

impl Shell {
    pub fn new(
        font_system: &mut FontSystem,
        family: &'static str,
        font_size: f32,
        view_width: f32,
        view_height: f32,
    ) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("C:\\"));
        let line_height = (font_size * theme::LINE_HEIGHT_RATIO).ceil();
        let buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
        let mut shell = Self {
            cwd,
            lines: Vec::new(),
            input: String::new(),
            input_cursor: 0,
            input_selection: None,
            buffer,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            running: None,
            family,
            font_size,
            line_height,
            view_width: view_width.max(1.0),
            view_height: view_height.max(1.0),
            prompt_bytes: 0,
            prompt_len: 0,
        };
        for line in banner::lines() {
            shell.lines.push(ShellLine {
                text: line.text,
                kind: LineKind::Output,
                prompt_bytes: 0,
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

    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    pub fn shape(&mut self, font_system: &mut FontSystem) {
        self.buffer
            .shape_until_cursor(font_system, self.prompt_cursor(), true);
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn prompt_text(&self) -> String {
        format!("{}> ", display_path(&self.cwd))
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

    pub fn position_at(&mut self, font_system: &mut FontSystem, x: f32, y: f32) -> Option<usize> {
        self.buffer.shape_until_scroll(font_system, true);
        let line = self.buffer.lines.len().checked_sub(1)?;
        let cursor = self.buffer.hit(x, y)?;
        if cursor.line != line {
            return None;
        }
        let byte = cursor.index.saturating_sub(self.prompt_bytes);
        Some(byte_to_char(&self.input, byte.min(self.input.len())))
    }

    pub fn cut_selection(&mut self, font_system: &mut FontSystem) -> Option<String> {
        let text = self.selected_text()?;
        if self.delete_selection() {
            self.refresh_prompt(font_system);
        }
        Some(text)
    }

    pub fn set_input_caret(&mut self, font_system: &mut FontSystem, ch: usize) {
        self.input_cursor = ch.min(self.input.chars().count());
        self.input_selection = None;
        self.refresh_prompt(font_system);
    }

    pub fn set_input_selection(
        &mut self,
        font_system: &mut FontSystem,
        anchor: usize,
        cursor: usize,
    ) {
        let len = self.input.chars().count();
        let anchor = anchor.min(len);
        let cursor = cursor.min(len);
        self.input_cursor = cursor;
        self.input_selection = if anchor == cursor {
            None
        } else {
            Some((anchor, cursor))
        };
        self.refresh_prompt(font_system);
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
        let command = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        self.history_index = None;
        self.history_draft.clear();

        let prompt = self.prompt_text();
        self.push_echo(&prompt, &command);

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
        let pid = job.child.id().to_string();
        let killed = Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if !killed {
            let _ = job.child.kill();
        }
        let _ = job.child.try_wait();
        self.running = None;
        self.push_line("^C".to_string(), LineKind::Error);
        self.rebuild_buffer(font_system);
        true
    }

    pub fn kill_job(&mut self) {
        if let Some(job) = &mut self.running {
            let pid = job.child.id().to_string();
            let _ = Command::new("taskkill")
                .args(["/PID", &pid, "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = job.child.kill();
            let _ = job.child.try_wait();
            self.running = None;
        }
    }

    pub fn print_error(&mut self, font_system: &mut FontSystem, message: String) {
        self.push_line(message, LineKind::Error);
        self.rebuild_buffer(font_system);
    }

    pub fn scroll_by(&mut self, font_system: &mut FontSystem, delta: f32) {
        let scroll = self.buffer.scroll();
        self.buffer.set_scroll(Scroll {
            line: scroll.line,
            vertical: scroll.vertical + delta,
            horizontal: 0.0,
        });
        self.buffer.shape_until_scroll(font_system, true);
    }

    pub fn cursor_rect(&self) -> Option<(f32, f32, f32)> {
        let line = self.buffer.lines.len().checked_sub(1)?;
        let byte = self.prompt_bytes + char_to_byte(&self.input, self.input_cursor);
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
        let Some((start, end)) = self.input_selection else {
            return Vec::new();
        };
        let line = self.buffer.lines.len().saturating_sub(1);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let start_byte = self.prompt_bytes + char_to_byte(&self.input, start);
        let end_byte = self.prompt_bytes + char_to_byte(&self.input, end);
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

    pub fn cursor_line_column(&self) -> (usize, usize) {
        (
            self.lines.len() + 1,
            self.prompt_len + self.input_cursor + 1,
        )
    }

    fn refresh_prompt(&mut self, font_system: &mut FontSystem) {
        self.rebuild_last_line(font_system);
        let last = self.buffer.lines.len().saturating_sub(1);
        self.buffer.set_scroll(Scroll {
            line: last,
            vertical: 0.0,
            horizontal: 0.0,
        });
        self.buffer
            .shape_until_cursor(font_system, self.prompt_cursor(), true);
    }

    fn prompt_cursor(&self) -> Cursor {
        let line = self.buffer.lines.len().saturating_sub(1);
        Cursor::new(
            line,
            self.prompt_bytes + char_to_byte(&self.input, self.input_cursor),
        )
    }

    fn rebuild_last_line(&mut self, font_system: &mut FontSystem) {
        let (text, list, prompt_bytes) = self.prompt_line();
        self.prompt_bytes = prompt_bytes;
        let Some(line) = self.buffer.lines.last_mut() else {
            self.rebuild_buffer(font_system);
            return;
        };
        line.set_text(text, glyphon::cosmic_text::LineEnding::Lf, list);
        force_relayout(&mut self.buffer);
    }

    fn rebuild_buffer(&mut self, font_system: &mut FontSystem) {
        let metrics = Metrics::new(self.font_size, self.line_height);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_tab_width(4);
        buffer.set_size(Some(self.view_width), Some(self.view_height));

        for line in &self.lines {
            if line.kind == LineKind::Echo && line.prompt_bytes > 0 {
                let base = attrs(
                    self.family,
                    theme::prompt().map_or(theme::text(), |prompt| prompt.input),
                );
                let mut list = AttrsList::new(&base);
                match theme::prompt() {
                    Some(prompt) if line.prompt_bytes >= 2 => {
                        let path_end = line.prompt_bytes - 2;
                        list.add_span(0..path_end, &attrs(self.family, prompt.path));
                        list.add_span(
                            path_end..line.prompt_bytes,
                            &attrs(self.family, prompt.arrow),
                        );
                    }
                    Some(_) => {}
                    None => {
                        list.add_span(0..line.prompt_bytes, &attrs(self.family, theme::text_dim()));
                    }
                }
                add_syntax_spans(
                    &mut list,
                    self.family,
                    &line.text[line.prompt_bytes..],
                    line.prompt_bytes,
                );
                buffer.lines.push(BufferLine::new(
                    line.text.clone(),
                    glyphon::cosmic_text::LineEnding::Lf,
                    list,
                    Shaping::Advanced,
                ));
                continue;
            }
            let color = match line.kind {
                LineKind::Error => theme::red(),
                LineKind::Echo => theme::text_dim(),
                LineKind::Output => theme::text(),
            };
            let line_attrs = attrs(self.family, color);
            let mut list = AttrsList::new(&line_attrs);
            for (range, span_color) in &line.spans {
                list.add_span(range.clone(), &attrs(self.family, *span_color));
            }
            buffer.lines.push(BufferLine::new(
                line.text.clone(),
                glyphon::cosmic_text::LineEnding::Lf,
                list,
                Shaping::Advanced,
            ));
        }

        let (prompt, list, prompt_bytes) = self.prompt_line();
        self.prompt_bytes = prompt_bytes;
        buffer.lines.push(BufferLine::new(
            prompt,
            glyphon::cosmic_text::LineEnding::Lf,
            list,
            Shaping::Advanced,
        ));

        self.buffer = buffer;
        let last = self.buffer.lines.len().saturating_sub(1);
        self.buffer.set_scroll(Scroll {
            line: last,
            vertical: 0.0,
            horizontal: 0.0,
        });
        self.buffer
            .shape_until_cursor(font_system, self.prompt_cursor(), true);
    }

    fn prompt_line(&mut self) -> (String, AttrsList, usize) {
        let prompt = self.prompt_text();
        self.prompt_len = prompt.chars().count();
        let prompt_bytes = prompt.len();
        let text = format!("{prompt}{}", self.input);
        let base = attrs(
            self.family,
            theme::prompt().map_or(theme::text(), |prompt| prompt.input),
        );
        let mut list = AttrsList::new(&base);
        if prompt_bytes > 0 {
            if let Some(prompt) = theme::prompt() {
                if prompt_bytes >= 2 {
                    let path_end = prompt_bytes - 2;
                    list.add_span(0..path_end, &attrs(self.family, prompt.path));
                    list.add_span(path_end..prompt_bytes, &attrs(self.family, prompt.arrow));
                }
            } else {
                list.add_span(0..prompt_bytes, &attrs(self.family, theme::text_dim()));
            }
        }
        add_syntax_spans(&mut list, self.family, &self.input, prompt_bytes);
        (text, list, prompt_bytes)
    }

    fn push_line(&mut self, text: String, kind: LineKind) {
        self.lines.push(ShellLine {
            text,
            kind,
            prompt_bytes: 0,
            spans: Vec::new(),
        });
        self.trim_lines();
    }

    fn push_echo(&mut self, prompt: &str, command: &str) {
        let mut text = String::with_capacity(prompt.len() + command.len());
        text.push_str(prompt);
        let prompt_bytes = text.len();
        text.push_str(command);
        self.lines.push(ShellLine {
            text,
            kind: LineKind::Echo,
            prompt_bytes,
            spans: Vec::new(),
        });
        self.trim_lines();
    }

    fn trim_lines(&mut self) {
        if self.lines.len() > SCROLLBACK_LIMIT {
            let overflow = self.lines.len() - SCROLLBACK_LIMIT;
            self.lines.drain(0..overflow);
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
                    "  clear           clear the scrollback",
                    "",
                    "Anything else runs as an external program.",
                ] {
                    self.push_line(line.to_string(), LineKind::Output);
                }
                true
            }
            "clear" => {
                self.lines.clear();
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
                    Ok(path) if path.is_dir() => self.cwd = path,
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
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
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
                self.running = Some(Job { child, rx });
            }
            Err(error) => {
                self.push_line(format!("{}: {error}", args[0]), LineKind::Error);
            }
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
}
