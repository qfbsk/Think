//! A list of ways to finish what is being typed.
//!
//! The shell and a page of code want the same thing from it: hold the candidates
//! under the caret, let one be walked to and chosen. What differs is where the
//! half-finished word lives, so the list never edits anything. The caller keeps
//! the range a pick replaces — a character index into the shell's input line, or
//! a position in a document — and does the replacing itself when [`Completer::accept`]
//! hands the text back.

use glyphon::FontSystem;
use glyphon::cosmic_text::{Buffer, Color};
use glyphon::{TextArea, TextBounds};
use kite_core::Position;

use crate::renderer::QuadVertex;
use crate::text::{attrs, make_label, measure};
use crate::theme;
use crate::ui::{push_line, push_rect, Rect};

/// Rows on screen at once. Past this the list scrolls under the caret rather
/// than growing a second column, which would have to dodge the window edge too.
const MAX_ROWS: usize = 9;
/// Candidates kept at all. A word list from a big file runs to thousands, and
/// nobody reads past the first screenful of a tie.
const MAX_ITEMS: usize = 200;
const ROW_H: f32 = 21.0;
const PAD: f32 = 7.0;
/// The gap between the caret's row and the top of the list.
const GAP: f32 = 3.0;
const MIN_W: f32 = 170.0;
const MAX_W: f32 = 460.0;

/// What a candidate is, so the row can say so without spelling it out. The
/// spellings are the ones a language server uses for the same ideas.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Command,
    Path,
    /// A program's own argument, as distinct from a word on a page.
    Flag,
    Word,
    Keyword,
    Function,
    Struct,
    Type,
    Member,
    Variable,
    Module,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Command => "cmd",
            Kind::Path => "path",
            Kind::Flag => "flag",
            Kind::Word => "word",
            Kind::Keyword => "key",
            Kind::Function => "fn",
            Kind::Struct => "struct",
            Kind::Type => "type",
            Kind::Member => "field",
            Kind::Variable => "var",
            Kind::Module => "mod",
        }
    }
}

#[derive(Clone)]
pub struct Item {
    /// The row, and the word the prefix is matched against. A header is listed
    /// as `stdio.h`, though accepting it types the quote that closes the
    /// directive.
    pub name: String,
    /// What accepting types, exactly as the server wrote it.
    pub text: String,
    pub kind: Kind,
    /// What goes beside the name in the panel: a server's signature, or the
    /// folder a path came out of. It is decoration, so a row can be read with it
    /// missing.
    pub note: Option<String>,
    /// The call this row is accepted as, when the server sent a snippet: the
    /// parentheses, the placeholders and where Tab walks through them. The row
    /// itself still reads as the name.
    pub call: Option<Call>,
    /// Edits the server wants made along with the insert: the `#include` that
    /// makes the name compile where it is being put.
    pub edits: Vec<ExtraEdit>,
}

impl Item {
    pub fn new(text: impl Into<String>, kind: Kind) -> Self {
        let text = text.into();
        Self {
            name: text.clone(),
            text,
            kind,
            note: None,
            call: None,
            edits: Vec::new(),
        }
    }
}

/// A snippet, already unrolled: the text to type, and the placeholders inside
/// it as character offsets. The last stop is the final caret (`$0`), which is
/// where the session ends rather than something to select.
#[derive(Clone)]
pub struct Call {
    pub text: String,
    pub stops: Vec<(usize, usize)>,
}

/// One edit at the server's own coordinates: lines as shown, columns in UTF-16
/// units. It is turned into a document position only where the page is known.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExtraEdit {
    pub start: (usize, u64),
    pub end: (usize, u64),
    pub text: String,
}

/// Where the pick goes, and how far back the half-word reaches.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A character index into `Shell::input`, which is what the caret sits after.
    Shell { start: usize },
    /// The span the pick replaces. The caret does not move while the list is up,
    /// so this stays where it was pointed; `end` can sit past the caret when the
    /// server means the whole of a word the reader is standing in the middle of.
    Editor { start: Position, end: Position },
}

