use glyphon::FontSystem;
use glyphon::cosmic_text::{Buffer, Cursor, Metrics, Scroll, Wrap};
use kite_core::{Document, Position, Selection, Unencodable};
use unicode_segmentation::UnicodeSegmentation;

use crate::text::{attrs, buffer_line, byte_to_char, char_to_byte, force_relayout};
use crate::theme;

pub struct Editor {
    pub document: Document,
    pub buffer: Buffer,
    pub selection: Selection,
    pub desired_x: Option<f32>,
    family: &'static str,
    font_size: f32,
    line_height: f32,
    view_width: f32,
    view_height: f32,
}

impl Editor {
    pub fn new(
        document: Document,
        font_system: &mut FontSystem,
        family: &'static str,
        view_width: f32,
        view_height: f32,
        font_size: f32,
    ) -> Self {
        let line_height = (font_size * theme::LINE_HEIGHT_RATIO).ceil();
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_tab_width(4);
        buffer.set_size(Some(view_width.max(1.0)), Some(view_height.max(1.0)));
        let line_attrs = attrs(family, theme::text());
        for index in 0..document.line_count() {
            let text = document.line_text(index);
            buffer.lines.push(buffer_line(&text, &line_attrs));
        }
        let mut editor = Self {
            document,
            buffer,
            selection: Selection::caret(Position::default()),
            desired_x: None,
            family,
            font_size,
            line_height,
            view_width,
            view_height,
        };
        force_relayout(&mut editor.buffer);
        editor.buffer.shape_until_scroll(font_system, true);
        editor
    }

