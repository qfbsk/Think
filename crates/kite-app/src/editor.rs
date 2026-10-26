use std::ops::Range;
use std::sync::Arc;

use glyphon::FontSystem;
use glyphon::cosmic_text::{AttrsList, Buffer, Color, Cursor, Metrics, Scroll, Wrap};
use kite_core::{Change, Document, Position, Selection, Unencodable};
use ropey::Rope;
use tree_house::tree_sitter::{InputEdit, Point};
use tree_house::{Language, Syntax};
use unicode_segmentation::UnicodeSegmentation;

use crate::syntax::{self, Engine, Run};
use crate::text::{bold_attrs, buffer_line, byte_to_char, char_to_byte, force_relayout};
use crate::theme;

pub struct Editor {
    pub document: Document,
    pub buffer: Buffer,
    pub selection: Selection,
    pub desired_x: Option<f32>,
    /// Decoded mirror of the document, only for the highlighter; never saved.
    rope: Option<Rope>,
    language: Option<Language>,
    tree: Option<Syntax>,
    colored: Option<(u64, usize, usize, Position)>,
    /// What the last colouring pass painted on either side of the caret, which
    /// is where the typing powder takes its ink from.
    ink_before_caret: Option<Color>,
    ink_after_caret: Option<Color>,
    engine: Arc<Engine>,
    numbers: Buffer,
    numbers_key: Option<NumbersKey>,
    /// The rows holding a shape and layout cache, so the ones the view left
    /// behind can be let go without walking the file.
    shaped: Option<(usize, usize)>,
    cell_width: f32,
    family: &'static str,
    font_size: f32,
    line_height: f32,
    view_width: f32,
    view_height: f32,
}