/// Where the answer is written, and what the page around it looks like.
#[derive(Clone)]
pub struct Style {
    /// The caret: x, and the top of its row, in window coordinates.
    pub origin: (f32, f32),
    pub line_height: f32,
    pub family: &'static str,
    pub scale: f32,
}

pub struct Completer {
    open: bool,
    target: Target,
    /// Everything the caller offered, in the order it wants it read.
    pool: Vec<Item>,
    /// Indices into `pool` that still match what has been typed.
    rows: Vec<usize>,
    picked: usize,
    top: usize,
    /// The caret, in window coordinates: x and the top of its row.
    origin: (f32, f32),
    line_height: f32,
    family: &'static str,
    scale: f32,
    /// What has been typed so far.
    word: String,
    /// Whether the word itself is one of the candidates. Then it is a word the
    /// reader has finished writing, and a one-row list saying so is a menu that
    /// repeats the line.
    whole: bool,
    /// The word a dismissal was about. It stays dismissed while that is still
    /// the word: an answer that came straight back after being waved off would
    /// be a worse answer than no answer.
    dismissed: Option<String>,
    panel: Rect,
    row_rects: Vec<Rect>,
    labels: Vec<Buffer>,
    notes: Vec<Buffer>,
    dirty: bool,
}

impl Completer {
    pub fn new(family: &'static str, scale: f32) -> Self {
        Self {
            open: false,
            target: Target::Shell { start: 0 },
            pool: Vec::new(),
            rows: Vec::new(),
            picked: 0,
            top: 0,
            origin: (0.0, 0.0),
            line_height: ROW_H,
            family,
            scale,
            word: String::new(),
            whole: false,
            dismissed: None,
            panel: Rect::default(),
            row_rects: Vec::new(),
            labels: Vec::new(),
            notes: Vec::new(),
            dirty: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn target(&self) -> Target {
        self.target
    }

    /// Where the answer is written: the caret to hang on, and the page it is on.
    /// The list was not asked for, so Escape puts it away for as long as the word
    /// stands still.
    pub fn show(&mut self, style: &Style, pool: Vec<Item>, prefix: &str, target: Target) {
        self.target = target;
        self.origin = style.origin;
        self.line_height = style.line_height;
        self.family = style.family;
        self.scale = style.scale;
        self.pool = pool;
        if self.dismissed.as_deref() == Some(prefix) {
            return;
        }
        self.dismissed = None;
        if !self.match_rows(prefix) {
            self.close();
            return;
        }
        self.open = true;
        self.dirty = true;
    }

    /// Wave the answer off. It comes back as soon as the word is no longer the
    /// one that was refused — typing again is the reader asking again.
    pub fn dismiss(&mut self) {
        self.dismissed = Some(self.word.clone());
        self.close();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.pool.clear();
        self.rows.clear();
    }

    /// Narrow to what has been typed, and move the answer to wherever the caret
    /// has got to. The second half is not a detail: an answer that keeps the place
    /// it was first drawn in walks backwards over every letter typed after it.
    /// Returns false when nothing is left, which the caller reads as "put it away".
    pub fn refine(&mut self, style: &Style, prefix: &str) -> bool {
        if !self.open {
            return false;
        }
        if self.dismissed.as_deref() == Some(prefix) {
            return false;
        }
        self.origin = style.origin;
        self.line_height = style.line_height;
        self.family = style.family;
        self.scale = style.scale;
        if !self.match_rows(prefix) {
            self.close();
            return false;
        }
        self.dirty = true;
        true
    }

    /// Walk the highlight. It runs off the bottom and comes back at the top,
    /// because a list long enough to need that is a list you do not want to
    /// page through with the key held down.
    pub fn move_by(&mut self, delta: i32) {
        if !self.open || self.rows.is_empty() {
            return;
        }
        let count = self.rows.len() as i32;
        self.picked = (self.picked as i32 + delta).rem_euclid(count) as usize;
        let shown = self.shown();
        if self.picked < self.top {
            self.top = self.picked;
        } else if self.picked >= self.top + shown {
            self.top = self.picked + 1 - shown;
        }
        self.dirty = true;
    }

    /// Take the highlighted candidate and put the list away. The whole row is
    /// handed back, not just its name: a row can also carry the snippet it is
    /// accepted as, and the edits that make it compile where it lands.
    pub fn accept(&mut self) -> Option<Item> {
        let item = self
            .rows
            .get(self.picked)
            .and_then(|index| self.pool.get(*index))
            .cloned();
        self.close();
        item
    }

    /// Choose a row the mouse landed on.
    pub fn pick_at(&mut self, x: f32, y: f32) -> bool {
        if !self.over(x, y) {
            return false;
        }
        if let Some(index) = self
            .row_rects
            .iter()
            .position(|row| y >= row.y && y < row.bottom())
        {
            let absolute = self.top + index;
            if absolute < self.rows.len() {
                self.picked = absolute;
                self.dirty = true;
            }
        }
        true
    }

    pub fn over(&self, x: f32, y: f32) -> bool {
        self.open
            && x >= self.panel.x
            && x < self.panel.right()
            && y >= self.panel.y
            && y < self.panel.bottom()
    }

    fn shown(&self) -> usize {
        self.rows.len().min(MAX_ROWS)
    }

    fn match_rows(&mut self, prefix: &str) -> bool {
        self.word = prefix.to_string();
        let mut exact: Vec<usize> = Vec::new();
        let mut literal: Vec<usize> = Vec::new();
        let mut folded_starts: Vec<usize> = Vec::new();
        let mut rest: Vec<usize> = Vec::new();
        let needle = prefix.trim_start_matches(['.', '/', '\\', '~']);
        let folded = needle.to_lowercase();
        self.whole = false;
        for (index, item) in self.pool.iter().enumerate() {
            let name = item.name.to_lowercase();
            if name == folded {
                // The word on the row is already one of the answers, whole. It is
                // still the row the reader is typing towards, so it stays first;
                // leaving it out is how typing `int` offered `inline` instead.
                self.whole = true;
                exact.push(index);
                continue;
            }
            if item.name.starts_with(needle) {
                // What literally continues the word outranks what only matches
                // once the case is forgotten: `int16_t` before `INT16_C`.
                literal.push(index);
            } else if name.starts_with(&folded) {
                folded_starts.push(index);
            } else if !folded.is_empty() && in_order(&name, &folded) {
                // `pnt` for `printf`: the letters are there in order, which is the
                // loosest thing a name can do. It is a row in a list, never a word
                // written on the line by itself.
                rest.push(index);
            }
        }
        exact.extend(literal);
        exact.extend(folded_starts);
        exact.extend(rest);
        exact.truncate(MAX_ITEMS);
        self.rows = exact;
        self.picked = 0;
        self.top = 0;
        // One row that only repeats what is already written is a menu saying
        // nothing, so it does not open at all.
        !self.rows.is_empty() && !(self.rows.len() == 1 && self.whole)
    }

    fn text_size(&self) -> f32 {
        13.0 * self.scale
    }

    /// Re-measure and re-set the panel and its rows. Only run when something
    /// changed: `quads` asks for it every frame and gets the cache back.
    fn geometry(&mut self, font_system: &mut FontSystem, size: (f32, f32)) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let s = self.scale;
        let text = self.text_size();
        let body = attrs(self.family, theme::pane(true).text);
        let dim = attrs(self.family, theme::pane(true).text_dim);
        let shown = self.shown();

        let mut labels = Vec::with_capacity(shown);
        let mut notes = Vec::with_capacity(shown);
        let mut widest: f32 = 0.0;
        for index in self.top..self.top + shown {
            let Some(item) = self.rows.get(index).and_then(|i| self.pool.get(*i)) else {
                continue;
            };
            let label = make_label(font_system, &body, text, &item.name);
            let (width, _) = measure(&label);
            let note_text = match &item.note {
                Some(origin) => format!("{} · {}", item.kind.label(), origin),
                None => item.kind.label().to_string(),
            };
            let note = make_label(font_system, &dim, text * 0.85, &note_text);
            let (note_w, _) = measure(&note);
            // The row is the candidate and its kind, with a gap between them, so
            // the widest row is what the panel has to be.
            widest = widest.max(width + note_w + PAD * s);
            labels.push(label);
            notes.push(note);
        }

        let width = (widest + PAD * 2.0 * s).clamp(MIN_W * s, MAX_W * s);
        let height = shown as f32 * ROW_H * s + PAD * 2.0 * s;
        let (caret_x, caret_top) = self.origin;
        let mut x = caret_x;
        if x + width > size.0 {
            x = (size.0 - width).max(0.0);
        }
        let mut y = caret_top + self.line_height + GAP * s;
        if y + height > size.1 {
            // Run out of the window below, and it is better to cover the text
            // being completed than to vanish.
            y = (caret_top - height - GAP * s).max(0.0);
        }
        self.panel = Rect { x, y, w: width, h: height };
        self.row_rects = (0..shown)
            .map(|row| Rect {
                x: self.panel.x,
                y: self.panel.y + PAD * s + row as f32 * ROW_H * s,
                w: self.panel.w,
                h: ROW_H * s,
            })
            .collect();
        self.labels = labels;
        self.notes = notes;
    }

    pub fn quads(&mut self, font_system: &mut FontSystem, size: (f32, f32)) -> Vec<QuadVertex> {
        if !self.open {
            return Vec::new();
        }
        self.geometry(font_system, size);
        let colors = theme::pane(true);
        let s = self.scale;
        let mut quads = Vec::new();
        push_rect(&mut quads, self.panel, popup_bg(colors.bg));
        let edge = popup_edge(colors.border);
        for (from, to) in [
            ((self.panel.x, self.panel.y), (self.panel.right(), self.panel.y)),
            ((self.panel.x, self.panel.bottom()), (self.panel.right(), self.panel.bottom())),
            ((self.panel.x, self.panel.y), (self.panel.x, self.panel.bottom())),
            ((self.panel.right(), self.panel.y), (self.panel.right(), self.panel.bottom())),
        ] {
            push_line(&mut quads, from.0, from.1, to.0, to.1, 1.0 * s, edge);
        }
        let picked = self.picked;
        for (index, row) in self.row_rects.iter().enumerate() {
            if index + self.top == picked {
                push_rect(&mut quads, *row, popup_pick(colors.accent));
            }
        }
        quads
    }

    pub fn areas(&self) -> Vec<TextArea<'_>> {
        if !self.open {
            return Vec::new();
        }
        let colors = theme::pane(true);
        let s = self.scale;
        let bounds = TextBounds {
            left: self.panel.x as i32,
            top: self.panel.y as i32,
            right: self.panel.right() as i32,
            bottom: self.panel.bottom() as i32,
        };
        let mut areas = Vec::new();
        for (index, label) in self.labels.iter().enumerate() {
            let Some(row) = self.row_rects.get(index) else {
                continue;
            };
            let (_, height) = measure(label);
            areas.push(TextArea {
                buffer: label,
                left: row.x + PAD * s,
                top: row.y + (row.h - height) / 2.0,
                scale: 1.0,
                bounds,
                default_color: colors.text,
                custom_glyphs: &[],
            });
        }
        for (index, note) in self.notes.iter().enumerate() {
            let Some(row) = self.row_rects.get(index) else {
                continue;
            };
            let (width, height) = measure(note);
            areas.push(TextArea {
                buffer: note,
                left: row.right() - PAD * s - width,
                top: row.y + (row.h - height) / 2.0,
                scale: 1.0,
                bounds,
                default_color: colors.text_dim,
                custom_glyphs: &[],
            });
        }
        areas
    }
}