    pub fn set_viewport(&mut self, font_system: &mut FontSystem, width: f32, height: f32) {
        let width = width.max(1.0);
        let height = height.max(1.0);
        if (width - self.view_width).abs() < 0.01 && (height - self.view_height).abs() < 0.01 {
            return;
        }
        self.view_width = width;
        self.view_height = height;
        self.buffer.set_size(Some(width), Some(height));
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), true);
    }

    pub fn set_font_size(&mut self, font_system: &mut FontSystem, font_size: f32) {
        if (font_size - self.font_size).abs() < 0.01 {
            return;
        }
        self.font_size = font_size;
        self.line_height = (font_size * theme::LINE_HEIGHT_RATIO).ceil();
        self.buffer
            .set_metrics(Metrics::new(font_size, self.line_height));
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), true);
    }

    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    pub fn shape(&mut self, font_system: &mut FontSystem) {
        self.buffer.shape_until_scroll(font_system, true);
    }

    pub fn ensure_cursor_visible(&mut self, font_system: &mut FontSystem) {
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), true);
    }

    pub fn insert_text(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
    ) -> Result<(), Unencodable> {
        self.selection = self.document.apply_edit(self.selection, text)?;
        self.after_change(font_system);
        Ok(())
    }

    pub fn backspace(&mut self, font_system: &mut FontSystem) -> Result<(), Unencodable> {
        if !self.selection.is_empty() {
            self.selection = self.document.apply_edit(self.selection, "")?;
        } else if let Some(previous) = self.previous_position(self.selection.cursor) {
            let selection = Selection::new(previous, self.selection.cursor);
            self.selection = self.document.apply_edit(selection, "")?;
        }
        self.after_change(font_system);
        Ok(())
    }

    pub fn delete_selection(&mut self, font_system: &mut FontSystem) -> Result<(), Unencodable> {
        if self.selection.is_empty() {
            return Ok(());
        }
        self.selection = self.document.apply_edit(self.selection, "")?;
        self.after_change(font_system);
        Ok(())
    }

    pub fn delete_forward(&mut self, font_system: &mut FontSystem) -> Result<(), Unencodable> {
        if !self.selection.is_empty() {
            self.selection = self.document.apply_edit(self.selection, "")?;
        } else if let Some(next) = self.next_position(self.selection.cursor) {
            let selection = Selection::new(self.selection.cursor, next);
            self.selection = self.document.apply_edit(selection, "")?;
        }
        self.after_change(font_system);
        Ok(())
    }

    pub fn undo(&mut self, font_system: &mut FontSystem) -> bool {
        if let Some(selection) = self.document.undo() {
            self.selection = selection;
            self.after_change(font_system);
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self, font_system: &mut FontSystem) -> bool {
        if let Some(selection) = self.document.redo() {
            self.selection = selection;
            self.after_change(font_system);
            true
        } else {
            false
        }
    }

    pub fn save(&mut self) -> Result<(), String> {
        self.document.save().map_err(|error| error.to_string())
    }

    pub fn select_all(&mut self, font_system: &mut FontSystem) {
        self.selection = self.document.select_all();
        self.desired_x = None;
        self.ensure_cursor_visible(font_system);
    }

    pub fn selected_text(&self) -> Option<String> {
        if self.selection.is_empty() {
            return None;
        }
        let (start, end) = self.selection.ordered();
        let mut out = String::new();
        for line in start.line..=end.line {
            let text = self.document.line_text(line);
            let chars: Vec<char> = text.chars().collect();
            let from = if line == start.line { start.ch } else { 0 };
            let to = if line == end.line {
                end.ch.min(chars.len())
            } else {
                chars.len()
            };
            let from = from.min(to);
            out.extend(&chars[from..to]);
            if line != end.line {
                out.push('\n');
            }
        }
        Some(out)
    }

    pub fn move_left(&mut self, font_system: &mut FontSystem) {
        let target = if !self.selection.is_empty() {
            self.selection.ordered().0
        } else {
            self.previous_position(self.selection.cursor)
                .unwrap_or(self.selection.cursor)
        };
        self.set_caret(font_system, target);
    }

    pub fn move_right(&mut self, font_system: &mut FontSystem) {
        let target = if !self.selection.is_empty() {
            self.selection.ordered().1
        } else {
            self.next_position(self.selection.cursor)
                .unwrap_or(self.selection.cursor)
        };
        self.set_caret(font_system, target);
    }

    pub fn move_word_left(&mut self, font_system: &mut FontSystem) {
        let target = if !self.selection.is_empty() {
            self.selection.ordered().0
        } else {
            self.previous_word_position(self.selection.cursor)
        };
        self.set_caret(font_system, target);
    }

    pub fn move_word_right(&mut self, font_system: &mut FontSystem) {
        let target = if !self.selection.is_empty() {
            self.selection.ordered().1
        } else {
            self.next_word_position(self.selection.cursor)
        };
        self.set_caret(font_system, target);
    }

    pub fn move_line_start(&mut self, font_system: &mut FontSystem) {
        self.set_caret(font_system, Position::new(self.selection.cursor.line, 0));
    }

    pub fn move_line_end(&mut self, font_system: &mut FontSystem) {
        let line = self.selection.cursor.line;
        let ch = self.document.line_char_len(line);
        self.set_caret(font_system, Position::new(line, ch));
    }

    pub fn move_vertical(&mut self, font_system: &mut FontSystem, down: bool) {
        self.buffer.shape_until_scroll(font_system, true);
        let cursor = self.cursor_cosmic();
        let Some((current_x, _)) = self.buffer.cursor_position(&cursor) else {
            return;
        };
        let x = self.desired_x.unwrap_or(current_x);
        let Some(layout_cursor) = self.buffer.layout_cursor(font_system, cursor) else {
            return;
        };
        let paragraph = layout_cursor.line;
        let layout_len = self
            .buffer
            .line_layout(font_system, paragraph)
            .map_or(0, |layout| layout.len());

        let (target_line, target_layout) = if down {
            if layout_cursor.layout + 1 < layout_len {
                (paragraph, layout_cursor.layout + 1)
            } else if paragraph + 1 < self.buffer.lines.len() {
                (paragraph + 1, 0)
            } else {
                return;
            }
        } else if layout_cursor.layout > 0 {
            (paragraph, layout_cursor.layout - 1)
        } else if paragraph > 0 {
            let previous = paragraph - 1;
            let previous_len = self
                .buffer
                .line_layout(font_system, previous)
                .map_or(0, |layout| layout.len());
            if previous_len == 0 {
                return;
            }
            (previous, previous_len - 1)
        } else {
            return;
        };

        let mut cluster = None;
        let mut line_end = 0;
        if let Some(layout) = self.buffer.line_layout(font_system, target_line) {
            if let Some(row) = layout.get(target_layout) {
                for glyph in &row.glyphs {
                    line_end = glyph.end;
                    if x >= glyph.x && x <= glyph.x + glyph.w {
                        cluster = Some((
                            glyph.start,
                            glyph.end,
                            glyph.x,
                            glyph.w,
                            glyph.level.is_rtl(),
                        ));
                        break;
                    }
                }
            }
        }
        let ch = {
            let text = self.buffer.lines[target_line].text();
            let index = match cluster {
                Some((start, end, left, width, rtl)) => {
                    index_in_cluster(text, start, end, left, width, x, rtl)
                }
                None => line_end.min(text.len()),
            };
            byte_to_char(text, index)
        };
        self.selection = Selection::caret(Position::new(target_line, ch));
        self.desired_x = Some(x);
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), true);
    }

    pub fn scroll_by(&mut self, font_system: &mut FontSystem, delta: f32) {
        if delta.abs() < f32::EPSILON {
            return;
        }
        let scroll = self.buffer.scroll();
        self.buffer.set_scroll(Scroll {
            line: scroll.line,
            vertical: scroll.vertical + delta,
            horizontal: scroll.horizontal,
        });
        self.buffer.shape_until_scroll(font_system, true);
    }

    pub fn position_at(
        &mut self,
        font_system: &mut FontSystem,
        x: f32,
        y: f32,
    ) -> Option<Position> {
        self.buffer.shape_until_scroll(font_system, true);
        let cursor = self.buffer.hit(x, y)?;
        Some(self.cosmic_position(cursor))
    }

    pub fn set_selection(&mut self, font_system: &mut FontSystem, selection: Selection) {
        self.selection = selection;
        self.desired_x = None;
        self.ensure_cursor_visible(font_system);
    }

    pub fn selection_spans(&self) -> Vec<(f32, f32, f32, f32)> {
        if self.selection.is_empty() {
            return Vec::new();
        }
        let (start, end) = self.selection.ordered();
        let start_cursor = self.position_cosmic(start);
        let end_cursor = self.position_cosmic(end);
        let mut spans = Vec::new();
        for run in self.buffer.layout_runs() {
            for (x, width) in run.highlight(start_cursor, end_cursor) {
                spans.push((x, run.line_top, width, run.line_height));
            }
            if run.glyphs.is_empty() {
                let whole_line = start.line < run.line_i && end.line > run.line_i;
                let at_end = end.line == run.line_i && end.ch == 0 && start.line < run.line_i;
                if whole_line || at_end {
                    spans.push((0.0, run.line_top, self.font_size * 0.5, run.line_height));
                }
            }
        }
        spans
    }

    pub fn cursor_rect(&self) -> Option<(f32, f32, f32)> {
        let cursor = self.cursor_cosmic();
        let mut found = None;
        for run in self.buffer.layout_runs() {
            if run.line_i != cursor.line {
                continue;
            }
            if let Some(x) = run.cursor_position(&cursor) {
                found = Some((x, run.line_top, run.line_height));
                break;
            }
        }
        found
    }

    pub fn display_title(&self) -> String {
        self.document
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.document.path().to_string_lossy().into_owned())
    }

    pub fn cursor_position(&self) -> Position {
        self.selection.cursor
    }

    pub fn encoding_name(&self) -> &'static str {
        self.document.encoding().name()
    }

    pub fn is_modified(&self) -> bool {
        self.document.is_modified()
    }

    fn after_change(&mut self, font_system: &mut FontSystem) {
        if let Some(change) = self.document.take_change() {
            if change.start_line < self.buffer.scroll().line {
                let current = self.buffer.scroll().line as isize;
                let delta = change.new_count as isize - change.old_count as isize;
                let line = (current + delta).max(0) as usize;
                let scroll = self.buffer.scroll();
                self.buffer.set_scroll(Scroll { line, ..scroll });
            }
            let line_attrs = attrs(self.family, theme::text());
            let new_lines = (0..change.new_count)
                .map(|index| {
                    let text = self.document.line_text(change.start_line + index);
                    buffer_line(&text, &line_attrs)
                })
                .collect::<Vec<_>>();
            let range = change.start_line..change.start_line + change.old_count;
            if range.end <= self.buffer.lines.len() {
                self.buffer.lines.splice(range, new_lines);
            } else {
                self.rebuild_buffer(font_system);
                return;
            }
            force_relayout(&mut self.buffer);
        }
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), true);
    }

    fn rebuild_buffer(&mut self, font_system: &mut FontSystem) {
        let line_attrs = attrs(self.family, theme::text());
        let mut buffer = Buffer::new_empty(Metrics::new(self.font_size, self.line_height));
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_tab_width(4);
        buffer.set_size(Some(self.view_width), Some(self.view_height));
        for index in 0..self.document.line_count() {
            let text = self.document.line_text(index);
            buffer.lines.push(buffer_line(&text, &line_attrs));
        }
        self.buffer = buffer;
        force_relayout(&mut self.buffer);
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), true);
    }

    fn set_caret(&mut self, font_system: &mut FontSystem, position: Position) {
        self.selection = Selection::caret(position);
        self.desired_x = None;
        self.ensure_cursor_visible(font_system);
    }

    fn previous_position(&self, position: Position) -> Option<Position> {
        if position.ch > 0 {
            let text = self.document.line_text(position.line);
            let ch = grapheme_prev(&text, position.ch)?;
            Some(Position::new(position.line, ch))
        } else if position.line > 0 {
            let line = position.line - 1;
            Some(Position::new(line, self.document.line_char_len(line)))
        } else {
            None
        }
    }

    fn next_position(&self, position: Position) -> Option<Position> {
        let len = self.document.line_char_len(position.line);
        if position.ch < len {
            let text = self.document.line_text(position.line);
            let ch = grapheme_next(&text, position.ch)?;
            Some(Position::new(position.line, ch))
        } else if position.line + 1 < self.document.line_count() {
            Some(Position::new(position.line + 1, 0))
        } else {
            None
        }
    }

    fn previous_word_position(&self, position: Position) -> Position {
        if position.ch == 0 {
            if position.line == 0 {
                return position;
            }
            let line = position.line - 1;
            return Position::new(line, self.document.line_char_len(line));
        }
        let text = self.document.line_text(position.line);
        let chars: Vec<char> = text.chars().collect();
        let mut ch = position.ch.min(chars.len());
        while ch > 0 && !is_word_char(chars[ch - 1]) {
            ch -= 1;
        }
        while ch > 0 && is_word_char(chars[ch - 1]) {
            ch -= 1;
        }
        Position::new(position.line, ch)
    }

    fn next_word_position(&self, position: Position) -> Position {
        let text = self.document.line_text(position.line);
        let chars: Vec<char> = text.chars().collect();
        let mut ch = position.ch.min(chars.len());
        if ch >= chars.len() {
            if position.line + 1 < self.document.line_count() {
                return Position::new(position.line + 1, 0);
            }
            return position;
        }
        if is_word_char(chars[ch]) {
            while ch < chars.len() && is_word_char(chars[ch]) {
                ch += 1;
            }
        } else {
            while ch < chars.len() && !is_word_char(chars[ch]) {
                ch += 1;
            }
        }
        Position::new(position.line, ch)
    }

    fn cursor_cosmic(&self) -> Cursor {
        self.position_cosmic(self.selection.cursor)
    }

    fn position_cosmic(&self, position: Position) -> Cursor {
        let line = position.line.min(self.buffer.lines.len().saturating_sub(1));
        let text = self
            .buffer
            .lines
            .get(line)
            .map(|line| line.text())
            .unwrap_or("");
        Cursor::new(line, char_to_byte(text, position.ch))
    }

    fn cosmic_position(&self, cursor: Cursor) -> Position {
        let line = cursor.line.min(self.buffer.lines.len().saturating_sub(1));
        let text = self
            .buffer
            .lines
            .get(line)
            .map(|line| line.text())
            .unwrap_or("");
        Position::new(line, byte_to_char(text, cursor.index))
    }
}

fn is_word_char(value: char) -> bool {
    value.is_alphanumeric() || value == '_'
}

fn index_in_cluster(
    text: &str,
    start: usize,
    end: usize,
    glyph_x: f32,
    glyph_w: f32,
    x: f32,
    rtl: bool,
) -> usize {
    let end = end.min(text.len());
    let start = start.min(end);
    let cluster = &text[start..end];
    let total = cluster.grapheme_indices(true).count().max(1);
    let width = glyph_w / total as f32;
    let mut left = glyph_x;
    for (offset, grapheme) in cluster.grapheme_indices(true) {
        if x >= left && x <= left + width {
            let right_half = x >= left + width / 2.0;
            if right_half != rtl {
                return start + offset + grapheme.len();
            }
            return start + offset;
        }
        left += width;
    }
    let right_half = x >= glyph_x + glyph_w / 2.0;
    if right_half != rtl { end } else { start }
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