#[derive(Clone, Copy, PartialEq)]
struct NumbersKey {
    scroll_line: usize,
    scroll_vertical: f32,
    line_count: usize,
    font_size: f32,
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
        engine: Arc<Engine>,
    ) -> Self {
        let line_height = (font_size * theme::LINE_HEIGHT_RATIO).ceil();
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_tab_width(4);
        buffer.set_size(Some(view_width.max(1.0)), Some(view_height.max(1.0)));
        let line_attrs = bold_attrs(family, theme::text());
        for index in 0..document.line_count() {
            let text = document.line_text(index);
            buffer.lines.push(buffer_line(&text, &line_attrs));
        }
        let language = syntax::language_for_path(document.path());
        let rope = if language.is_some() {
            mirror(&document)
        } else {
            None
        };
        let tree = match (language, rope.as_ref()) {
            (Some(language), Some(rope)) => engine.parse(language, rope),
            _ => None,
        };
        let mut editor = Self {
            document,
            buffer,
            selection: Selection::caret(Position::default()),
            desired_x: None,
            rope,
            language,
            tree,
            colored: None,
            ink_before_caret: None,
            ink_after_caret: None,
            engine,
            numbers: Buffer::new_empty(metrics),
            numbers_key: None,
            shaped: None,
            cell_width: measure_cell(font_system, family, font_size, line_height),
            family,
            font_size,
            line_height,
            view_width,
            view_height,
        };
        editor.shape_view(font_system);
        editor
    }

    /// Colour the rows the viewport can see with one query pass over that range.
    /// Only glyph attributes change; the document is never touched. Returns
    /// whether the view was redrawn, so the caller knows the rows are already
    /// shaped and need not walk the document a second time.
    pub fn highlight(&mut self, font_system: &mut FontSystem) -> bool {
        let revision = self.document.revision();
        let rows = (self.view_height / self.line_height).ceil() as usize + 8;
        let count = self.document.line_count();
        let first = self.buffer.scroll().line.min(count);
        let last = (first + rows).min(count);
        let key = (revision, first, last, self.selection.cursor);
        if self.colored == Some(key) {
            return false;
        }
        let runs = match (self.rope.as_ref(), self.tree.as_ref()) {
            (Some(rope), Some(tree)) => {
                let lines = rope.len_lines();
                let from = rope.line_to_byte(first.min(lines));
                let to = rope.line_to_byte(last.min(lines)).min(rope.len_bytes());
                let mut runs = self.engine.runs(tree, rope, from, to);
                runs.extend(self.pair_runs(tree, rope));
                runs
            }
            _ => Vec::new(),
        };
        let caret = self.selection.cursor;
        let ink = |index: usize| -> Option<Color> {
            let rope = self.rope.as_ref()?;
            if caret.line >= rope.len_lines() {
                return None;
            }
            let text = self.document.line_text(caret.line);
            if index >= text.chars().count() {
                return None;
            }
            let byte = rope.line_to_byte(caret.line) + byte_of_char(&text, index);
            Some(
                runs.iter()
                    .find(|run| run.start <= byte && byte < run.end)
                    .map(|run| run.color)
                    .unwrap_or_else(theme::text),
            )
        };
        let before = caret.ch.checked_sub(1).and_then(|index| ink(index));
        let after = ink(caret.ch);
        self.ink_before_caret = before;
        self.ink_after_caret = after;
        let base = bold_attrs(self.family, theme::text());
        for index in first..last {
            let text = self.document.line_text(index);
            let start = self
                .rope
                .as_ref()
                .filter(|rope| index < rope.len_lines())
                .map(|rope| rope.line_to_byte(index))
                .unwrap_or(0);
            let mut list = AttrsList::new(&base);
            for run in &runs {
                let from = run.start.saturating_sub(start).min(text.len());
                let to = run.end.saturating_sub(start).min(text.len());
                if from < to {
                    list.add_span(from..to, &bold_attrs(self.family, run.color));
                }
            }
            if let Some(line) = self.buffer.lines.get_mut(index) {
                line.set_attrs_list(list);
            }
        }
        self.colored = Some(key);
        self.shape_view(font_system);
        true
    }

    /// The pair the caret sits to the right of, as two runs in the theme's match colour.
    fn pair_runs(&self, tree: &Syntax, rope: &Rope) -> Vec<Run> {
        if !self.selection.is_empty() {
            return Vec::new();
        }
        let caret = self.selection.cursor;
        let byte = rope.line_to_byte(caret.line)
            + byte_of_char(&self.document.line_text(caret.line), caret.ch);
        let pair = match syntax::bracket_pair(tree, byte) {
            Some(pair) => pair,
            None => return Vec::new(),
        };
        [pair.0, pair.1]
            .into_iter()
            .map(|(start, end)| Run {
                start,
                end,
                color: self.engine.pair_color(),
            })
            .collect()
    }

    /// Keep the highlighter's rope in step with the document and hand tree-sitter the
    /// same edit, so only the touched region re-parses.
    fn sync(&mut self, change: Change) {
        let lines = match self.rope.as_ref() {
            Some(rope) => rope.len_lines(),
            None => {
                // An undo can bring a document back under the highlighter's budget.
                if self.language.is_some() && syntax::enabled(self.document.len_bytes()) {
                    self.rebuild_tree();
                }
                return;
            }
        };
        if change.start_line + change.old_count > lines {
            self.rebuild_tree();
            return;
        }
        let from = self.rope.as_ref().unwrap().line_to_byte(change.start_line);
        let to = self
            .rope
            .as_ref()
            .unwrap()
            .line_to_byte(change.start_line + change.old_count);
        let joined: Vec<String> = (0..change.new_count)
            .map(|index| self.document.line_text(change.start_line + index))
            .collect();
        let mut text = joined.join("\n");
        if change.start_line + change.old_count < lines {
            text.push('\n');
        }
        {
            let rope = self.rope.as_mut().expect("rope");
            rope.remove(from..to);
            rope.insert(from, &text);
        }
        self.colored = None;
        let edit = InputEdit {
            start_byte: from as u32,
            old_end_byte: to as u32,
            new_end_byte: (from + text.len()) as u32,
            start_point: Point::ZERO,
            old_end_point: Point::ZERO,
            new_end_point: Point::ZERO,
        };
        // A timed-out update still leaves a usable partial tree behind, and rebuilding
        // would pay for a full re-parse; the next edit simply tries again.
        if let (Some(tree), Some(rope)) = (self.tree.as_mut(), self.rope.as_ref()) {
            let _ = tree.update(rope.slice(..), syntax::TIMEOUT, &[edit], &*self.engine);
        } else {
            self.rebuild_tree();
        }
    }

    fn rebuild_tree(&mut self) {
        self.rope = if self.language.is_some() {
            mirror(&self.document)
        } else {
            None
        };
        self.tree = match (self.language, self.rope.as_ref()) {
            (Some(language), Some(rope)) => self.engine.parse(language, rope),
            _ => None,
        };
        self.colored = None;
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
            .shape_until_cursor(font_system, self.cursor_cosmic(), false);
    }

    pub fn set_font_size(&mut self, font_system: &mut FontSystem, font_size: f32) {
        if (font_size - self.font_size).abs() < 0.01 {
            return;
        }
        self.font_size = font_size;
        self.line_height = (font_size * theme::LINE_HEIGHT_RATIO).ceil();
        self.cell_width = measure_cell(font_system, self.family, font_size, self.line_height);
        self.buffer
            .set_metrics(Metrics::new(font_size, self.line_height));
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), false);
    }

    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    pub fn gutter_width(&self) -> f32 {
        self.cell_width * (self.digits() as f32 + 1.5)
    }

    pub fn numbers(&self) -> &Buffer {
        &self.numbers
    }

    pub fn numbers_left(&self) -> f32 {
        self.cell_width * 0.5
    }

    /// One row per visible layout run so the gutter stays aligned with wrapped rows.
    pub fn refresh_numbers(&mut self, font_system: &mut FontSystem) {
        let scroll = self.buffer.scroll();
        let key = NumbersKey {
            scroll_line: scroll.line,
            scroll_vertical: scroll.vertical,
            line_count: self.document.line_count(),
            font_size: self.font_size,
            view_width: self.view_width,
            view_height: self.view_height,
        };
        if self.numbers_key == Some(key) {
            return;
        }
        self.numbers_key = Some(key);

        let digits = self.digits();
        let mut rows = Vec::new();
        let mut previous: Option<usize> = None;
        for run in self.buffer.layout_runs() {
            let first_row = previous != Some(run.line_i);
            previous = Some(run.line_i);
            let text = if first_row {
                format!("{:>digits$}", run.line_i + 1)
            } else {
                " ".repeat(digits)
            };
            rows.push((text, theme::text_dim()));
        }

        let mut buffer = Buffer::new_empty(Metrics::new(self.font_size, self.line_height));
        buffer.set_wrap(Wrap::None);
        buffer.set_tab_width(4);
        buffer.set_size(Some(self.gutter_width()), Some(self.view_height));
        for (text, color) in rows {
            buffer
                .lines
                .push(buffer_line(&text, &bold_attrs(self.family, color)));
        }
        if buffer.lines.is_empty() {
            buffer.lines.push(buffer_line(
                " ",
                &bold_attrs(self.family, theme::text_dim()),
            ));
        }
        buffer.set_scroll(Scroll {
            line: 0,
            vertical: scroll.vertical,
            horizontal: 0.0,
        });
        force_relayout(&mut buffer);
        buffer.shape_until_scroll(font_system, false);
        self.numbers = buffer;
    }

    fn digits(&self) -> usize {
        let mut value = self.document.line_count().max(1);
        let mut digits = 1;
        while value >= 10 {
            value /= 10;
            digits += 1;
        }
        digits
    }

    pub fn retheme(&mut self, font_system: &mut FontSystem) {
        let previous = self.buffer.scroll();
        self.rebuild_buffer(font_system);
        self.buffer.set_scroll(previous);
        self.shape_view(font_system);
        self.numbers_key = None;
    }

    pub fn shape(&mut self, font_system: &mut FontSystem) {
        self.shape_view(font_system);
    }

    /// Walk the view again.
    ///
    /// Rows this code puts into `buffer.lines` are new rather than stale, and
    /// cosmic-text only re-walks a buffer whose own caches it finds invalidated.
    /// Left to itself it walks nothing, so the old answer was to resize the
    /// buffer to nothing and back, which throws away the layout of every line
    /// in the file. Nudging the scroll says the same thing — the view is
    /// different now — without touching a line that did not change.
    fn shape_view(&mut self, font_system: &mut FontSystem) {
        let scroll = self.buffer.scroll();
        self.buffer.set_scroll(Scroll {
            vertical: scroll.vertical + 0.5,
            ..scroll
        });
        self.buffer.set_scroll(scroll);
        let screen = (self.view_height / self.line_height).ceil() as usize + 8;
        let rows = self.buffer.lines.len();
        let window = scroll.line.saturating_sub(screen)..(scroll.line + 2 * screen).min(rows);
        self.keep_only(window);
        self.buffer.shape_until_scroll(font_system, false);
    }

    /// Let go of the rows the view has left. A buffer that keeps every layout
    /// it ever made holds gigabytes for a big file, and cosmic-text's own way
    /// of avoiding that walks every line under the screen to do it; this walks
    /// the one screen it last shaped.
    fn keep_only(&mut self, window: Range<usize>) {
        if let Some((lo, hi)) = self.shaped {
            for index in lo..hi {
                if !window.contains(&index) {
                    if let Some(line) = self.buffer.lines.get_mut(index) {
                        line.reset_shaping();
                    }
                }
            }
        }
        self.shaped = (!window.is_empty()).then_some((window.start, window.end));
    }

    pub fn ensure_cursor_visible(&mut self, font_system: &mut FontSystem) {
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), false);
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

    /// A single typed character, with the completions a typist expects: an
    /// opener brings its closer and leaves the caret between the two, and `{`
    /// opens onto three indented lines. A closer that is already there is not
    /// doubled: `)` and `]` step over it, while `}`, `"` and `'` — the ones
    /// pressed reflexively — are swallowed and leave the caret where it was.
    /// The completion is written straight into the document and the caret
    /// placed by hand, never re-fed through this method, so it cannot retrigger
    /// itself.
    pub fn insert_typed(
        &mut self,
        font_system: &mut FontSystem,
        ch: char,
    ) -> Result<(), Unencodable> {
        if !self.selection.is_empty() {
            return self.insert_text(font_system, &ch.to_string());
        }
        let caret = self.selection.cursor;
        let line = self.document.line_text(caret.line);
        let at = byte_of_char(&line, caret.ch);
        let (before, rest) = (&line[..at], &line[at..]);
        let is_blank = |text: &str| text.chars().all(|c| c == ' ' || c == '\t');

        if matches!(ch, ')' | ']' | '}' | '"' | '\'') {
            let found = if rest.chars().next() == Some(ch) {
                // The closer we placed earlier, still the very next character.
                Some(Position::new(caret.line, caret.ch + 1))
            } else if is_blank(before)
                && is_blank(rest)
                && caret.line + 1 < self.document.line_count()
            {
                // The brace expansion parks the caret on an indent-only line with
                // its closer as the first thing on the line below; typing the
                // closer there must consume that closer, not stack a second one
                // on the middle line.
                let below = self.document.line_text(caret.line + 1);
                below
                    .chars()
                    .position(|c| c != ' ' && c != '\t')
                    .filter(|&at| below[at..].starts_with(ch))
                    .map(|at| Position::new(caret.line + 1, at + 1))
            } else {
                None
            };
            if let Some(target) = found {
                if matches!(ch, ')' | ']') {
                    self.selection = Selection::caret(target);
                    self.desired_x = None;
                    self.after_change(font_system);
                }
                return Ok(());
            }
        }

        let close = match ch {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '"' => '"',
            '\'' => '\'',
            _ => return self.insert_text(font_system, &ch.to_string()),
        };

        let lead: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let inserted = if ch == '{' {
            format!("{{\n{lead}\t\n{lead}{close}")
        } else {
            format!("{ch}{close}")
        };
        self.selection = self.document.apply_edit(self.selection, &inserted)?;
        let inner = if ch == '{' {
            Position::new(caret.line + 1, lead.chars().count() + 1)
        } else {
            Position::new(caret.line, caret.ch + 1)
        };
        self.selection = Selection::caret(inner);
        self.desired_x = None;
        self.after_change(font_system);
        Ok(())
    }

    /// Enter keeps the current line's leading whitespace, steps one literal tab
    /// deeper after an unclosed opener, and splits a pair the caret sits inside
    /// into three lines. Bytes outside the inserted text are never touched.
    pub fn newline(&mut self, font_system: &mut FontSystem) -> Result<(), Unencodable> {
        if !self.selection.is_empty() {
            self.selection = self.document.apply_edit(self.selection, "\n")?;
            self.after_change(font_system);
            return Ok(());
        }
        let caret = self.selection.cursor;
        let text = self.document.line_text(caret.line);
        let split = byte_of_char(&text, caret.ch);
        let (before, after) = (&text[..split], &text[split..]);
        let lead: String = text
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();

        let closes_here = after
            .chars()
            .find(|c| !c.is_whitespace())
            .is_some_and(|c| matches!(c, '}' | ')' | ']'))
            && after
                .chars()
                .rev()
                .find(|c| !c.is_whitespace())
                .is_some_and(|c| matches!(c, '}' | ')' | ']'));
        if closes_here {
            let middle = format!("{lead}\t");
            let inserted = format!("\n{middle}\n{lead}");
            self.selection = self.document.apply_edit(self.selection, &inserted)?;
            let inner = Position::new(caret.line + 1, middle.chars().count());
            self.selection = Selection::caret(inner);
            self.desired_x = None;
            self.after_change(font_system);
            return Ok(());
        }

        let opens = before
            .chars()
            .rev()
            .find(|c| !c.is_whitespace())
            .is_some_and(|c| matches!(c, '{' | '(' | '['));
        let middle = if opens { format!("{lead}\t") } else { lead };
        self.selection = self
            .document
            .apply_edit(self.selection, &format!("\n{middle}"))?;
        self.desired_x = None;
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
        self.shape_view(font_system);
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
            .shape_until_cursor(font_system, self.cursor_cosmic(), false);
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
        self.shape_view(font_system);
    }

    pub fn position_at(
        &mut self,
        font_system: &mut FontSystem,
        x: f32,
        y: f32,
    ) -> Option<Position> {
        self.shape_view(font_system);
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
            // cosmic-text marks every glyph of a run outside the range
            if run.line_i < start.line || run.line_i > end.line {
                continue;
            }
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

    /// The ink of the glyph beside the caret: `left` picks the one the caret
    /// stands to the right of, which is what typing just wrote and what
    /// Backspace is about to eat. Nothing when there is no glyph there.
    pub fn glyph_ink(&self, left: bool) -> Option<Color> {
        if left {
            self.ink_before_caret
        } else {
            self.ink_after_caret
        }
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

    pub fn line_delta(&mut self) -> (usize, usize) {
        self.document.line_delta()
    }

    fn after_change(&mut self, font_system: &mut FontSystem) {
        let mut deeper = false;
        if let Some(change) = self.document.take_change() {
            self.sync(change);
            if change.start_line < self.buffer.scroll().line {
                let current = self.buffer.scroll().line as isize;
                let delta = change.new_count as isize - change.old_count as isize;
                let line = (current + delta).max(0) as usize;
                let scroll = self.buffer.scroll();
                self.buffer.set_scroll(Scroll { line, ..scroll });
            }
            deeper = change.new_count > change.old_count;
            let line_attrs = bold_attrs(self.family, theme::text());
            let new_lines = (0..change.new_count)
                .map(|index| {
                    let text = self.document.line_text(change.start_line + index);
                    buffer_line(&text, &line_attrs)
                })
                .collect::<Vec<_>>();
            let range = change.start_line..change.start_line + change.old_count;
            if range.end > self.buffer.lines.len() {
                self.rebuild_buffer(font_system);
                return;
            }
            if change.old_count == change.new_count {
                // The same number of rows: overwrite them where they stand
                // rather than shift the tail of three hundred thousand lines.
                for (index, line) in range.zip(new_lines) {
                    self.buffer.lines[index] = line;
                }
            } else {
                self.buffer.lines.splice(range, new_lines);
            }
        }
        // One pass over the view. The caret cannot leave the screen by typing
        // into a line that is already on it, so it is only worth chasing when
        // the edit added lines.
        if deeper {
            self.ensure_cursor_visible(font_system);
        } else {
            self.shape_view(font_system);
        }
    }

    fn rebuild_buffer(&mut self, font_system: &mut FontSystem) {
        let line_attrs = bold_attrs(self.family, theme::text());
        let mut buffer = Buffer::new_empty(Metrics::new(self.font_size, self.line_height));
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_tab_width(4);
        buffer.set_size(Some(self.view_width), Some(self.view_height));
        for index in 0..self.document.line_count() {
            let text = self.document.line_text(index);
            buffer.lines.push(buffer_line(&text, &line_attrs));
        }
        self.buffer = buffer;
        self.colored = None;
        force_relayout(&mut self.buffer);
        self.buffer
            .shape_until_cursor(font_system, self.cursor_cosmic(), false);
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

fn byte_of_char(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

/// Decoded copy of the document, kept only for the highlighter. A document over the
/// tree-sitter budget returns `None` and stays uncoloured rather than trading memory
/// for colour; nothing here is ever written back.
fn mirror(document: &Document) -> Option<Rope> {
    if !syntax::enabled(document.len_bytes()) {
        return None;
    }
    let joined = (0..document.line_count())
        .map(|index| document.line_text(index))
        .collect::<Vec<String>>()
        .join("\n");
    Some(Rope::from(joined))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Fonts;
    use kite_core::Position;

    fn editor(text: &str) -> (Editor, Fonts) {
        let mut fonts = Fonts::load();
        let document =
            Document::from_bytes("kite-editor-test.txt", text.as_bytes().to_vec()).unwrap();
        let editor = Editor::new(
            document,
            &mut fonts.system,
            fonts.mono,
            400.0,
            200.0,
            16.0,
            Arc::new(Engine::new()),
        );
        (editor, fonts)
    }

    /// A Rust buffer, so the highlighter has real roles to colour.
    fn rust_editor(text: &str) -> (Editor, Fonts) {
        let mut fonts = Fonts::load();
        let document = Document::from_bytes("kite-editor-test.rs", text.as_bytes().to_vec())
            .expect("a document");
        let editor = Editor::new(
            document,
            &mut fonts.system,
            fonts.mono,
            400.0,
            200.0,
            16.0,
            Arc::new(Engine::new()),
        );
        (editor, fonts)
    }

    fn tops(editor: &Editor, line: usize) -> Vec<f32> {
        editor
            .buffer
            .layout_runs()
            .filter(|run| run.line_i == line)
            .map(|run| run.line_top)
            .collect()
    }

    /// The typing powder takes its ink from the glyph beside the caret: the one
    /// to its left is what typing just wrote, the one to its right is what a
    /// forward delete is about to eat.
    #[test]
    fn the_caret_reports_the_ink_on_either_side() {
        let (mut editor, mut fonts) = rust_editor("fn main() { let s = \"hi\"; }\n");
        let at = |ch: usize| Selection::new(Position::new(0, ch), Position::new(0, ch));
        let mauve = Some(Color::rgb(0xcb, 0xa6, 0xf7));
        let green = Some(Color::rgb(0xa6, 0xe3, 0xa1));

        editor.set_selection(&mut fonts.system, at(0));
        editor.highlight(&mut fonts.system);
        assert_eq!(
            editor.glyph_ink(true),
            None,
            "nothing sits left of column 0"
        );
        assert_eq!(
            editor.glyph_ink(false),
            mauve,
            "`f` of `fn` sits at the caret"
        );

        editor.set_selection(&mut fonts.system, at(2));
        editor.highlight(&mut fonts.system);
        assert_eq!(
            editor.glyph_ink(true),
            mauve,
            "`fn` is the theme's keyword mauve"
        );
        assert_eq!(
            editor.glyph_ink(false),
            Some(theme::text()),
            "the space past it is plain text"
        );

        editor.set_selection(&mut fonts.system, at(22));
        editor.highlight(&mut fonts.system);
        assert_eq!(
            editor.glyph_ink(true),
            green,
            "the same caret inside the literal takes the string green"
        );
    }

    #[test]
    fn selection_stays_on_the_selected_rows() {
        let (mut editor, mut fonts) = editor("alpha\nbravo\ncharlie delta\n");
        editor.set_selection(
            &mut fonts.system,
            Selection::new(Position::new(2, 0), Position::new(2, 7)),
        );
        let spans = editor.selection_spans();
        assert!(!spans.is_empty());
        let rows = tops(&editor, 2);
        assert!(
            spans.iter().all(|(_, y, _, _)| rows.contains(y)),
            "spans leaked onto other lines: {spans:?}"
        );
    }

    #[test]
    fn multiline_selection_covers_only_its_range() {
        let (mut editor, mut fonts) = editor("alpha\nbravo\ncharlie\ndelta\n");
        editor.set_selection(
            &mut fonts.system,
            Selection::new(Position::new(1, 2), Position::new(2, 3)),
        );
        let spans = editor.selection_spans();
        let mut rows = tops(&editor, 1);
        rows.extend(tops(&editor, 2));
        assert!(!spans.is_empty());
        assert!(
            spans.iter().all(|(_, y, _, _)| rows.contains(y)),
            "spans leaked outside the selection: {spans:?}"
        );
    }

    #[test]
    fn gutter_holds_one_row_per_visible_run() {
        let (mut editor, mut fonts) = editor("one\ntwo\nthree\n");
        editor.refresh_numbers(&mut fonts.system);
        let rows: Vec<String> = editor
            .numbers
            .lines
            .iter()
            .map(|line| line.text().to_string())
            .collect();
        assert_eq!(rows, vec!["1", "2", "3", "4"]);
    }

    /// The cost of a keystroke is the screen, not the file: a change relays out
    /// the row it touched and nothing else. Run with `--nocapture` to see the
    /// numbers; the bound is the one a document-wide relayout breaks.
    #[test]
    fn typing_in_a_ten_megabyte_file_costs_by_the_screen_not_by_the_file() {
        use std::time::{Duration, Instant};

        let line = "line of text that is long enough to wrap\n";
        let (mut few, mut fonts) = editor(&line.repeat(300));
        let start = Instant::now();
        for _ in 0..100 {
            few.insert_text(&mut fonts.system, "x").unwrap();
        }
        let small = start.elapsed();

        let big_text = line.repeat(300_000);
        assert!(big_text.len() > 10 * 1024 * 1024);
        let (mut many, _) = editor(&big_text);
        drop(big_text);
        let start = Instant::now();
        for _ in 0..100 {
            many.insert_text(&mut fonts.system, "x").unwrap();
        }
        let big = start.elapsed();

        assert_eq!(
            many.document
                .line_text(0)
                .chars()
                .take_while(|ch| *ch == 'x')
                .count(),
            100,
            "the keystrokes did not land on one line"
        );
        println!("100 keystrokes: {small:?} in 300 lines, {big:?} in 12MB");
        assert!(
            big < Duration::from_millis(100),
            "100 keystrokes in a 12MB file took {big:?}, against the 11ms it \
             costs once a keystroke lays out the screen rather than the file"
        );
    }

    /// The rows behind the view let go of their layout, so walking a whole
    /// 12MB file costs the same memory as looking at it.
    #[test]
    fn scrolling_lets_go_of_the_rows_it_passed() {
        let (mut editor, mut fonts) =
            editor(&"line of text that is long enough to wrap\n".repeat(300_000));
        for _ in 0..12_000 {
            editor.scroll_by(&mut fonts.system, editor.view_height);
        }
        assert!(
            editor.buffer.scroll().line > 90_000,
            "the walk stopped at {}",
            editor.buffer.scroll().line
        );
        let shaped = editor
            .buffer
            .lines
            .iter()
            .filter(|line| line.shape_opt().is_some())
            .count();
        let rows = (editor.view_height / editor.line_height).ceil() as usize;
        assert!(
            shaped <= 4 * rows + 16,
            "{shaped} rows kept their layout for a {rows}-row screen"
        );
    }

    /// A row that starts wrapping pushes the rows below it down. Their layouts
    /// are recomputed as the view walks them, so the change needs no
    /// document-wide invalidation to land in the right place.
    #[test]
    fn a_row_that_starts_wrapping_pushes_the_rows_below_down() {
        let (mut editor, mut fonts) = editor("ab\ncd\n");
        let before = tops(&editor, 1);
        editor
            .insert_text(&mut fonts.system, &"w".repeat(200))
            .unwrap();
        let after = tops(&editor, 1);
        assert!(
            editor
                .buffer
                .layout_runs()
                .filter(|run| run.line_i == 0)
                .count()
                > 1,
            "the first line did not wrap"
        );
        assert!(
            after[0] > before[0],
            "line 1 stayed put while line 0 wrapped: {before:?} -> {after:?}"
        );
    }

    fn place(editor: &mut Editor, fonts: &mut Fonts, line: usize, ch: usize) {
        editor.set_selection(&mut fonts.system, Selection::caret(Position::new(line, ch)));
    }

    fn rows(editor: &Editor) -> Vec<String> {
        (0..editor.document.line_count())
            .map(|index| editor.document.line_text(index))
            .collect()
    }

    #[test]
    fn enter_repeats_the_leading_whitespace() {
        let (mut editor, mut fonts) = editor("\tint x = 1;\n");
        place(&mut editor, &mut fonts, 0, 11);
        editor.newline(&mut fonts.system).unwrap();
        assert_eq!(rows(&editor), vec!["\tint x = 1;", "\t", ""]);
        assert_eq!(editor.selection.cursor, Position::new(1, 1));
    }

    #[test]
    fn enter_copies_tabs_and_spaces_in_order() {
        let (mut editor, mut fonts) = editor("  \tx\n");
        place(&mut editor, &mut fonts, 0, 4);
        editor.newline(&mut fonts.system).unwrap();
        assert_eq!(rows(&editor)[1], "  \t");
    }

    #[test]
    fn enter_steps_one_tab_after_an_opener() {
        let (mut editor, mut fonts) = editor("int main() {\n}\n");
        place(&mut editor, &mut fonts, 0, 12);
        editor.newline(&mut fonts.system).unwrap();
        assert_eq!(rows(&editor), vec!["int main() {", "\t", "}", ""]);
        assert_eq!(editor.selection.cursor, Position::new(1, 1));
    }

    #[test]
    fn enter_inside_a_pair_splits_into_three_lines() {
        let (mut editor, mut fonts) = editor("    if (x) {}\n");
        place(&mut editor, &mut fonts, 0, 12);
        editor.newline(&mut fonts.system).unwrap();
        assert_eq!(rows(&editor), vec!["    if (x) {", "    \t", "    }", ""]);
        assert_eq!(editor.selection.cursor, Position::new(1, 5));
    }

    #[test]
    fn enter_never_touches_the_other_side_of_a_pair() {
        let (mut editor, mut fonts) = editor("f(ab)\n");
        place(&mut editor, &mut fonts, 0, 3);
        editor.newline(&mut fonts.system).unwrap();
        // Nothing at the caret is a closer and the line does not end with an
        // opener, so this is a plain line break: text after the caret is intact.
        assert_eq!(rows(&editor), vec!["f(a", "b)", ""]);
        assert_eq!(editor.selection.cursor, Position::new(1, 0));
    }

    #[test]
    fn typing_an_opener_parks_the_caret_between_the_pair() {
        let (mut editor, mut fonts) = editor("f\n");
        place(&mut editor, &mut fonts, 0, 1);
        editor.insert_typed(&mut fonts.system, '(').unwrap();
        assert_eq!(rows(&editor), vec!["f()", ""]);
        assert_eq!(editor.selection.cursor, Position::new(0, 2));
    }

    #[test]
    fn typing_the_closer_over_a_completion_steps_over_it() {
        let (mut editor, mut fonts) = editor("f\n");
        place(&mut editor, &mut fonts, 0, 1);
        editor.insert_typed(&mut fonts.system, '(').unwrap();
        editor.insert_typed(&mut fonts.system, ')').unwrap();
        assert_eq!(rows(&editor), vec!["f()", ""], "the closer is not doubled");
        assert_eq!(
            editor.selection.cursor,
            Position::new(0, 3),
            "the caret steps onto the existing closer"
        );
    }

    #[test]
    fn typing_a_brace_opens_three_indented_lines() {
        let (mut editor, mut fonts) = editor("    if (x)\n");
        place(&mut editor, &mut fonts, 0, 10);
        editor.insert_typed(&mut fonts.system, '{').unwrap();
        assert_eq!(rows(&editor), vec!["    if (x){", "    \t", "    }", ""]);
        assert_eq!(editor.selection.cursor, Position::new(1, 5));
    }

    #[test]
    fn closing_the_expansion_neither_doubles_nor_moves() {
        let (mut editor, mut fonts) = editor("    if (x)\n");
        place(&mut editor, &mut fonts, 0, 10);
        editor.insert_typed(&mut fonts.system, '{').unwrap();
        // The caret sits on the indent-only middle line; the typist's closing
        // brace must eat the closer below and stay put, not leave two of them
        // and not drag the caret down onto the brace.
        editor.insert_typed(&mut fonts.system, '}').unwrap();
        assert_eq!(rows(&editor), vec!["    if (x){", "    \t", "    }", ""]);
        assert_eq!(editor.selection.cursor, Position::new(1, 5));
    }

    #[test]
    fn a_closer_after_real_code_still_inserts() {
        let (mut editor, mut fonts) = editor("{\n    stmt\n}\n");
        place(&mut editor, &mut fonts, 1, 8);
        editor.insert_typed(&mut fonts.system, '}').unwrap();
        assert_eq!(
            rows(&editor),
            vec!["{", "    stmt}", "}", ""],
            "a closer below a line of code is not the expansion's, so it inserts"
        );
    }

    #[test]
    fn typing_a_quote_twice_yields_one_pair_not_four() {
        let (mut editor, mut fonts) = editor("\n");
        place(&mut editor, &mut fonts, 0, 0);
        editor.insert_typed(&mut fonts.system, '"').unwrap();
        editor.insert_typed(&mut fonts.system, '"').unwrap();
        assert_eq!(rows(&editor), vec!["\"\"", ""]);
        assert_eq!(
            editor.selection.cursor,
            Position::new(0, 1),
            "the second quote is swallowed and the caret stays between the pair"
        );
    }

    #[test]
    fn an_apostrophe_over_its_own_completion_is_swallowed() {
        let (mut editor, mut fonts) = editor("a\n");
        place(&mut editor, &mut fonts, 0, 1);
        editor.insert_typed(&mut fonts.system, '\'').unwrap();
        editor.insert_typed(&mut fonts.system, '\'').unwrap();
        editor.insert_typed(&mut fonts.system, '\'').unwrap();
        assert_eq!(rows(&editor), vec!["a''", ""], "one pair, however often it is pressed");
        assert_eq!(editor.selection.cursor, Position::new(0, 2));
    }

    #[test]
    fn a_verbatim_insert_does_not_pair() {
        let (mut editor, mut fonts) = editor("f\n");
        place(&mut editor, &mut fonts, 0, 1);
        editor.insert_text(&mut fonts.system, "(").unwrap();
        assert_eq!(rows(&editor), vec!["f(", ""], "pastes and IME stay literal");
    }

    #[test]
    fn one_undo_undoes_the_whole_split() {
        let (mut editor, mut fonts) = editor("if (x) {\n}\n");
        place(&mut editor, &mut fonts, 0, 8);
        editor.newline(&mut fonts.system).unwrap();
        assert_eq!(editor.document.is_modified(), true);
        assert!(editor.undo(&mut fonts.system));
        assert_eq!(rows(&editor), vec!["if (x) {", "}", ""]);
        assert_eq!(editor.document.line_delta(), (0, 0));
    }

    #[test]
    fn enter_with_a_selection_replaces_it_and_stays_atomic() {
        let (mut editor, mut fonts) = editor("alpha\nbravo\n");
        editor.set_selection(
            &mut fonts.system,
            Selection::new(Position::new(0, 5), Position::new(1, 5)),
        );
        editor.newline(&mut fonts.system).unwrap();
        assert_eq!(rows(&editor), vec!["alpha", "", ""]);
        assert!(editor.undo(&mut fonts.system));
        assert_eq!(rows(&editor), vec!["alpha", "bravo", ""]);
    }
}

pub(crate) fn measure_cell(
    font_system: &mut FontSystem,
    family: &'static str,
    font_size: f32,
    line_height: f32,
) -> f32 {
    const SAMPLE: &str = "0000000000";
    let mut buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
    buffer.set_wrap(Wrap::None);
    buffer.set_size(None, None);
    buffer
        .lines
        .push(buffer_line(SAMPLE, &bold_attrs(family, theme::text())));
    buffer.shape_until_scroll(font_system, false);
    match buffer.layout_runs().next() {
        Some(run) => run.line_w / SAMPLE.chars().count() as f32,
        None => font_size * 0.6,
    }
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