/// Whether every letter of the needle turns up in the name in the same order,
/// with anything allowed in between: `pnt` answers `printf`. Both sides are
/// already folded.
fn in_order(haystack: &str, needle: &str) -> bool {
    let mut wanted = needle.chars();
    let mut goal = wanted.next();
    for ch in haystack.chars() {
        if Some(ch) == goal {
            goal = wanted.next();
            if goal.is_none() {
                return true;
            }
        }
    }
    goal.is_none()
}

/// The panel sits over whatever the theme is already drawing, so it needs to be
/// opaque on its own; a translucent one would let the code underneath read
/// through the candidates.
fn popup_bg(bg: Color) -> Color {
    let [_, _, _, a] = bg.as_rgba();
    if a >= 250 {
        bg
    } else {
        Color::rgba(bg.r(), bg.g(), bg.b(), 250)
    }
}

fn popup_pick(accent: Color) -> Color {
    Color::rgba(accent.r(), accent.g(), accent.b(), 0x33)
}

fn popup_edge(border: Color) -> Color {
    let [_, _, _, a] = border.as_rgba();
    if a > 0x60 {
        border
    } else {
        Color::rgba(border.r(), border.g(), border.b(), 0xc0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> Style {
        Style {
            origin: (0.0, 0.0),
            line_height: 16.0,
            family: "test",
            scale: 1.0,
        }
    }

    fn open(names: &[&str], prefix: &str) -> Completer {
        let pool = names
            .iter()
            .map(|name| Item::new(*name, Kind::Word))
            .collect();
        let mut list = Completer::new("test", 1.0);
        list.show(&style(), pool, prefix, Target::Shell { start: 0 });
        list
    }

    fn shown(list: &Completer) -> Vec<String> {
        list.rows
            .iter()
            .map(|index| list.pool[*index].text.clone())
            .collect()
    }

    /// The list is drawn where there is room for it: below the caret normally,
    /// above it when the caret sits near the bottom of the window. This is the
    /// whole reason the command line can have a list at all — its line is the last
    /// thing on the screen, and the ghost was kept there on the strength of it.
    #[test]
    fn a_list_opens_where_there_is_room_for_it() {
        let mut fonts = crate::fonts::Fonts::load();
        let mut place = |caret_y: f32, window: f32| {
            let mut list = open(&["gcc.txt", "gccd.txt", "gcc.exe"], "g");
            list.origin = (0.0, caret_y);
            list.quads(&mut fonts.system, (1600.0, window));
            list.panel
        };

        let high = place(100.0, 1000.0);
        assert!(high.y > 100.0, "room below, so it opens below: {high:?}");
        let low = place(960.0, 1000.0);
        assert!(
            low.y + low.h <= 960.0,
            "no room below, so it turns around: {low:?}"
        );
        assert!(low.y >= 0.0, "and it stays inside the window");
    }

    /// The word already on the row is a candidate like any other and it is the one
    /// the reader is typing towards, so the list shows it first. A list of nothing
    /// but that word is no choice at all, and does not open.
    #[test]
    fn a_whole_word_is_the_first_row() {
        let list = open(&["led", "ledger"], "led");
        assert_eq!(shown(&list), vec!["led", "ledger"]);
        let alone = open(&["led"], "led");
        assert!(!alone.is_open(), "one row that repeats the page is not a list");
    }

    /// A name that only matches once the case is forgotten is a row in a list —
    /// where the reader can see what Tab would really write — and never a finish
    /// claimed for the word. `INT16_C` for `int` is the case that used to be drawn
    /// as `int16_C`.
    #[test]
    fn a_name_that_only_matches_by_case_is_a_row() {
        let list = open(&["INT16_C", "int16_t"], "int");
        assert_eq!(shown(&list), vec!["int16_t", "INT16_C"]);
    }

    #[test]
    fn the_candidate_that_starts_the_way_it_was_typed_comes_first() {
        let list = open(&["SCardErr.h>", "stdio.h>"], "s");
        assert_eq!(shown(&list), vec!["stdio.h>", "SCardErr.h>"]);
    }

    #[test]
    fn typing_narrows_and_deleting_widens_again() {
        let mut list = open(&["cargo", "card", "code"], "ca");
        assert_eq!(shown(&list), vec!["cargo", "card"]);
        list.refine(&style(), "cod");
        assert_eq!(shown(&list), vec!["code"]);
        list.refine(&style(), "ca");
        assert_eq!(
            shown(&list),
            vec!["cargo", "card"],
            "the offer is kept, not asked for again"
        );
    }

    #[test]
    fn a_prefix_from_the_middle_is_still_found() {
        let list = open(&["readme_first", "notes"], "first");
        assert_eq!(shown(&list), vec!["readme_first"]);
    }

    #[test]
    fn letters_that_begin_words_are_enough() {
        let list = open(&["main_function", "compute_thing"], "mf");
        assert_eq!(shown(&list), vec!["main_function"]);
    }

    #[test]
    fn letters_in_order_are_the_last_thing_tried() {
        let list = open(&["printf", "name_thing"], "pnt");
        assert_eq!(shown(&list), vec!["printf"]);
        let list = open(&["pnt_helper", "printf"], "pnt");
        assert_eq!(
            shown(&list),
            vec!["pnt_helper", "printf"],
            "a name that really starts with it outranks a loose match"
        );
    }

    #[test]
    fn when_nothing_starts_with_it_the_list_goes_away() {
        let mut list = open(&["cargo", "code"], "ca");
        assert!(!list.refine(&style(), "zzz"));
        assert!(!list.is_open());
    }

    #[test]
    fn the_highlight_wraps_round_at_both_ends() {
        let mut list = open(&["a_1", "a_2", "a_3"], "a_");
        assert_eq!(list.picked, 0);
        list.move_by(-1);
        assert_eq!(list.picked, 2, "up from the first row lands on the last");
        list.move_by(1);
        assert_eq!(list.picked, 0, "and down from the last lands back on the first");
    }

    #[test]
    fn accepting_hands_over_the_highlighted_row_once() {
        let mut list = open(&["cargo", "card"], "ca");
        list.move_by(1);
        assert_eq!(list.accept().map(|item| item.text).as_deref(), Some("card"));
        assert!(!list.is_open(), "choosing is also finishing");
        assert!(list.accept().is_none(), "and there is no second choice to take");
    }

    #[test]
    fn an_empty_offer_is_not_a_list() {
        let mut list = Completer::new("test", 1.0);
        list.show(&style(), Vec::new(), "", Target::Shell { start: 0 });
        assert!(!list.is_open());
    }

    #[test]
    fn a_list_past_the_panel_scrolls_with_the_highlight() {
        let pool: Vec<Item> = (0..20)
            .map(|index| Item::new(format!("item_{index}"), Kind::Word))
            .collect();
        let mut list = Completer::new("test", 1.0);
        list.show(&style(), pool, "item", Target::Shell { start: 0 });
        assert_eq!(list.top, 0);
        for _ in 0..MAX_ROWS {
            list.move_by(1);
        }
        assert_eq!(list.picked, MAX_ROWS);
        assert!(list.top > 0, "the window moved down to keep it in view");
    }


    /// The bug this guards: an answer placed once, when the word was one letter
    /// long, and left there while more letters were typed — the list ending up
    /// written on top of the question.
    #[test]
    fn a_list_keeps_up_with_the_caret() {
        let mut list = open(&["gcc", "gdb"], "g");
        let where_it_started = list.origin.0;
        let mut after_typing = style();
        after_typing.origin = (where_it_started + 12.0, 0.0);
        assert!(list.refine(&after_typing, "gc"));
        assert_eq!(
            list.origin.0,
            where_it_started + 12.0,
            "the panel moved to where the caret now stands"
        );
        assert_eq!(shown(&list), vec!["gcc"], "and shrank to match");
    }

    #[test]
    fn a_refused_list_stays_refused_while_the_word_is_the_same() {
        let mut list = open(&["gcc", "gdb"], "g");
        assert!(list.is_open());
        list.dismiss();
        assert!(!list.is_open(), "Escape put it away");
        let pool = vec![Item::new("gcc", Kind::Command), Item::new("gdb", Kind::Command)];
        list.show(&style(), pool.clone(), "g", Target::Shell { start: 0 });
        assert!(!list.is_open(), "it came straight back, which is worse than nothing");
        list.show(&style(), pool, "gc", Target::Shell { start: 0 });
        assert!(list.is_open(), "a different word is a different answer");
    }
}
