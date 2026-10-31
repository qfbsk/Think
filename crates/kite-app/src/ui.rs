use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glyphon::cosmic_text::{
    Attrs, AttrsList, Buffer, BufferLine, Color, LineEnding, Metrics, Shaping, Weight, Wrap,
};
use glyphon::{FontSystem, TextArea, TextBounds};
use kite_core::{Document, Position, Selection, file_key};
use winit::dpi::PhysicalSize;
use winit::keyboard::KeyCode;

use crate::activity;
use crate::complete::{Completer, ExtraEdit, Item, Style, Target};
use crate::day::Day;
use crate::editor::Editor;
use crate::lsp::{self, Asking, Page, Servers};
use crate::miku::Miku;
use crate::palette::Palette; // engineering only, remove whole
use crate::renderer::QuadVertex;
use crate::shell::{Shell, ShellEffect};
use crate::sidebar::{Sidebar, SidebarAction};
use crate::sparks::Sparks;
use crate::syntax::Engine;
use crate::text::{attrs, make_label, make_paragraph, measure, measure_block};
use crate::theme;
use crate::usage::Usage;
use crate::visualizer::{STRIP_HEIGHT, STRIP_MIN_HEIGHT, Visualizer};

/// The sidebar's text size, before the window scale. It is set here once so the
/// initial build and every later resize agree on one number.
const SIDEBAR_FONT_SIZE: f32 = 16.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Right,
    Sidebar,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn inset(&self, amount: f32) -> Rect {
        Rect {
            x: self.x + amount,
            y: self.y + amount,
            w: (self.w - amount * 2.0).max(1.0),
            h: (self.h - amount * 2.0).max(1.0),
        }
    }

    pub fn clip_x(&self, left: f32, right: f32) -> Rect {
        let clipped_left = self.x.max(left);
        let clipped_right = self.right().min(right);
        Rect {
            x: clipped_left,
            y: self.y,
            w: (clipped_right - clipped_left).max(0.0),
            h: self.h,
        }
    }
}

pub enum TabContent {
    Shell(Shell),
    Editor(Editor),
    Miku(Miku),
}

pub struct Tab {
    pub content: TabContent,
    pub title: String,
    pub label: Buffer,
    pub label_width: f32,
    pub desired_width: f32,
    /// The `+added -removed` the label was built with, so the counter only
    /// reshapes when it actually changes.
    label_delta: (usize, usize),
    pub rect: Rect,
    pub close_rect: Option<Rect>,
}

impl Tab {
    pub fn is_shell(&self) -> bool {
        matches!(self.content, TabContent::Shell(_))
    }
}

/// The drawn caret chasing the logical one. Every move re-targets and the
/// drawn spot eases toward it — τ = 30 ms, so ~90 ms to cover 95% of the
/// distance; a held key (~32 ms per repeat) re-targets mid-flight and the
/// caret flows instead of stepping. Jumps past `SNAP` (a click across the
/// document, a tab switch) teleport: a long glide there reads as lag, not
/// as motion. Only the drawing moves; hit-testing and text stay on the
/// logical caret.
///
/// A glide is a reaction to the *caret* moving, so the logical spot is part of
/// the state: when only the view moved — scrolling, or scrolling the caret out
/// of sight — the caret belongs to the text and travels with it. Easing there
/// would read as a trail laid down by a caret nobody touched.
struct CaretState {
    x: f32,
    y: f32,
    flying: bool,
    had_target: bool,
    target: (f32, f32),
    spot: Option<(isize, isize)>,
    last_tick: Instant,
}

impl Default for CaretState {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            flying: false,
            had_target: false,
            target: (0.0, 0.0),
            spot: None,
            last_tick: Instant::now(),
        }
    }
}

/// Physical pixels a retarget may cover as a glide; past it, snap.
const CARET_SNAP: f32 = 400.0;
/// Below this distance to the target the glide counts as landed.
const CARET_LANDED: f32 = 0.4;
const CARET_TAU: f32 = 0.030;

/// One exponential-chase step: cover the fraction `1 - e^(-dt/τ)` of the
/// remaining distance from `cur` to `target`. τ = 30 ms, so a single 90 ms
/// frame closes ~95% of the gap and the caret reads as gliding, not stepping.
fn caret_step(cur: f32, target: f32, dt: f32) -> f32 {
    let alpha = 1.0 - (-dt / CARET_TAU).exp();
    cur + (target - cur) * alpha
}

/// A question the caret asked a server, kept until either it is answered or the
/// caret moves away from where it was asked. The caret and the document revision
/// are recorded so a late answer can be recognised as late: one that arrives
/// after the reader has moved on describes a place nobody is looking at, and
/// putting it on the screen would offer a finish to a word no longer there.
/// The card that says what the trouble under the mouse is, already shaped: a
/// text area has to outlive the frame that measured it.
struct Hover {
    text: Buffer,
    x: f32,
    y: f32,
}

struct Awaiting {
    id: u64,
    /// The line the question was asked on. An answer is only ever placed where
    /// it can still be read: a caret on another line is asking something else.
    line: usize,
}

/// The answer on screen, as far as the caret is concerned: the span of one line
/// it speaks for, and the word that was typed in that span when it landed. While
/// the caret stays inside it and the text there only grows, this answer is still
/// the answer — narrowing it locally costs nothing and keeps up with the keys,
/// which is the whole reason a list is worth having.
struct Answered {
    line: usize,
    start: usize,
    word: String,
    /// The server said its list was cut short. Then the next letter is worth a
    /// second question, because the names it did not send cannot be filtered out
    /// of what it did.
    truncated: bool,
}

/// Where a keystroke asks, as far as an answer can be placed without having one.
enum Where {
    /// The shell keeps its own view of the command line, so it answers in the
    /// same breath as the key that asked.
    Shell {
        target: Target,
        word: String,
        style: Style,
    },
    /// A file has to ask somebody else, and only the place is known yet. The
    /// path is here because the server that answers is chosen by it.
    File {
        path: std::path::PathBuf,
        language: &'static str,
        line: usize,
        column: usize,
        revision: u64,
        caret_line: String,
        style: Style,
    },
}

pub struct Ui {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub mode: Mode,
    pub focus: Focus,
    pub sidebar: Sidebar,
    pub size: PhysicalSize<u32>,
    pub scale: f32,
    pub body_font_size: f32,
    pub cursor_blink: bool,
    caret: CaretState,
    pub show_whitespace: bool,
    pub topmost: bool,
    pub message: Option<String>,
    family: &'static str,
    engine: Arc<Engine>,

    logo: Buffer,
    logo_width: f32,
    status: Buffer,
    status_key: String,
    status_right: Buffer,
    status_right_w: f32,

    titlebar_h: f32,
    status_h: f32,
    tab_pad: f32,
    tab_gap: f32,
    close_w: f32,
    body_pad: f32,
    sidebar_w: f32,
    right: Rect,
    /// The right pane as a styled region: body plus visualizer band, one
    /// background, one focus state. The band is its content, not a region.
    right_pane: Rect,
    sidebar_rect: Rect,
    status_rect: Rect,
    strip_rect: Rect,
    visualizer: Visualizer,
    sparks: Sparks,
    /// engineering palette: remove this field and its uses to rip the feature out
    pub palette: Palette,
    /// Where saves are tallied for the board. Held, not re-read per save, so a
    /// test can point the history at a temporary file.
    activity_log: Option<PathBuf>,
    /// The list of ways to finish what is being typed, if there is one up.
    completer: Completer,
    /// The names this machine's code actually holds, kept by the language
    /// servers this machine runs.
    servers: Servers,
    /// The names the reader has taken from a list before, by language.
    usage: Usage,
    /// A question posted to a server that has not answered yet.
    awaiting: Option<Awaiting>,
    /// The answer the list on screen is standing for, so the next letter can be
    /// filtered out of it instead of asking the server again.
    answered: Option<Answered>,
    /// Which server report the open pages are showing. A server tells when it
    /// likes, so this is the only way to notice it has said anything.
    seen_generation: u64,
    /// The card saying what the trouble under the mouse is, already shaped: a
    /// text area has to outlive the frame that measured it, so it is built while
    /// the quads are and drawn from there.
    hover: Option<Hover>,
    window_close: Rect,
    tab_strip: Rect,
    tab_scroll: f32,
    tab_overflow: bool,
    tab_font_size: f32,
    tab_line_height: f32,
    status_font_size: f32,
    status_line_height: f32,

    mouse_x: f32,
    mouse_y: f32,
}

impl Ui {
    pub fn new(
        font_system: &mut FontSystem,
        family: &'static str,
        size: PhysicalSize<u32>,
        scale: f32,
    ) -> Self {
        let scale = scale.max(0.5);
        let titlebar_h = 40.0 * scale;
        let status_h = 26.0 * scale;
        let body_pad = 10.0 * scale;
        let sidebar_w = 280.0 * scale;
        let tab_font_size = 13.0 * scale;
        let tab_line_height = (tab_font_size * 1.35).ceil();
        let status_font_size = 12.0 * scale;
        let status_line_height = (status_font_size * 1.3).ceil();
        let body_font_size = theme::FONT_SIZE_BASE * scale;

        let body_height = (size.height as f32
            - titlebar_h
            - status_h
            - strip_height(scale, size.height as f32 / scale))
        .max(1.0);
        let editor_width = (size.width as f32 - body_pad * 2.0).max(1.0);
        let editor_height = (body_height - body_pad * 2.0).max(1.0);

        let shell = Shell::new(
            font_system,
            family,
            body_font_size,
            editor_width,
            editor_height,
        );
        let sidebar = Sidebar::new(
            font_system,
            family,
            SIDEBAR_FONT_SIZE * scale,
            sidebar_w - body_pad * 2.0,
            body_height - body_pad * 2.0,
        );

        let logo_attrs = Attrs::new()
            .family(glyphon::cosmic_text::Family::Name(family))
            .color(theme::accent())
            .weight(Weight::BOLD);
        let logo = make_label(font_system, &logo_attrs, 15.0 * scale, "kite");
        let (logo_width, _) = measure(&logo);
        let status_attrs = attrs(family, theme::text_dim());
        let status = make_label(font_system, &status_attrs, status_font_size, "");
        let count_attrs = attrs(family, theme::status_text());
        let status_right = make_label(font_system, &count_attrs, status_font_size, "");
        let engine = Arc::new(Engine::new());

        let shell_tab = make_tab(
            font_system,
            family,
            tab_font_size,
            11.0 * scale,
            18.0 * scale,
            4.0 * scale,
            "shell",
            TabContent::Shell(shell),
            true,
        );
        let mut ui = Self {
            tabs: vec![shell_tab],
            active: 0,
            // A window that opens ready for `i` rather than mid-typing, and that
            // shows the shell instead of a file tree nobody asked for.
            mode: Mode::Normal,
            focus: Focus::Right,
            sidebar,
            size,
            scale,
            body_font_size,
            cursor_blink: true,
            caret: CaretState::default(),
            show_whitespace: false,
            topmost: false,
            message: None,
            family,
            engine,
            logo,
            logo_width,
            status,
            status_key: String::new(),
            status_right,
            status_right_w: 0.0,
            titlebar_h,
            status_h,
            tab_pad: 11.0 * scale,
            tab_gap: 4.0 * scale,
            close_w: 18.0 * scale,
            body_pad,
            sidebar_w,
            right: Rect::default(),
            right_pane: Rect::default(),
            sidebar_rect: Rect::default(),
            status_rect: Rect::default(),
            strip_rect: Rect::default(),
            visualizer: Visualizer::new(),
            sparks: Sparks::new(),
            palette: Palette::new(font_system, family, scale),
            activity_log: activity::log_path(),
            completer: Completer::new(family, scale),
            servers: Servers::new(),
            usage: Usage::store(),
            awaiting: None,
            answered: None,
            seen_generation: 0,
            hover: None,
            window_close: Rect::default(),
            tab_strip: Rect::default(),
            tab_scroll: 0.0,
            tab_overflow: false,
            tab_font_size,
            tab_line_height,
            status_font_size,
            status_line_height,
            mouse_x: 0.0,
            mouse_y: 0.0,
        };
        ui.recompute_layout(font_system);
        for index in 0..ui.tabs.len() {
            ui.refresh_label(font_system, index);
        }
        ui.recompute_layout(font_system);
        ui.refresh_status(font_system);
        ui
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn active_editor(&self) -> Option<&Editor> {
        match &self.tabs[self.active].content {
            TabContent::Editor(editor) => Some(editor),
            _ => None,
        }
    }

    pub fn active_editor_mut(&mut self) -> Option<&mut Editor> {
        match &mut self.tabs[self.active].content {
            TabContent::Editor(editor) => Some(editor),
            _ => None,
        }
    }

    pub fn active_shell(&self) -> Option<&Shell> {
        match &self.tabs[self.active].content {
            TabContent::Shell(shell) => Some(shell),
            _ => None,
        }
    }

    pub fn active_shell_mut(&mut self) -> Option<&mut Shell> {
        match &mut self.tabs[self.active].content {
            TabContent::Shell(shell) => Some(shell),
            _ => None,
        }
    }

    pub fn shell_tab_mut(&mut self) -> Option<&mut Shell> {
        self.tabs.iter_mut().find_map(|tab| match &mut tab.content {
            TabContent::Shell(shell) => Some(shell),
            _ => None,
        })
    }

    pub fn active_miku(&self) -> Option<&Miku> {
        match &self.tabs[self.active].content {
            TabContent::Miku(miku) => Some(miku),
            _ => None,
        }
    }

    pub fn active_miku_mut(&mut self) -> Option<&mut Miku> {
        match &mut self.tabs[self.active].content {
            TabContent::Miku(miku) => Some(miku),
            _ => None,
        }
    }

    /// The board's index, or `None` when it is not open. There is never a second one.
    pub fn miku_tab(&self) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| matches!(tab.content, TabContent::Miku(_)))
    }

    /// Focus is an input to both panes' styles, so a flip re-bakes every
    /// buffer that painted colors into its glyphs. Equal slots on the two
    /// sides make the flip an exact appearance swap; nothing here knows
    /// anything about swapping.
    fn set_focus(&mut self, font_system: &mut FontSystem, focus: Focus) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        self.retheme(font_system);
    }

    /// Open the board, or move to the one already open. It always sits at index
    /// one, right after the shell, so the file tabs shift along with it.
    pub fn open_miku(&mut self, font_system: &mut FontSystem) {
        if let Some(index) = self.miku_tab() {
            self.activate_tab(font_system, index);
            self.set_focus(font_system, Focus::Right);
            self.mode = Mode::Normal;
            self.recompute_layout(font_system);
            return;
        }
        let content = self.editor_content_rect();
        let mut miku = Miku::new(font_system, self.family, self.scale);
        miku.set_viewport(font_system, content.w, content.h);
        let tab = make_tab(
            font_system,
            self.family,
            self.tab_font_size,
            self.tab_pad,
            self.close_w,
            4.0 * self.scale,
            "miku",
            TabContent::Miku(miku),
            true,
        );
        self.tabs.insert(1, tab);
        self.active = 1;
        self.set_focus(font_system, Focus::Right);
        self.mode = Mode::Normal;
        for index in 1..self.tabs.len() {
            self.refresh_label(font_system, index);
        }
        self.recompute_layout(font_system);
    }

    pub fn editor_content_rect(&self) -> Rect {
        self.right.inset(self.body_pad)
    }

    /// Body text area, inset by the line-number gutter on file tabs.
    pub fn body_text_rect(&self) -> Rect {
        let content = self.editor_content_rect();
        match &self.active_tab().content {
            TabContent::Editor(editor) => {
                let gutter = editor.gutter_width();
                Rect {
                    x: content.x + gutter,
                    y: content.y,
                    w: (content.w - gutter).max(1.0),
                    h: content.h,
                }
            }
            TabContent::Shell(_) => content,
            TabContent::Miku(_) => content,
        }
    }

    pub fn sidebar_content_rect(&self) -> Rect {
        Rect {
            x: self.sidebar_rect.x + self.body_pad * 0.8,
            y: self.sidebar_rect.y + self.body_pad * 0.4,
            w: (self.sidebar_rect.w - self.body_pad * 1.6).max(1.0),
            h: (self.sidebar_rect.h - self.body_pad * 0.8).max(1.0),
        }
    }

    pub fn resize(&mut self, font_system: &mut FontSystem, size: PhysicalSize<u32>) {
        if size.width == self.size.width && size.height == self.size.height {
            return;
        }
        self.size = size;
        self.recompute_layout(font_system);
    }

    pub fn set_scale(&mut self, font_system: &mut FontSystem, scale: f32, size: PhysicalSize<u32>) {
        let scale = scale.max(0.5);
        self.scale = scale;
        self.size = size;
        self.titlebar_h = 40.0 * scale;
        self.status_h = 26.0 * scale;
        self.body_pad = 10.0 * scale;
        self.sidebar_w = 280.0 * scale;
        self.tab_pad = 11.0 * scale;
        self.tab_gap = 4.0 * scale;
        self.close_w = 18.0 * scale;
        self.tab_font_size = 13.0 * scale;
        self.tab_line_height = (self.tab_font_size * 1.35).ceil();
        self.status_font_size = 12.0 * scale;
        self.status_line_height = (self.status_font_size * 1.3).ceil();
        self.body_font_size = theme::FONT_SIZE_BASE * scale;

        let logo_attrs = Attrs::new()
            .family(glyphon::cosmic_text::Family::Name(self.family))
            .color(theme::accent())
            .weight(Weight::BOLD);
        self.logo = make_label(font_system, &logo_attrs, 15.0 * scale, "kite");
        self.logo_width = measure(&self.logo).0;
        self.status = make_label(
            font_system,
            &attrs(self.family, theme::text_dim()),
            self.status_font_size,
            "",
        );
        self.status_key.clear();
        self.refresh_status(font_system);
        for index in 0..self.tabs.len() {
            self.refresh_label(font_system, index);
        }
        self.sidebar
            .set_font_size(font_system, SIDEBAR_FONT_SIZE * scale);
        let editor_size = self.editor_font_size();
        for tab in &mut self.tabs {
            match &mut tab.content {
                TabContent::Shell(shell) => shell.set_font_size(font_system, self.body_font_size),
                TabContent::Editor(editor) => editor.set_font_size(font_system, editor_size),
                // The board's type comes from its own geometry, which the new
                // scale reaches through the layout below.
                TabContent::Miku(_) => {}
            }
        }
        self.recompute_layout(font_system);
    }

    pub fn toggle_sidebar(&mut self, font_system: &mut FontSystem) {
        if self.sidebar.visible {
            self.sidebar.visible = false;
            self.set_focus(font_system, Focus::Right);
            self.mode = Mode::Normal;
            self.recompute_layout(font_system);
        } else {
            self.sidebar.visible = true;
            self.set_focus(font_system, Focus::Sidebar);
            self.mode = Mode::Normal;
            self.recompute_layout(font_system);
            // Reopening is the moment to ask git again; the viewport size has not
            // changed, so nothing else would rebuild the listing.
            self.sidebar.git_changed(font_system);
            self.sidebar.shape(font_system);
        }
    }

    pub fn focus_right_insert(&mut self, font_system: &mut FontSystem) {
        self.set_focus(font_system, Focus::Right);
        self.mode = Mode::Insert;
        if let Some(editor) = self.active_editor_mut() {
            editor.ensure_cursor_visible(font_system);
        }
    }

    pub fn focus_sidebar(&mut self, font_system: &mut FontSystem) {
        if self.sidebar.visible {
            self.set_focus(font_system, Focus::Sidebar);
            self.mode = Mode::Normal;
            self.sidebar.shape(font_system);
        }
    }

    pub fn escape(&mut self, font_system: &mut FontSystem) {
        // Escape first means "I am not choosing from that list". Only with the
        // list gone does it mean "I have finished typing".
        self.completer.close();
        if self.mode == Mode::Insert {
            self.mode = Mode::Normal;
            let focus = if self.sidebar.visible {
                Focus::Sidebar
            } else {
                Focus::Right
            };
            self.set_focus(font_system, focus);
            if self.focus == Focus::Sidebar {
                self.sidebar.shape(font_system);
            } else if let Some(editor) = self.active_editor_mut() {
                editor.ensure_cursor_visible(font_system);
            }
        }
    }

    pub fn activate_tab(&mut self, font_system: &mut FontSystem, index: usize) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        // The list offered a word in the page being left, and that page is no
        // longer where the caret is.
        self.completer.close();
        self.awaiting = None;
        self.answered = None;
        let previous = self.active;
        self.active = index;
        self.message = None;
        self.refresh_label(font_system, previous);
        self.refresh_label(font_system, index);
        self.recompute_layout(font_system);
    }

    /// `1` is the shell and every other tab takes the next slot in order, so the
    /// board is always `2` while it exists. `0` is the tenth tab.
    pub fn activate_digit(&mut self, font_system: &mut FontSystem, digit: u8) -> bool {
        let slot = usize::from(if digit == 0 { 10 } else { digit });
        let index = slot - 1;
        if index >= self.tabs.len() {
            return false;
        }
        if self.active != index {
            self.activate_tab(font_system, index);
        }
        true
    }

    pub fn close_tab(&mut self, font_system: &mut FontSystem, index: usize) -> bool {
        if index == 0 || index >= self.tabs.len() {
            return false;
        }
        let was_active = index == self.active;
        // A server is hundreds of megabytes of somebody's index, so it goes when
        // the last page that needed it closes rather than when one does.
        if let TabContent::Editor(editor) = &self.tabs[index].content {
            let path = editor.document.path().to_path_buf();
            let Ok(key) = file_key(&path) else { return false };
            let language = editor.language_name();
            let still_open = self.tabs.iter().enumerate().any(|(other, tab)| {
                other != index
                    && matches!(&tab.content, TabContent::Editor(open)
                        if file_key(open.document.path()).ok().as_ref() == Some(&key))
            });
            if let Some(language) = language {
                self.servers.forget(&path, language, still_open);
            }
        }
        self.tabs.remove(index);
        self.completer.close();
        self.awaiting = None;
        self.answered = None;
        self.message = None;
        if index < self.active {
            self.active -= 1;
        } else if was_active {
            if index < self.tabs.len() {
                self.active = index;
            } else {
                self.active = index - 1;
            }
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        for index in 0..self.tabs.len() {
            self.refresh_label(font_system, index);
        }
        self.recompute_layout(font_system);
        if let Some(editor) = self.active_editor_mut() {
            editor.ensure_cursor_visible(font_system);
        }
        true
    }

    pub fn close_active_file(&mut self, font_system: &mut FontSystem) -> bool {
        // Who may ask is the caller's business; the tree holds the focus just as
        // well as the page does. What never closes is the shell tab.
        if self.active_tab().is_shell() {
            return false;
        }
        self.close_tab(font_system, self.active)
    }

    pub fn open_file(&mut self, font_system: &mut FontSystem, path: PathBuf) -> Result<(), String> {
        let key = file_key(&path).map_err(|error| error.to_string())?;
        for (index, tab) in self.tabs.iter().enumerate() {
            if let TabContent::Editor(editor) = &tab.content {
                if file_key(editor.document.path()).ok().as_ref() == Some(&key) {
                    self.activate_tab(font_system, index);
                    self.focus_right_insert(font_system);
                    return Ok(());
                }
            }
        }

        let document = Document::open(&path).map_err(|error| error.to_string())?;
        let content = self.editor_content_rect();
        let editor = Editor::new(
            document,
            font_system,
            self.family,
            content.w,
            content.h,
            self.editor_font_size(),
            self.engine.clone(),
        );
        let title = editor.display_title();
        let tab = make_tab(
            font_system,
            self.family,
            self.tab_font_size,
            self.tab_pad,
            self.close_w,
            4.0 * self.scale,
            &title,
            TabContent::Editor(editor),
            true,
        );
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.awaiting = None;
        self.answered = None;
        self.set_focus(font_system, Focus::Right);
        self.mode = Mode::Insert;
        self.message = Some(format!("opened {title}"));
        self.refresh_label(font_system, self.active);
        self.recompute_layout(font_system);
        Ok(())
    }

    pub fn save_active(&mut self, font_system: &mut FontSystem) -> Result<(), String> {
        let index = self.active;
        let (title, path) = match &mut self.tabs[index].content {
            TabContent::Editor(editor) => {
                let title = editor.display_title();
                let path = editor.document.path().to_path_buf();
                editor.save()?;
                (title, path)
            }
            TabContent::Shell(_) => return Err("the shell has nothing to save".to_string()),
            TabContent::Miku(_) => return Err("the board has nothing to save".to_string()),
        };
        self.refresh_label(font_system, index);
        self.message = Some(format!("saved {title}"));
        self.sidebar.git_changed(font_system);
        self.note_save(font_system, &path);
        Ok(())
    }

    /// The board counts saves, so a save is worth recording even when the board
    /// is not the page in front — and even when no board has ever been opened.
    /// The log is the history; a live board is only told about the line it just
    /// gained, because its reader stopped at the end of the file long ago.
    fn note_save(&mut self, font_system: &mut FontSystem, path: &Path) {
        let text = kite_core::display_path(path);
        activity::record_save(self.activity_log.as_deref(), Day::today(), &text);
        for tab in &mut self.tabs {
            if let TabContent::Miku(miku) = &mut tab.content {
                miku.note_save(font_system, &text);
            }
        }
    }

    /// Shift and the wheel turn whole years; the plain arrows move one day.
    pub fn miku_page(&mut self, font_system: &mut FontSystem, back: bool) {
        let Some(miku) = self.active_miku_mut() else {
            return;
        };
        miku.page(font_system, if back { 1 } else { -1 });
    }

    /// A click on the board picks the day under it; anywhere else it is a no-op.
    pub fn miku_click(&mut self, font_system: &mut FontSystem, x: f32, y: f32) {
        let content = self.editor_content_rect();
        let Some(day) = self
            .active_miku()
            .and_then(|miku| miku.hit(x - content.x, y - content.y))
        else {
            return;
        };
        self.set_focus(font_system, Focus::Right);
        self.mode = Mode::Normal;
        if let Some(miku) = self.active_miku_mut() {
            miku.select(font_system, day);
        }
    }

    pub fn miku_pending(&self) -> bool {
        self.active_miku().is_some_and(Miku::pending)
    }

    pub fn poll_miku(&mut self, font_system: &mut FontSystem) -> bool {
        self.active_miku_mut()
            .is_some_and(|miku| miku.poll(font_system))
    }

    /// Whether the completion list is up and holding keys.
    pub fn completing(&self) -> bool {
        self.completer.is_open()
    }

    pub fn close_completion(&mut self) {
        self.completer.close();
    }

    /// Whether a taken snippet still has placeholders to walk. Only a file
    /// page can hold one, so only a file page can answer yes.
    pub fn snippet_pending(&self) -> bool {
        self.active_editor().is_some_and(Editor::snippet_pending)
    }

    /// Tab through a taken snippet. Reports false when there is none, so Tab
    /// goes on being Tab.
    pub fn snippet_tab(&mut self, font_system: &mut FontSystem) -> bool {
        self.active_editor_mut()
            .is_some_and(|editor| editor.snippet_tab(font_system))
    }

    pub fn end_snippet(&mut self) {
        if let Some(editor) = self.active_editor_mut() {
            editor.clear_snippet();
        }
    }

    /// Walk the highlight one row, `delta` being ±1.
    pub fn complete_move(&mut self, delta: i32) {
        self.completer.move_by(delta);
    }

    /// A click inside the list chooses that row and finishes. Anywhere else it
    /// reports false, so the click goes on being a click.
    pub fn completion_press(&mut self, font_system: &mut FontSystem, x: f32, y: f32) -> bool {
        if !self.completer.over(x, y) {
            return false;
        }
        self.completer.pick_at(x, y);
        self.accept_completion(font_system);
        true
    }

    /// The caret's place on the page: where a ghost would be drawn, how far the
    /// row behind it has been typed, and which revision that text is. Cheap
    /// enough to ask after every keystroke, which is the point — an answer that
    /// keeps the place it first appeared in walks backwards over every letter
    /// typed since.
    fn completion_where(&mut self) -> Option<Where> {
        // The page's own left edge, which for a file is behind the line numbers.
        // Reading the caret from one rect and drawing the answer from another is
        // how the faint text ended up written on top of the real text.
        let body = self.body_text_rect();
        let scale = self.scale;
        let family = self.family;
        match &mut self.tabs[self.active].content {
            TabContent::Shell(shell) => {
                // A child that owns the screen owns the keys with it; nothing
                // kite could offer would be what that program is waiting for.
                if shell.is_running() {
                    return None;
                }
                let (start, word) = shell.word_before_caret()?;
                let (x, y, height) = shell.cursor_rect()?;
                Some(Where::Shell {
                    target: Target::Shell { start },
                    word,
                    style: Style {
                        origin: (body.x + x, body.y + y),
                        line_height: height,
                        family,
                        scale,
                    },
                })
            }
            TabContent::Editor(editor) => {
                // A file with no server is a file with no completion. Saying so
                // by saying nothing is the whole of the policy.
                let language = editor.language_name()?;
                let caret = editor.selection.cursor;
                let (x, y, height) = editor.cursor_rect()?;
                let caret_line = editor.document.line_text(caret.line);
                Some(Where::File {
                    path: editor.document.path().to_path_buf(),
                    language,
                    line: caret.line,
                    column: caret.ch,
                    revision: editor.document.revision(),
                    caret_line,
                    style: Style {
                        origin: (body.x + x, body.y + y),
                        line_height: height,
                        family,
                        scale,
                    },
                })
            }
            TabContent::Miku(_) => None,
        }
    }

    /// The shell's own view of the word being typed. Only it can supply this: the
    /// running child has a working folder and a history that kite cannot see.
    fn shell_pool(&mut self) -> Vec<Item> {
        if let TabContent::Shell(shell) = &mut self.tabs[self.active].content {
            return shell.completions();
        }
        Vec::new()
    }

    /// Post the question. Never waits, never draws: an answer is put on the
    /// screen by [`Ui::poll_lsp`] some milliseconds later, and by then the caret
    /// may have moved on, which is fine — the answer is a pool of names for this
    /// span of the line, not a one-shot ghost for one column.
    fn ask(
        &mut self,
        language: &'static str,
        line: usize,
        column: usize,
        revision: u64,
        caret_line: &str,
    ) {
        let (path, text) = match &self.tabs[self.active].content {
            TabContent::Editor(editor) => (
                editor.document.path().to_path_buf(),
                String::from_utf8_lossy(&editor.document.to_bytes()).into_owned(),
            ),
            _ => return,
        };
        let behind = column.checked_sub(1).and_then(|at| caret_line.chars().nth(at));
        let Some(id) = self.servers.ask(
            &Page {
                path: &path,
                language,
                text: &text,
                revision,
            },
            &Asking {
                caret_line,
                line,
                column,
                trigger: behind,
            },
        ) else {
            return;
        };
        self.awaiting = Some(Awaiting { id, line });
    }

    /// The answer that has come in since the last key, put on the screen. Same
    /// contract as the sidebar and the band: true means something changed.
    ///
    /// An answer is not thrown away because the caret moved on while it was in
    /// flight: it is a list of the names that fit the span it was asked for, and
    /// letters typed since then only narrow that list. It is filtered against
    /// what is written there now, which is what makes a fast typist see a list
    /// at all. A caret on another line is a different question, and that one is
    /// dropped.
    pub fn poll_lsp(&mut self) -> bool {
        let Some(pending) = &self.awaiting else {
            return false;
        };
        let (id, line) = (pending.id, pending.line);
        let Some(Where::File {
            language,
            caret_line,
            line: here,
            column,
            style,
            ..
        }) = self.completion_where()
        else {
            self.awaiting = None;
            return false;
        };
        if here != line {
            self.awaiting = None;
            return false;
        }
        let Some(answer) = self.servers.take(id, &caret_line, column) else {
            return false;
        };
        self.awaiting = None;
        let start = answer.start.min(column);
        let end = answer.end.max(column).min(caret_line.chars().count());
        let word: String = caret_line
            .chars()
            .skip(start)
            .take(column.saturating_sub(start))
            .collect();
        self.answered = Some(Answered {
            line,
            start,
            word: word.clone(),
            truncated: answer.incomplete,
        });
        let target = Target::Editor {
            start: Position::new(line, start),
            end: Position::new(line, end),
        };
        let mut items = answer.items;
        // `#include "` with nothing typed after it is a question about this
        // project's own headers; clangd answers it with every header on the
        // search path — the Windows SDK, alphabetically — so the ones that are
        // not files in this folder are dropped. Type a letter, or use the angle
        // bracket, and the whole search path is back: there the reader is asking
        // for a system header by name.
        self.usage.rank(language, &mut items);
        self.completer
            .show(&style, items, &word, target);
        true
    }

    /// Whether the page on screen is one a server was started for. The reports
    /// arrive with no question of ours outstanding, so this is what tells the
    /// loop to keep waking up to look for them.
    pub fn server_backed(&self) -> bool {
        self.active_editor()
            .is_some_and(|editor| editor.language_name().is_some())
    }

    /// Whether a server owes the caret an answer.
    pub fn lsp_pending(&self) -> bool {
        self.awaiting.is_some()
    }

    /// Keep the page on screen in front of its server.
    ///
    /// The words go over the wire whenever they are not already there, whether or
    /// not anything is asked of the server. A report about what is wrong with a
    /// file is worthless if it is about the text that was there two keystrokes
    /// ago — the line lands in the wrong place and the reader learns to ignore
    /// it — and completion is not the only thing a server is good for.
    pub fn sync_servers(&mut self) {
        let Some((path, language, revision)) = self.server_page() else {
            return;
        };
        if self.servers.holds(&path, language, revision) {
            return;
        }
        let text = match &self.tabs[self.active].content {
            TabContent::Editor(editor) => {
                String::from_utf8_lossy(&editor.document.to_bytes()).into_owned()
            }
            _ => return,
        };
        self.servers.sync(&Page {
            path: &path,
            language,
            text: &text,
            revision,
        });
    }

    /// The page a server would be told about, without its text: the one on
    /// screen, when its language has a server at all.
    fn server_page(&self) -> Option<(PathBuf, &'static str, u64)> {
        let TabContent::Editor(editor) = &self.tabs[self.active].content else {
            return None;
        };
        let language = editor.language_name()?;
        Some((
            editor.document.path().to_path_buf(),
            language,
            editor.document.revision(),
        ))
    }

    /// What the servers have said about the open pages since the last look.
    ///
    /// A report arrives when the server gets round to it, which is not when the
    /// reader moves the mouse, so this is asked on every frame: the generation
    /// counter is the only cheap way to know whether anything was told. Every
    /// page is offered its own report, not just the one on screen, because a
    /// tab switched to is a tab that should already be showing what is wrong
    /// with it.
    pub fn poll_diagnostics(&mut self) -> bool {
        let generation = self.servers.generation();
        if generation == self.seen_generation {
            return false;
        }
        self.seen_generation = generation;
        let mut changed = false;
        for tab in &mut self.tabs {
            let TabContent::Editor(editor) = &mut tab.content else {
                continue;
            };
            let found = self.servers.diagnostics(editor.document.path());
            changed |= editor.set_issues(&found);
        }
        changed
    }

    /// The report the mouse is sitting on, as the words to show, where to open
    /// the card, and how bad it is. A hover rather than a click or a key: the
    /// reader is already looking at the red line and wants to know what it says
    /// without moving the caret off his place.
    fn hovered_issue(&self) -> Option<(String, f32, f32, bool)> {
        let TabContent::Editor(editor) = &self.tabs[self.active].content else {
            return None;
        };
        // The wave is painted inside the gutter, and so is the mouse that asks
        // about it: the rect that placed the ink is the rect that must take the
        // hit, or a card opens for a mouse over the line numbers.
        let content = self.body_text_rect();
        let (index, x, top, width, height, _) = editor.issue_spans().into_iter().find(|span| {
            self.mouse_x >= content.x + span.1
                && self.mouse_x < content.x + span.1 + span.3
                && self.mouse_y >= content.y + span.2
                && self.mouse_y < content.y + span.2 + span.4
        })?;
        let issue = editor.issue(index)?;
        let words = match issue.note.is_empty() {
            true => issue.message.clone(),
            false => format!("{}  ({})", issue.message, issue.note),
        };
        // One report per card. A message long enough to outgrow the pane would be
        // a second page, and the trouble is always in its first sentence.
        let words: String = words.chars().take(400).collect();
        Some((
            words,
            content.x + x + width.min(12.0),
            content.y + top + height,
            issue.error,
        ))
    }

    /// The answer that follows typing. A page gets a list under the word, opened
    /// by the keystroke that started it; the shell gets the best candidate
    /// written faintly on the rest of the row, because a menu there would cover
    /// the line being typed into. Either way it puts itself away as soon as the
    /// caret leaves the word it was offered for, and an answer already on screen
    /// narrows rather than being rebuilt from under the reader.
    pub fn update_completion(&mut self) {
        let Some(spot) = self.completion_where() else {
            self.completer.close();
            self.awaiting = None;
            return;
        };
        match spot {
            Where::Shell {
                target,
                word,
                style,
            } => {
                // Nothing typed is nothing finished. A bare prompt is a question
                // nobody has asked yet, and a word offered there is a guess at a
                // command the reader never meant to write.
                if word.is_empty() {
                    self.completer.close();
                    return;
                }
                if self.completer.is_open() && self.completer.target() == target {
                    if self.completer.refine(&style, &word) {
                        return;
                    }
                }
                self.completer.close();
                let mut pool = self.shell_pool();
                self.usage.rank("shell", &mut pool);
                // The same list the file page gets. It was kept off the shell on
                // the grounds that a menu covers the line being typed into, and
                // the answer is that the list already knows to open above when
                // there is no room below — the file page has never covered the
                // text it is completing either.
                self.completer.show(&style, pool, &word, target);
            }
            Where::File {
                path,
                language,
                line,
                column,
                revision,
                caret_line,
                style,
            } => {
                let typed = lsp::name_before(&caret_line, column);
                let behind = column.checked_sub(1).and_then(|at| caret_line.chars().nth(at));
                // A question is worth posting where the caret is inside a name,
                // or stands just behind a character the server named as one it
                // answers to. Both are the server's own vocabulary: nothing here
                // decides what punctuation means.
                if typed.is_empty() && !self.servers.declares(&path, language, behind) {
                    self.completer.close();
                    self.answered = None;
                    self.awaiting = None;
                    return;
                }
                // The list on screen speaks for a span of this line. While the
                // caret is still past the start of that span and what is written
                // there is the word it was asked about with letters added, the
                // answer it holds is still the answer: narrow it here, in the
                // same breath as the key, instead of waiting on a server that was
                // already asked.
                let known = self.answered.as_ref().filter(|known| {
                    known.line == line && column >= known.start
                });
                if let Some(known) = known {
                    let word: String = caret_line
                        .chars()
                        .skip(known.start)
                        .take(column - known.start)
                        .collect();
                    if word.starts_with(&known.word) {
                        let truncated = known.truncated;
                        if self.completer.refine(&style, &word) {
                            // A server that cut its list short knows names this
                            // pool has never seen, so keep asking while the word
                            // grows — but never stack a second question on one
                            // already in flight.
                            if truncated && self.awaiting.is_none() {
                                self.ask(language, line, column, revision, &caret_line);
                            }
                            return;
                        }
                        self.answered = None;
                    }
                }
                self.completer.close();
                self.ask(language, line, column, revision, &caret_line);
            }
        }
    }

    /// Wave the faint answer off. It stays out of the way while the word it was
    /// offered for is still the word under the caret.
    pub fn dismiss_completion(&mut self) {
        self.completer.dismiss();
    }

    /// Put the chosen candidate where the half-word was. The list has already
    /// closed by the time it hands the text over; only the range it was
    /// replacing is left, and only its owner knows how to splice into it.
    pub fn accept_completion(&mut self, font_system: &mut FontSystem) -> bool {
        let Some(item) = self.completer.accept() else {
            return false;
        };
        // The span the answer spoke for has just been rewritten, so nothing it
        // said about that word stands any more.
        self.answered = None;
        let text = item.text.clone();
        // A name taken once is a name this project talks about: the next list
        // puts it first. The language is the one the list was asked from.
        let language = match self.completer.target() {
            Target::Shell { .. } => Some("shell"),
            Target::Editor { .. } => self.active_editor().and_then(Editor::language_name),
        };
        if let Some(language) = language {
            self.usage.take(language, &item.name);
        }
        match self.completer.target() {
            Target::Shell { start } => {
                if let Some(shell) = self.active_shell_mut() {
                    shell.replace_word(font_system, start, &text);
                }
            }
            Target::Editor { start, end } => {
                let Some(editor) = self.active_editor_mut() else {
                    return true;
                };
                let caret = editor.selection.cursor;
                // The answer belongs to the caret that asked for it, and
                // that caret may have moved while the answer was on screen.
                // Another row is another word, and a span that no longer
                // ends where the caret stands would be spliced around it.
                if caret.line != start.line {
                    return true;
                }
                editor.clear_snippet();
                let mut start = start;
                let mut end = end;
                // The include a server attached to the answer is what makes
                // the name mean anything. It goes in first, and the word's own
                // span rides along with the lines it adds.
                if !item.edits.is_empty() {
                    let mut edits: Vec<(Position, Position, String)> = item
                        .edits
                        .iter()
                        .map(|edit| placed_edit(&editor.document, edit))
                        .collect();
                    edits.sort_by(|left, right| right.0.cmp(&left.0));
                    if let Err(error) = editor.apply_edits(font_system, &edits) {
                        self.message = Some(format!("!{error}"));
                        return true;
                    }
                    for (from, to, text) in &edits {
                        start = crate::editor::shifted(start, *from, *to, text);
                        end = crate::editor::shifted(end, *from, *to, text);
                    }
                }
                let caret = editor.selection.cursor;
                let length = editor.document.line_text(start.line).chars().count();
                let stop = if editor.text_after_caret().is_empty() {
                    caret.ch
                } else {
                    end.ch.max(caret.ch)
                };
                let start = Position::new(start.line, start.ch.min(caret.ch).min(length));
                let end = Position::new(start.line, stop.min(length));
                // A function comes with its brackets and the places inside
                // them; a name comes alone. Either way one edit is made, so a
                // single undo takes the whole completion back — and the space
                // that finishes a word belongs to that same edit.
                let stops = match &item.call {
                    Some(call) => call.stops.clone(),
                    None => Vec::new(),
                };
                let mut typed = match &item.call {
                    Some(call) => call.text.clone(),
                    None => text.clone(),
                };
                // Only a plain name at the end of a row. A call puts the caret
                // inside its brackets, and `printf (` is not how a call reads;
                // a row with text after the caret is one the name is being
                // written into, not one being finished.
                if stops.is_empty() && editor.text_after_caret().is_empty() {
                    typed.push(' ');
                }
                if let Err(error) = editor.replace_range(font_system, start, end, &typed) {
                    self.message = Some(format!("!{error}"));
                    return true;
                }
                if !stops.is_empty() {
                    editor.begin_snippet(font_system, stops_at(start, &typed, &stops));
                }
            }
        }
        true
    }

    /// What a file page is drawn at. The shell keeps the body size because its
    /// grid is the terminal's own metric; a file is read, not echoed.
    fn editor_font_size(&self) -> f32 {
        self.body_font_size * theme::EDITOR_FONT_RATIO
    }

    pub fn adjust_body_font(&mut self, font_system: &mut FontSystem, delta: f32) {
        let min = theme::MIN_FONT_SIZE * self.scale;
        let max = theme::MAX_FONT_SIZE * self.scale;
        let size = (self.body_font_size + delta * self.scale).clamp(min, max);
        if (size - self.body_font_size).abs() < 0.01 {
            return;
        }
        self.body_font_size = size;
        let editor_size = self.editor_font_size();
        for tab in &mut self.tabs {
            match &mut tab.content {
                TabContent::Shell(shell) => shell.set_font_size(font_system, size),
                TabContent::Editor(editor) => editor.set_font_size(font_system, editor_size),
                // The board sizes its own type from the panel, so Ctrl++/Ctrl+-
                // does not reach it.
                TabContent::Miku(_) => {}
            }
        }
        self.message = Some(format!("text size {:.0}px", size / self.scale));
    }

    /// A command line has just been dealt with. Built-ins write files right
    /// there — `ni t.c` makes one and never starts a process — so this is where
    /// the tree learns it may have changed.
    pub fn command_ran(&mut self, font_system: &mut FontSystem) {
        self.sidebar.git_changed(font_system);
    }

    pub fn poll_shell(&mut self, font_system: &mut FontSystem) -> bool {
        let mut changed = false;
        let mut command_ended = false;
        for tab in &mut self.tabs {
            if let TabContent::Shell(shell) = &mut tab.content {
                let was_running = shell.is_running();
                if shell.poll(font_system) {
                    changed = true;
                }
                if was_running && !shell.is_running() {
                    command_ended = true;
                }
            }
        }
        // Whatever the command did to the tree is on disk by now, so the listing
        // is rebuilt the moment it is done rather than waiting for the panel to
        // be closed and opened again.
        if command_ended {
            self.sidebar.git_changed(font_system);
        }
        changed
    }

    pub fn any_shell_running(&self) -> bool {
        self.tabs.iter().any(|tab| match &tab.content {
            TabContent::Shell(shell) => shell.is_running(),
            _ => false,
        })
    }

    pub fn kill_shells(&mut self) {
        for tab in &mut self.tabs {
            if let TabContent::Shell(shell) = &mut tab.content {
                shell.kill_job();
            }
        }
    }

    /// The window is going away. A language server left alive after it is a
    /// process of several hundred megabytes that nobody can see or close.
    pub fn kill_servers(&mut self) {
        self.servers.quit();
    }

    /// Either background worker still owes the sidebar an answer.
    pub fn sidebar_pending(&self) -> bool {
        self.sidebar.visible && (self.sidebar.git_pending() || self.sidebar.archive_pending())
    }

    pub fn poll_sidebar(&mut self, font_system: &mut FontSystem) -> bool {
        let git = self.sidebar.poll_git(font_system);
        let archive = self.sidebar.poll_archive(font_system);
        git || archive
    }

    pub fn sidebar_enter(&mut self, font_system: &mut FontSystem) -> Option<PathBuf> {
        match self.sidebar.enter(font_system) {
            Some(SidebarAction::Open(path)) => Some(path),
            Some(SidebarAction::Missing) => {
                self.message = Some("! deleted in the work tree".to_string());
                None
            }
            Some(SidebarAction::ReadOnly) => {
                self.message = Some("! read-only inside archives".to_string());
                None
            }
            None => None,
        }
    }

    /// Space hands the selected row to the shell input, without running it.
    pub fn sidebar_space(&mut self, font_system: &mut FontSystem) {
        let Some(row) = self.sidebar.selected_row() else {
            return;
        };
        // A name inside an archive is not something the shell could open.
        let message = if row.missing {
            Some("! deleted in the work tree")
        } else if row.archive.is_some() {
            Some("! read-only inside archives")
        } else {
            None
        };
        if let Some(message) = message {
            self.message = Some(message.to_string());
            return;
        }
        let command = row.quick_command();
        self.activate_tab(font_system, 0);
        self.focus_right_insert(font_system);
        if let Some(shell) = self.shell_tab_mut() {
            shell.insert_text(font_system, &command);
        }
    }

    pub fn handle_shell_effect(&mut self, font_system: &mut FontSystem, effect: ShellEffect) {
        match effect {
            ShellEffect::Open(path) => {
                if let Err(error) = self.open_file(font_system, path) {
                    if let Some(shell) = self.shell_tab_mut() {
                        shell.print_error(font_system, format!("kkk: {error}"));
                    }
                }
            }
            ShellEffect::Retheme => self.retheme(font_system),
            ShellEffect::Restart => self.restart_shell(font_system),
            ShellEffect::Miku => self.open_miku(font_system),
        }
    }

    /// Rebuilds the shell inside its own tab. Other tabs, the tab strip and
    /// the page on screen are untouched.
    pub fn restart_shell(&mut self, font_system: &mut FontSystem) {
        let Some(index) = self.tabs.iter().position(Tab::is_shell) else {
            return;
        };
        let content = self.editor_content_rect();
        self.tabs[index].content = TabContent::Shell(Shell::new(
            font_system,
            self.family,
            self.body_font_size,
            content.w,
            content.h,
        ));
        self.active = index;
        self.focus_right_insert(font_system);
        self.recompute_layout(font_system);
    }

    /// Repaint every buffer that baked theme colors into its glyphs. Geometry,
    /// text, scroll and caret are left exactly as they were. The panes bake
    /// their colors through the focus-aware resolver, so this is also what a
    /// focus flip goes through.
    pub fn retheme(&mut self, font_system: &mut FontSystem) {
        // engineering palette: keep its fields pointed at the running theme
        self.palette.sync();
        let right_focused = self.focus == Focus::Right;
        let logo_attrs = Attrs::new()
            .family(glyphon::cosmic_text::Family::Name(self.family))
            .color(theme::accent())
            .weight(Weight::BOLD);
        self.logo = make_label(font_system, &logo_attrs, 15.0 * self.scale, "kite");
        self.logo_width = measure(&self.logo).0;
        self.status = make_label(
            font_system,
            &attrs(self.family, theme::text_dim()),
            self.status_font_size,
            "",
        );
        self.status_key.clear();
        for index in 0..self.tabs.len() {
            self.refresh_label(font_system, index);
        }
        for tab in &mut self.tabs {
            match &mut tab.content {
                TabContent::Shell(shell) => {
                    shell.focused = right_focused;
                    shell.retheme(font_system);
                }
                TabContent::Editor(editor) => {
                    editor.focused = right_focused;
                    editor.retheme(font_system);
                }
                TabContent::Miku(miku) => {
                    miku.focused = right_focused;
                    miku.retheme(font_system);
                }
            }
        }
        self.sidebar.focused = self.focus == Focus::Sidebar;
        self.sidebar.retheme(font_system);
        self.recompute_layout(font_system);
    }

    pub fn prepare(&mut self, font_system: &mut FontSystem) {
        let delta = match &mut self.tabs[self.active].content {
            TabContent::Editor(editor) => {
                // Colouring the rows shapes them, so a pass that recoloured
                // nothing needs no second walk over the document.
                if !editor.highlight(font_system) {
                    editor.shape(font_system);
                }
                editor.refresh_numbers(font_system);
                editor.line_delta()
            }
            TabContent::Shell(shell) => {
                shell.shape(font_system);
                (0, 0)
            }
            TabContent::Miku(_) => (0, 0),
        };
        if delta != self.tabs[self.active].label_delta {
            self.refresh_label(font_system, self.active);
            // The counter widened the label, so the strip has to hand this tab
            // more room or the new text is clipped away.
            self.layout_tabs(false);
        }
        if self.sidebar.visible {
            self.sidebar.shape(font_system);
        }
        self.refresh_status(font_system);
    }

    pub fn text_areas(&self) -> Vec<TextArea<'_>> {
        let mut areas = Vec::new();
        let left = theme::pane(self.focus == Focus::Sidebar);
        let right = theme::pane(self.focus == Focus::Right);
        let logo_bounds = TextBounds {
            left: 0,
            top: 0,
            right: self.size.width as i32,
            bottom: self.titlebar_h as i32,
        };
        areas.push(TextArea {
            buffer: &self.logo,
            left: 16.0 * self.scale,
            top: (self.titlebar_h - 15.0 * self.scale * 1.35) / 2.0,
            scale: 1.0,
            bounds: logo_bounds,
            default_color: theme::accent(),
            custom_glyphs: &[],
        });

        for (index, tab) in self.tabs.iter().enumerate() {
            if tab.rect.w <= 1.0 {
                continue;
            }
            let close_space = if tab.close_rect.is_some() {
                self.close_w + 6.0 * self.scale
            } else {
                4.0 * self.scale
            };
            let clip_left = self.tab_strip.x + 2.0 * self.scale;
            let clip_right = self.tab_strip.right();
            let bounds = TextBounds {
                left: (tab.rect.x + 2.0 * self.scale).max(clip_left) as i32,
                top: tab.rect.y as i32,
                right: (tab.rect.right() - close_space).min(clip_right) as i32,
                bottom: tab.rect.bottom() as i32,
            };
            if bounds.right <= bounds.left {
                continue;
            }
            areas.push(TextArea {
                buffer: &tab.label,
                left: (tab.rect.x + self.tab_pad).max(clip_left),
                top: tab.rect.y + (tab.rect.h - self.tab_line_height) / 2.0
                    - if index == self.active {
                        0.0
                    } else {
                        0.5 * self.scale
                    },
                scale: 1.0,
                bounds,
                default_color: if index == self.active {
                    right.text
                } else {
                    right.text_dim
                },
                custom_glyphs: &[],
            });
        }

        match &self.active_tab().content {
            TabContent::Editor(editor) => {
                let content = self.editor_content_rect();
                areas.push(TextArea {
                    buffer: editor.numbers(),
                    left: content.x + editor.numbers_left(),
                    top: content.y,
                    scale: 1.0,
                    bounds: bounds_of(content),
                    default_color: right.text_dim,
                    custom_glyphs: &[],
                });
                let text = self.body_text_rect();
                areas.push(TextArea {
                    buffer: &editor.buffer,
                    left: text.x,
                    top: text.y,
                    scale: 1.0,
                    bounds: bounds_of(text),
                    default_color: right.text,
                    custom_glyphs: &[],
                });
            }
            TabContent::Shell(shell) => {
                let content = self.editor_content_rect();
                areas.push(TextArea {
                    buffer: &shell.buffer,
                    left: content.x,
                    top: content.y,
                    scale: 1.0,
                    bounds: bounds_of(content),
                    default_color: right.text,
                    custom_glyphs: &[],
                });
            }
            TabContent::Miku(miku) => {
                // The board lays itself out in its own page coordinates.
                let content = self.editor_content_rect();
                for (buffer, x, y) in miku.areas() {
                    areas.push(TextArea {
                        buffer,
                        left: content.x + x,
                        top: content.y + y,
                        scale: 1.0,
                        bounds: bounds_of(content),
                        default_color: right.text,
                        custom_glyphs: &[],
                    });
                }
            }
        }

        if self.sidebar.visible {
            let content = self.sidebar_content_rect();
            areas.push(TextArea {
                buffer: &self.sidebar.buffer,
                left: content.x,
                top: content.y,
                scale: 1.0,
                bounds: bounds_of(content),
                default_color: left.text,
                custom_glyphs: &[],
            });
        }

        let status_bounds = bounds_of(self.status_rect);
        areas.push(TextArea {
            buffer: &self.status,
            left: self.status_rect.x + 10.0 * self.scale,
            top: self.status_rect.y + (self.status_rect.h - self.status_line_height) / 2.0,
            scale: 1.0,
            bounds: status_bounds,
            default_color: theme::status_text(),
            custom_glyphs: &[],
        });
        if self.status_right_w > 0.0 {
            areas.push(TextArea {
                buffer: &self.status_right,
                left: self.status_right_x(),
                top: self.status_rect.y + (self.status_rect.h - self.status_line_height) / 2.0,
                scale: 1.0,
                bounds: status_bounds,
                default_color: theme::status_text(),
                custom_glyphs: &[],
            });
        }

        // engineering palette
        areas
    }

    /// engineering palette: its labels render in their own pass, above quads.
    /// The completion list rides the same pass, since it is the other thing that
    /// has to sit on top of whatever page is underneath.
    pub fn palette_areas(&self) -> Vec<TextArea<'_>> {
        let size = (self.size.width as f32, self.size.height as f32);
        let mut areas = self.palette.areas(size);
        if let Some(card) = &self.hover {
            let bounds = TextBounds {
                left: self.right.x as i32,
                top: self.right.y as i32,
                right: self.right.right() as i32,
                bottom: self.right.bottom() as i32,
            };
            areas.push(TextArea {
                buffer: &card.text,
                left: card.x,
                top: card.y,
                scale: 1.0,
                bounds,
                default_color: theme::pane(self.focus == Focus::Right).text,
                custom_glyphs: &[],
            });
        }
        areas.extend(self.completer.areas());
        areas
    }

    /// Hit the band with one physical keypress. A collapsed band takes the hit
    /// and drops it: it has nothing to show and must not start animating.
    pub fn visualizer_beat(&mut self, key: KeyCode) {
        if self.strip_rect.h > 0.0 {
            self.visualizer.beat(key);
        }
    }

    pub fn visualizer_live(&self) -> bool {
        self.strip_rect.h > 0.0 && self.visualizer.live()
    }

    pub fn visualizer_tick(&mut self) -> bool {
        self.strip_rect.h > 0.0 && self.visualizer.tick()
    }

    /// Where the powder leaves: the middle of the caret, in window coordinates.
    /// Nothing when this is not a caret that types.
    fn caret_spot(&self) -> Option<(f32, f32)> {
        if self.focus != Focus::Right || self.mode != Mode::Insert {
            return None;
        }
        let content = self.body_text_rect();
        let (x, y, height) = match &self.active_tab().content {
            TabContent::Editor(editor) => editor.cursor_rect()?,
            TabContent::Shell(shell) => shell.cursor_rect()?,
            TabContent::Miku(_) => return None,
        };
        Some((content.x + x, content.y + y + height * 0.5))
    }

    /// The ink of the glyph beside the caret — the one typing just wrote, or
    /// the one a delete is about to eat.
    pub fn caret_ink(&self, left: bool) -> Option<Color> {
        if self.focus != Focus::Right || self.mode != Mode::Insert {
            return None;
        }
        match &self.active_tab().content {
            TabContent::Editor(editor) => editor.glyph_ink(left),
            TabContent::Shell(shell) => shell.glyph_ink(left),
            TabContent::Miku(_) => None,
        }
    }

    /// A keystroke throws powder off the caret it just landed on, in the ink of
    /// the glyph it just wrote.
    pub fn spark_at_caret(&mut self) {
        let ink = self
            .caret_ink(true)
            .unwrap_or_else(|| theme::pane(self.focus == Focus::Right).text);
        if let Some((x, y)) = self.caret_spot() {
            self.sparks.burst(x, y, self.scale, ink);
        }
    }

    /// A delete throws the same powder, in the ink of the glyph it ate. That
    /// ink has to be read before the key lands, so it comes in as an argument.
    pub fn spark_delete(&mut self, ink: Color) {
        if let Some((x, y)) = self.caret_spot() {
            self.sparks.burst(x, y, self.scale, ink);
        }
    }

    pub fn sparks_live(&self) -> bool {
        self.sparks.live()
    }

    pub fn sparks_tick(&mut self) {
        self.sparks.tick();
    }

    pub fn background_quads(&self) -> Vec<QuadVertex> {
        let mut quads = Vec::new();
        let left = theme::pane(self.focus == Focus::Sidebar);
        let right = theme::pane(self.focus == Focus::Right);
        push_rect(&mut quads, self.right_pane, right.panel);
        push_rect(&mut quads, self.status_rect, theme::status());
        push_rect(
            &mut quads,
            Rect {
                x: self.right.x,
                y: self.titlebar_h,
                w: self.size.width as f32,
                h: 1.0 * self.scale,
            },
            right.border,
        );
        push_rect(
            &mut quads,
            Rect {
                x: 0.0,
                y: self.status_rect.y - 1.0 * self.scale,
                w: self.size.width as f32,
                h: 1.0 * self.scale,
            },
            theme::border(),
        );

        if self.sidebar.visible {
            push_rect(&mut quads, self.sidebar_rect, left.sidebar);
            push_rect(
                &mut quads,
                Rect {
                    x: self.sidebar_rect.right() - 1.0 * self.scale,
                    y: self.sidebar_rect.y,
                    w: 1.0 * self.scale,
                    h: self.sidebar_rect.h,
                },
                left.border,
            );
            if let Some(row) = self.sidebar.selected_row() {
                let visible_start = self.sidebar.buffer.scroll().line;
                if self.sidebar.selected >= visible_start {
                    let row_index = self.sidebar.selected - visible_start;
                    let content = self.sidebar_content_rect();
                    let y = content.y + row_index as f32 * self.sidebar.line_height();
                    if y + self.sidebar.line_height() <= self.sidebar_rect.bottom() {
                        push_rect(
                            &mut quads,
                            Rect {
                                x: self.sidebar_rect.x + 4.0 * self.scale,
                                y,
                                w: self.sidebar_rect.w - 8.0 * self.scale,
                                h: self.sidebar.line_height(),
                            },
                            left.accent_dim,
                        );
                    }
                    let _ = row;
                }
            }
        }

        push_rect(&mut quads, self.tabbar_rect(), right.panel_dim);
        for (index, tab) in self.tabs.iter().enumerate() {
            let shown = tab.rect.clip_x(self.tab_strip.x, self.tab_strip.right());
            if shown.w <= 1.0 {
                continue;
            }
            let color = if index == self.active {
                right.tab_active
            } else if self.hover_tab(index) {
                right.tab_hover
            } else {
                right.panel_dim
            };
            push_rect(&mut quads, shown, color);
            if index == self.active {
                let underline = Rect {
                    x: tab.rect.x + 6.0 * self.scale,
                    y: tab.rect.bottom() - 2.0 * self.scale,
                    w: (tab.rect.w - 12.0 * self.scale).max(0.0),
                    h: 2.0 * self.scale,
                }
                .clip_x(self.tab_strip.x, self.tab_strip.right());
                push_rect(&mut quads, underline, right.accent);
            }
            if let Some(close) = tab.close_rect {
                let color = if self.hover_close(index) {
                    right.red
                } else {
                    right.text_faint
                };
                let inset = 5.0 * self.scale;
                push_line(
                    &mut quads,
                    close.x + inset,
                    close.y + inset,
                    close.right() - inset,
                    close.bottom() - inset,
                    1.3 * self.scale,
                    color,
                );
                push_line(
                    &mut quads,
                    close.right() - inset,
                    close.y + inset,
                    close.x + inset,
                    close.bottom() - inset,
                    1.3 * self.scale,
                    color,
                );
            }
        }

        if self.window_close.contains(self.mouse_x, self.mouse_y) {
            push_rect(&mut quads, self.window_close, theme::red());
        }
        {
            let close = self.window_close;
            let cx = close.x + close.w / 2.0;
            let cy = close.y + close.h / 2.0;
            let arm = 5.0 * self.scale;
            let color = if close.contains(self.mouse_x, self.mouse_y) {
                theme::bg()
            } else {
                theme::text_dim()
            };
            push_line(
                &mut quads,
                cx - arm,
                cy - arm,
                cx + arm,
                cy + arm,
                1.4 * self.scale,
                color,
            );
            push_line(
                &mut quads,
                cx + arm,
                cy - arm,
                cx - arm,
                cy + arm,
                1.4 * self.scale,
                color,
            );
        }

        let content = self.body_text_rect();
        match &self.active_tab().content {
            TabContent::Miku(miku) => {
                for (cell, color, radius) in miku.quads() {
                    push_round_rect(
                        &mut quads,
                        Rect {
                            x: content.x + cell.x,
                            y: content.y + cell.y,
                            w: cell.w,
                            h: cell.h,
                        },
                        color,
                        radius,
                    );
                }
            }
            TabContent::Editor(editor) => {
                for (x, y, width, height) in editor.selection_spans() {
                    push_rect(
                        &mut quads,
                        Rect {
                            x: content.x + x,
                            y: content.y + y,
                            w: width,
                            h: height,
                        },
                        right.selection,
                    );
                }
                // What the server said about this page, under the text it said
                // it about. Red for the trouble that stops the program, yellow
                // for the doubt: two colours because a page that cannot compile
                // and a page that might be wrong are not the same notice.
                for (_, x, top, width, height, error) in editor.issue_spans() {
                    push_squiggle(
                        &mut quads,
                        Rect {
                            x: content.x + x,
                            y: content.y + top,
                            w: width,
                            h: height,
                        },
                        if error { right.red } else { right.yellow },
                        self.scale,
                    );
                }
            }
            TabContent::Shell(shell) => {
                for (x, y, width, height) in shell.selection_spans() {
                    push_rect(
                        &mut quads,
                        Rect {
                            x: content.x + x,
                            y: content.y + y,
                            w: width,
                            h: height,
                        },
                        right.selection,
                    );
                }
            }
        }

        let mut band = self.visualizer.quads(self.strip_rect, self.scale);
        if self.focus != Focus::Right {
            unfocus_quads(&mut band, theme::unfocus(), theme::bg());
        }
        quads.extend(band);

        quads
    }

    pub fn foreground_quads(&mut self, font_system: &mut FontSystem) -> Vec<QuadVertex> {
        let mut quads = Vec::new();
        let content = self.body_text_rect();
        let right = theme::pane(self.focus == Focus::Right);

        if self.show_whitespace {
            match &self.active_tab().content {
                TabContent::Editor(editor) => {
                    let line_height = editor.line_height();
                    let breaks = |line: usize| editor.document.has_line_break(line);
                    push_whitespace_markers(
                        &mut quads,
                        &editor.buffer,
                        content,
                        self.editor_font_size(),
                        line_height,
                        &breaks,
                        right,
                    );
                }
                TabContent::Shell(shell) => {
                    let line_height = shell.line_height();
                    // A grid row is exactly as wide as the screen, so no row
                    // continues into another and none needs a wrap marker.
                    let breaks = |_line: usize| false;
                    push_whitespace_markers(
                        &mut quads,
                        &shell.buffer,
                        content,
                        self.body_font_size,
                        line_height,
                        &breaks,
                        right,
                    );
                }
                // The board has no text of its own to mark up.
                TabContent::Miku(_) => {}
            }
        }

        if self.focus == Focus::Right
            && (self.mode == Mode::Normal || self.cursor_blink || self.caret.flying)
        {
            let cursor = match &self.active_tab().content {
                TabContent::Editor(editor) => editor.cursor_rect(),
                TabContent::Shell(shell) => shell.cursor_rect(),
                TabContent::Miku(_) => None,
            };
            if let Some((x, y, height)) = cursor {
                let raw_x = content.x + x;
                let raw_y = content.y + y + 2.0 * self.scale;
                // A caret the view scrolled past is not on screen to draw.
                let inside =
                    raw_x >= content.x && raw_x <= content.right() && raw_y >= content.y;
                let (cx, cy) = if self.caret.had_target {
                    (self.caret.x, self.caret.y)
                } else {
                    (raw_x, raw_y)
                };
                if inside {
                    push_rect(
                        &mut quads,
                        Rect {
                            x: cx,
                            y: cy,
                            w: 2.0 * self.scale,
                            h: (height - 4.0 * self.scale).max(2.0),
                        },
                        right.cursor,
                    );
                }
            }
        }

        self.hover = None;
        if let Some((words, x, y, error)) = self.hovered_issue() {
            let right = theme::pane(self.focus == Focus::Right);
            let ink = if error { right.red } else { right.yellow };
            let size = self.editor_font_size();
            let pad = 7.0 * self.scale;
            // A server writes a sentence and the card is not a row of the grid:
            // it takes the room the pane has left, never so wide that the eye
            // loses the line on its way back, and breaks the sentence across as
            // many rows as it needs.
            let room = (self.right.w - pad * 2.0).min(size * 40.0);
            let text = make_paragraph(
                font_system,
                &attrs(self.family, right.text),
                size,
                &words,
                room,
            );
            let (text_w, text_h) = measure_block(&text);
            let card = Rect {
                x: x.max(self.right.x + pad)
                    .min((self.right.right() - text_w - pad * 3.0).max(self.right.x + pad)),
                y: (y + 4.0 * self.scale).min(self.right.bottom() - text_h - pad * 2.0),
                w: text_w + pad * 2.0,
                h: text_h + pad * 2.0,
            };
            push_round_rect(&mut quads, card, right.panel, 4.0 * self.scale);
            // One edge in the colour of the trouble, so a card that has opened
            // is still obvious when the mouse has moved off the line it was for.
            push_rect(
                &mut quads,
                Rect {
                    x: card.x,
                    y: card.y,
                    w: 2.5 * self.scale,
                    h: card.h,
                },
                ink,
            );
            self.hover = Some(Hover {
                text,
                x: card.x + pad,
                y: card.y + pad,
            });
        }

        quads.extend(self.sparks.quads());
        // engineering palette: drawn last so it covers everything
        let size = (self.size.width as f32, self.size.height as f32);
        quads.extend(self.palette.quads(font_system, size));
        quads.extend(self.completer.quads(font_system, size));
        quads
    }

    pub fn ime_area(&self) -> Option<Rect> {
        if self.focus != Focus::Right || self.mode != Mode::Insert {
            return None;
        }
        let content = self.body_text_rect();
        let cursor = match &self.active_tab().content {
            TabContent::Editor(editor) => editor.cursor_rect(),
            TabContent::Shell(shell) => shell.cursor_rect(),
            TabContent::Miku(_) => None,
        }?;
        Some(Rect {
            x: content.x + cursor.0,
            y: content.y + cursor.1,
            w: 2.0 * self.scale,
            h: cursor.2,
        })
    }

    pub fn cursor_blink_reset(&mut self) {
        self.cursor_blink = true;
    }

    pub fn toggle_cursor_blink(&mut self) {
        self.cursor_blink = !self.cursor_blink;
    }

    /// Advance the caret's glide one frame toward the logical caret. It reads
    /// the active tab's own cursor rect, so callers never pass coordinates.
    /// Called from the redraw path (to pick up a new target) and from the
    /// animation loop while it is still flying (to keep easing).
    pub fn caret_tick(&mut self) {
        let now = Instant::now();
        let content = self.body_text_rect();
        let (rect, spot) = match &self.active_tab().content {
            TabContent::Editor(editor) => {
                let position = editor.cursor_position();
                (
                    editor.cursor_rect(),
                    Some((position.line as isize, position.ch as isize)),
                )
            }
            TabContent::Shell(shell) => (shell.cursor_rect(), shell.cursor_spot()),
            TabContent::Miku(_) => (None, None),
        };
        let (tx, ty) = match rect {
            Some((x, y, _)) => (content.x + x, content.y + y + 2.0 * self.scale),
            None => {
                self.caret.had_target = false;
                self.caret.flying = false;
                self.caret.spot = None;
                self.caret.last_tick = now;
                return;
            }
        };

        // A logical spot we have not aimed at yet: glide if it is close enough
        // to read as motion, otherwise teleport. Resetting the clock here means
        // the first eased frame steps by ~0 and the flight begins fresh rather
        // than snapping shut over whatever idle time elapsed since the last tick.
        if !self.caret.had_target || self.caret.target != (tx, ty) {
            // The caret itself only moved if its logical spot did. Same spot at
            // a new pixel means the view scrolled under it: the caret belongs to
            // the text, so it goes where the text went, with no easing.
            let caret_moved = self.caret.spot != spot;
            let dx = tx - self.caret.x;
            let dy = ty - self.caret.y;
            if !self.caret.had_target || !caret_moved || (dx * dx + dy * dy).sqrt() > CARET_SNAP
            {
                self.caret.x = tx;
                self.caret.y = ty;
                self.caret.flying = false;
            } else {
                self.caret.flying = true;
            }
            self.caret.target = (tx, ty);
            self.caret.spot = spot;
            self.caret.last_tick = now;
            self.caret.had_target = true;
            return;
        }

        let dt = now.duration_since(self.caret.last_tick).as_secs_f32();
        self.caret.last_tick = now;
        if !self.caret.flying {
            return;
        }
        let dx = tx - self.caret.x;
        let dy = ty - self.caret.y;
        if (dx * dx + dy * dy).sqrt() <= CARET_LANDED {
            self.caret.x = tx;
            self.caret.y = ty;
            self.caret.flying = false;
            return;
        }
        self.caret.x = caret_step(self.caret.x, tx, dt);
        self.caret.y = caret_step(self.caret.y, ty, dt);
    }

    pub fn caret_flying(&self) -> bool {
        self.caret.flying
    }

    pub fn set_mouse(&mut self, x: f32, y: f32) {
        self.mouse_x = x;
        self.mouse_y = y;
    }

    pub fn mouse(&self) -> (f32, f32) {
        (self.mouse_x, self.mouse_y)
    }

    pub fn hit_tab(&self, x: f32, y: f32) -> Option<usize> {
        if !self.tab_strip.contains(x, y) {
            return None;
        }
        self.tabs
            .iter()
            .position(|tab| tab.rect.w > 1.0 && tab.rect.contains(x, y))
    }

    pub fn hit_close(&self, x: f32, y: f32) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.close_rect.is_some_and(|close| close.contains(x, y)))
    }

    pub fn hit_window_close(&self, x: f32, y: f32) -> bool {
        self.window_close.contains(x, y)
    }

    pub fn hit_titlebar(&self, y: f32) -> bool {
        y < self.titlebar_h
    }

    pub fn hit_sidebar(&self, x: f32, y: f32) -> bool {
        self.sidebar.visible && self.sidebar_rect.contains(x, y)
    }

    pub fn hit_right(&self, x: f32, y: f32) -> bool {
        self.right.contains(x, y)
    }

    pub fn editor_position(
        &mut self,
        font_system: &mut FontSystem,
        x: f32,
        y: f32,
    ) -> Option<Position> {
        let content = self.body_text_rect();
        let editor = self.active_editor_mut()?;
        editor.position_at(font_system, x - content.x, y - content.y)
    }

    pub fn set_editor_selection(&mut self, font_system: &mut FontSystem, selection: Selection) {
        if let Some(editor) = self.active_editor_mut() {
            editor.set_selection(font_system, selection);
        }
    }

    pub fn scroll(&mut self, font_system: &mut FontSystem, x: f32, y: f32, lines: f32) {
        if self.tab_overflow && self.tab_strip.contains(x, y) {
            self.tab_scroll = (self.tab_scroll - lines * 48.0 * self.scale).max(0.0);
            self.layout_tabs(false);
            return;
        }
        if self.sidebar.visible && self.sidebar_rect.contains(x, y) {
            self.sidebar.scroll_by(font_system, -lines);
            return;
        }
        if !self.right.contains(x, y) {
            return;
        }
        let content = self.editor_content_rect();
        match &mut self.active_tab_mut().content {
            TabContent::Editor(editor) => {
                editor.scroll_by(font_system, -lines * editor.line_height());
            }
            TabContent::Shell(shell) => {
                shell.scroll_by(
                    font_system,
                    x - content.x,
                    y - content.y,
                    -lines * shell.line_height(),
                );
            }
            // One notch turns one year over; up the wheel, back in time.
            TabContent::Miku(miku) => miku.page(font_system, if lines > 0.0 { 1 } else { -1 }),
        }
    }

    pub fn refresh_status(&mut self, font_system: &mut FontSystem) {
        let mut text = match &self.active_tab().content {
            TabContent::Editor(editor) => {
                let cursor = editor.cursor_position();
                // The count is the whole reason to look at the bar: a page with
                // two troubles on it says so before the reader goes hunting, and
                // a clean page says nothing at all.
                let (errors, warnings) = editor.issue_counts();
                let trouble = match (errors, warnings) {
                    (0, 0) => String::new(),
                    (0, warnings) => format!("   {warnings} warn"),
                    (errors, 0) => format!("   {errors} err"),
                    (errors, warnings) => format!("   {errors} err   {warnings} warn"),
                };
                format!(
                    "{}   {}   Ln {}, Col {}{trouble}",
                    kite_core::display_path(editor.document.path()),
                    editor.encoding_name(),
                    cursor.line + 1,
                    cursor.ch + 1,
                )
            }
            TabContent::Shell(shell) => {
                let (line, column) = shell.cursor_line_column();
                let running = if shell.is_running() { "   running" } else { "" };
                format!(
                    "{}   shell{}   Ln {}, Col {}",
                    kite_core::display_path(&shell.cwd),
                    running,
                    line,
                    column
                )
            }
            TabContent::Miku(miku) => format!("miku   {}", miku.status()),
        };
        if let Some(message) = &self.message {
            text.push_str("   ");
            text.push_str(message);
        }
        // A character count belongs in the corner, not in the middle of the bar.
        let count = match &self.active_tab().content {
            TabContent::Editor(editor) => format!("{} chars", editor.document.char_count()),
            TabContent::Shell(_) => String::new(),
            TabContent::Miku(_) => String::new(),
        };
        // While a child holds the keyboard, kite's editing mode is not what the
        // screen means: a terminal does not advertise a modal state either.
        let child_runs =
            matches!(&self.active_tab().content, TabContent::Shell(shell) if shell.is_running());
        if !child_runs {
            text.push_str(match (self.focus, self.mode) {
                (Focus::Sidebar, _) | (Focus::Right, Mode::Normal) => "   [NORMAL]",
                (Focus::Right, Mode::Insert) => "   [INSERT]",
            });
        }
        if self.topmost {
            text.push_str("   [TOP]");
        }
        let key = format!("{text}|{count}|{}", self.message.as_deref().unwrap_or(""));
        if key == self.status_key {
            return;
        }
        self.status_key = key;
        let color = if self
            .message
            .as_deref()
            .is_some_and(|message| message.starts_with('!'))
        {
            theme::red()
        } else {
            theme::status_text()
        };
        self.status = make_label(
            font_system,
            &attrs(self.family, color),
            self.status_font_size,
            &text,
        );
        self.status_right = make_label(
            font_system,
            &attrs(self.family, theme::status_text()),
            self.status_font_size,
            &count,
        );
        self.status_right_w = measure(&self.status_right).0;
    }

    /// The count hugs the right edge of the bar, with the same padding as the left text.
    fn status_right_x(&self) -> f32 {
        (self.status_rect.right() - 10.0 * self.scale - self.status_right_w).max(self.status_rect.x)
    }

    fn refresh_label(&mut self, font_system: &mut FontSystem, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let active = index == self.active;
        let right = theme::pane(self.focus == Focus::Right);
        let color = if active { right.text } else { right.text_dim };
        let font_size = self.tab_font_size;
        let delta = match &mut tab.content {
            TabContent::Editor(editor) => editor.line_delta(),
            TabContent::Shell(_) => (0, 0),
            TabContent::Miku(_) => (0, 0),
        };
        let title = tab.title.clone();
        tab.label_delta = delta;
        tab.label = make_counted_label(font_system, self.family, &title, delta, color, font_size, right);
        let (width, _) = measure(&tab.label);
        tab.label_width = width;
        let close_extra = if tab.is_shell() {
            4.0 * self.scale
        } else {
            self.close_w + 6.0 * self.scale
        };
        tab.desired_width = (width + self.tab_pad * 2.0 + close_extra)
            .min(220.0 * self.scale)
            .max(70.0 * self.scale);
    }

    fn recompute_layout(&mut self, font_system: &mut FontSystem) {
        let width = self.size.width as f32;
        let height = self.size.height as f32;
        self.titlebar_h = 40.0 * self.scale;
        self.status_h = 26.0 * self.scale;
        let strip_h = strip_height(self.scale, height / self.scale);
        if strip_h == 0.0 {
            self.visualizer.reset();
        }
        // The left/right split runs all the way down to the status bar, so the
        // strip only occupies the content column while the sidebar is open.
        self.sidebar_w = if self.sidebar.visible {
            (280.0 * self.scale).min(width * 0.45)
        } else {
            0.0
        };
        self.strip_rect = Rect {
            x: self.sidebar_w,
            y: height - self.status_h - strip_h,
            w: (width - self.sidebar_w).max(1.0),
            h: strip_h,
        };
        let body_h = (height - self.titlebar_h - self.status_h - strip_h).max(1.0);
        self.status_rect = Rect {
            x: 0.0,
            y: height - self.status_h,
            w: width,
            h: self.status_h,
        };
        self.window_close = Rect {
            x: width - 46.0 * self.scale,
            y: 0.0,
            w: 46.0 * self.scale,
            h: self.titlebar_h,
        };

        if self.sidebar.visible {
            self.sidebar_rect = Rect {
                x: 0.0,
                y: self.titlebar_h,
                w: self.sidebar_w,
                h: body_h + strip_h,
            };
            self.right = Rect {
                x: self.sidebar_w,
                y: self.titlebar_h,
                w: (width - self.sidebar_w).max(1.0),
                h: body_h,
            };
        } else {
            self.sidebar_rect = Rect::default();
            self.right = Rect {
                x: 0.0,
                y: self.titlebar_h,
                w: width.max(1.0),
                h: body_h,
            };
        }
        self.right_pane = Rect {
            h: self.right.h + strip_h,
            ..self.right
        };

        let content = self.editor_content_rect();
        for tab in &mut self.tabs {
            match &mut tab.content {
                TabContent::Editor(editor) => {
                    let gutter = editor.gutter_width();
                    editor.set_viewport(font_system, (content.w - gutter).max(1.0), content.h)
                }
                TabContent::Shell(shell) => shell.set_viewport(font_system, content.w, content.h),
                TabContent::Miku(miku) => miku.set_viewport(font_system, content.w, content.h),
            }
        }
        if self.sidebar.visible {
            let sidebar_content = self.sidebar_content_rect();
            self.sidebar
                .set_viewport(font_system, sidebar_content.w, sidebar_content.h);
        }

        self.layout_tabs(true);
        self.refresh_status(font_system);
    }

    fn layout_tabs(&mut self, reveal_active: bool) {
        let y = 5.0 * self.scale;
        let h = (self.titlebar_h - 10.0 * self.scale).max(1.0);
        let start = self.logo_width + 42.0 * self.scale;
        let max_right = self.window_close.x - 8.0 * self.scale;
        let available = (max_right - start).max(1.0);
        self.tab_strip = Rect {
            x: start,
            y,
            w: (max_right - start).max(0.0),
            h,
        };
        let count = self.tabs.len();
        if count == 0 {
            self.tab_scroll = 0.0;
            self.tab_overflow = false;
            return;
        }

        let gap = self.tab_gap;
        let total_gap = gap * count.saturating_sub(1) as f32;
        let min_width = 64.0 * self.scale;
        let desired: Vec<f32> = self
            .tabs
            .iter()
            .map(|tab| tab.desired_width.max(min_width))
            .collect();
        let desired_sum: f32 = desired.iter().sum();
        let mut widths = desired.clone();
        self.tab_overflow = false;
        if desired_sum + total_gap > available {
            let room = (available - total_gap).max(1.0);
            let min_sum = min_width * count as f32;
            if min_sum <= room {
                let slack: f32 = desired.iter().map(|width| width - min_width).sum();
                let factor = if slack > 0.0 {
                    ((room - min_sum) / slack).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                for (width, wanted) in widths.iter_mut().zip(&desired) {
                    *width = min_width + (wanted - min_width) * factor;
                }
            } else {
                widths = vec![min_width; count];
                self.tab_overflow = true;
            }
        }

        let content_width: f32 = widths.iter().sum::<f32>() + total_gap;
        let max_scroll = (content_width - available).max(0.0);
        self.tab_scroll = self.tab_scroll.clamp(0.0, max_scroll);
        if reveal_active {
            let active = self.active.min(count - 1);
            let active_left: f32 = widths[..active].iter().map(|width| width + gap).sum();
            let active_right = active_left + widths[active];
            if active_left < self.tab_scroll {
                self.tab_scroll = active_left;
            } else if active_right > self.tab_scroll + available {
                self.tab_scroll = active_right - available;
            }
            self.tab_scroll = self.tab_scroll.clamp(0.0, max_scroll);
        }

        let mut x = start - self.tab_scroll;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let width = widths[index];
            let visible_width = (x + width).min(max_right) - x.max(start);
            if visible_width <= 1.0 {
                tab.rect = Rect { x, y, w: 0.0, h };
                tab.close_rect = None;
                x += width + gap;
                continue;
            }
            tab.rect = Rect { x, y, w: width, h };
            let close = Rect {
                x: x + width - self.close_w - 3.0 * self.scale,
                y: y + (h - self.close_w) / 2.0,
                w: self.close_w,
                h: self.close_w,
            };
            tab.close_rect = if tab.is_shell()
                || width < 44.0 * self.scale
                || close.x < start
                || close.right() > max_right
            {
                None
            } else {
                Some(close)
            };
            x += width + gap;
        }
    }

    fn tabbar_rect(&self) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: self.size.width as f32,
            h: self.titlebar_h,
        }
    }

    fn hover_tab(&self, index: usize) -> bool {
        self.tab_strip.contains(self.mouse_x, self.mouse_y)
            && self
                .tabs
                .get(index)
                .is_some_and(|tab| tab.rect.contains(self.mouse_x, self.mouse_y))
    }

    fn hover_close(&self, index: usize) -> bool {
        self.tabs.get(index).is_some_and(|tab| {
            tab.close_rect
                .is_some_and(|close| close.contains(self.mouse_x, self.mouse_y))
        })
    }
}

/// An edit a server attached to an answer, in the document's own coordinates.
/// A line past the last one is the end of the last one: a server writing
/// beyond the file is writing exactly there.
fn placed_edit(document: &Document, edit: &ExtraEdit) -> (Position, Position, String) {
    let last = document.line_count().saturating_sub(1);
    let at = |(line, units): (usize, u64)| {
        let line = line.min(last);
        Position::new(
            line,
            lsp::character_column(&document.line_text(line), units as usize),
        )
    };
    (at(edit.start), at(edit.end), edit.text.clone())
}

/// The placeholders of a snippet, counted from the place its text was typed.
/// A server counts characters; the document counts lines and columns.
fn stops_at(origin: Position, text: &str, stops: &[(usize, usize)]) -> Vec<(Position, Position)> {
    let mut places = vec![origin];
    let mut line = origin.line;
    let mut ch = origin.ch;
    for value in text.chars() {
        if value == '\n' {
            line += 1;
            ch = 0;
        } else {
            ch += 1;
        }
        places.push(Position::new(line, ch));
    }
    stops
        .iter()
        .filter_map(|&(start, end)| Some((*places.get(start)?, *places.get(end)?)))
        .collect()
}

fn make_tab(
    font_system: &mut FontSystem,
    family: &'static str,
    font_size: f32,
    tab_pad: f32,
    close_w: f32,
    shell_pad: f32,
    title: &str,
    content: TabContent,
    right_focused: bool,
) -> Tab {
    let attr = attrs(family, theme::pane(right_focused).text_dim);
    let label = make_label(font_system, &attr, font_size, title);
    let (label_width, _) = measure(&label);
    let close_extra = match &content {
        TabContent::Shell(_) => shell_pad,
        TabContent::Editor(_) => close_w + 6.0,
        TabContent::Miku(_) => close_w + 6.0,
    };
    Tab {
        content,
        title: title.to_string(),
        label,
        label_width,
        label_delta: (0, 0),
        desired_width: label_width + tab_pad * 2.0 + close_extra,
        rect: Rect::default(),
        close_rect: None,
    }
}

/// Tab title plus its `+added -removed` line counter, in the counter colors.
fn make_counted_label(
    font_system: &mut FontSystem,
    family: &'static str,
    title: &str,
    delta: (usize, usize),
    color: Color,
    font_size: f32,
    colors: theme::Colors,
) -> Buffer {
    let (added, removed) = delta;
    let text = if added == 0 && removed == 0 {
        title.to_string()
    } else {
        format!("{title}  +{added} -{removed}")
    };
    let mut list = AttrsList::new(&attrs(family, color));
    if added != 0 || removed != 0 {
        let plus = title.len() + 2;
        let plus_end = plus + 1 + added.to_string().len();
        let minus = plus_end + 1;
        let minus_end = minus + 1 + removed.to_string().len();
        list.add_span(plus..plus_end, &attrs(family, colors.green));
        list.add_span(minus..minus_end, &attrs(family, colors.red));
    }
    let line_height = (font_size * 1.35).ceil();
    let mut buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
    buffer.set_wrap(Wrap::None);
    buffer.set_size(None, None);
    buffer.lines.push(BufferLine::new(
        text,
        LineEnding::Lf,
        list,
        Shaping::Advanced,
    ));
    buffer.shape_until_scroll(font_system, false);
    buffer
}

fn bounds_of(rect: Rect) -> TextBounds {
    TextBounds {
        left: rect.x as i32,
        top: rect.y as i32,
        right: rect.right() as i32,
        bottom: rect.bottom() as i32,
    }
}

/// The band above the status bar, in physical pixels. A window too short for
/// it gets no band at all rather than a squashed one.
fn strip_height(scale: f32, logical_height: f32) -> f32 {
    if logical_height >= STRIP_MIN_HEIGHT {
        STRIP_HEIGHT * scale
    } else {
        0.0
    }
}

fn linear_color(color: Color) -> [f32; 4] {
    let [r, g, b, a] = color.as_rgba();
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

pub(crate) fn push_rect(quads: &mut Vec<QuadVertex>, rect: Rect, color: Color) {
    push_round_rect(quads, rect, color, 0.0);
}

/// The mark under a trouble the server reported: a wave two dots tall along the
/// bottom of the row. A straight rule under text reads as a separator, and the
/// whole job of this one is to read as "here is something wrong" without
/// covering the letters it is about.
fn push_squiggle(quads: &mut Vec<QuadVertex>, row: Rect, color: Color, scale: f32) {
    let step = (3.0 * scale).max(2.0);
    let thick = (1.4 * scale).max(1.0);
    let base = row.y + row.h - thick * 2.5;
    let mut at = 0.0;
    let mut high = true;
    while at < row.w {
        let piece = step.min(row.w - at);
        push_rect(
            quads,
            Rect {
                x: row.x + at,
                y: if high { base } else { base + thick },
                w: piece,
                h: thick,
            },
            color,
        );
        at += piece;
        high = !high;
    }
}

/// A rectangle whose colour runs from one edge to the other. The shader
/// interpolates the vertex colours, so two corners per row are enough.
pub(crate) fn push_gradient_rect(
    quads: &mut Vec<QuadVertex>,
    rect: Rect,
    top: Color,
    bottom: Color,
) {
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let (t, b) = (linear_color(top), linear_color(bottom));
    let (x0, y0) = (rect.x, rect.y);
    let (x1, y1) = (rect.right(), rect.bottom());
    let size = [rect.w, rect.h];
    for (corner, color) in [
        ([x0, y0], t),
        ([x1, y0], t),
        ([x1, y1], b),
        ([x0, y0], t),
        ([x1, y1], b),
        ([x0, y1], b),
    ] {
        quads.push(QuadVertex {
            position: corner,
            color,
            rect: [x0, y0, size[0], size[1]],
            radius: 0.0,
        });
    }
}

fn push_round_rect(quads: &mut Vec<QuadVertex>, rect: Rect, color: Color, radius: f32) {
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let color = linear_color(color);
    let (x0, y0) = (rect.x, rect.y);
    let (w, h) = (rect.w, rect.h);
    let x1 = rect.right();
    let y1 = rect.bottom();
    for [x, y] in [[x0, y0], [x1, y0], [x1, y1], [x0, y0], [x1, y1], [x0, y1]] {
        quads.push(QuadVertex {
            position: [x, y],
            color,
            rect: [x0, y0, w, h],
            radius,
        });
    }
}

pub(crate) fn push_line(
    quads: &mut Vec<QuadVertex>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    width: f32,
    color: Color,
) {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 0.001 {
        return;
    }
    let nx = -dy / length * width / 2.0;
    let ny = dx / length * width / 2.0;
    let color = linear_color(color);
    let corners = [
        [x1 + nx, y1 + ny],
        [x2 + nx, y2 + ny],
        [x2 - nx, y2 - ny],
        [x1 - nx, y1 - ny],
    ];
    for index in [0usize, 1, 2, 0, 2, 3] {
        quads.push(QuadVertex {
            position: corners[index],
            color,
            // A slanted line has no axis-aligned box to round inside, and the
            // shader never reads these while the radius stays at zero.
            rect: [0.0, 0.0, 0.0, 0.0],
            radius: 0.0,
        });
    }
}

/// The band is right-pane content, so an unfocused right pane pulls its bar
/// colors toward `bg` exactly like the pane's own slots.
fn unfocus_quads(quads: &mut [QuadVertex], k: f32, toward: Color) {
    let [tr, tg, tb, ta] = toward.as_rgba();
    let pull = |from: f32, to: u8| from * (1.0 - k) + to as f32 / 255.0 * k;
    for quad in quads {
        quad.color = [
            pull(quad.color[0], tr),
            pull(quad.color[1], tg),
            pull(quad.color[2], tb),
            pull(quad.color[3], ta),
        ];
    }
}

fn push_whitespace_markers(
    quads: &mut Vec<QuadVertex>,
    buffer: &Buffer,
    origin: Rect,
    font_size: f32,
    line_height: f32,
    has_break: &dyn Fn(usize) -> bool,
    colors: theme::Colors,
) {
    let dot_color = colors.whitespace_dot;
    let mark_color = colors.whitespace_mark;
    let mut runs = buffer.layout_runs().peekable();
    while let Some(run) = runs.next() {
        for glyph in run.glyphs {
            let cluster = &run.text[glyph.start..glyph.end];
            let count = cluster.chars().count().max(1);
            let cell = glyph.w / count as f32;
            for (index, value) in cluster.chars().enumerate() {
                let x = glyph.x + cell * index as f32;
                match value {
                    ' ' => {
                        let size = (font_size * 0.12).max(1.5);
                        push_rect(
                            quads,
                            Rect {
                                x: origin.x + x + cell / 2.0 - size / 2.0,
                                y: origin.y + run.line_top + line_height * 0.62,
                                w: size,
                                h: size,
                            },
                            dot_color,
                        );
                    }
                    '\t' => {
                        let y = origin.y + run.line_top + line_height * 0.62;
                        let start = origin.x + x + 2.0;
                        let end = origin.x + x + cell - 2.0;
                        push_line(quads, start, y, end, y, 1.2, mark_color);
                        push_line(quads, end, y, end - 4.0, y - 3.0, 1.2, mark_color);
                        push_line(quads, end, y, end - 4.0, y + 3.0, 1.2, mark_color);
                    }
                    _ => {}
                }
            }
        }
        let last_visual = runs.peek().is_none_or(|next| next.line_i != run.line_i);
        if last_visual && has_break(run.line_i) {
            let x = origin.x + run.line_w + 3.0;
            let y = origin.y + run.line_top + line_height * 0.55;
            push_line(quads, x, y, x + 6.0, y, 1.2, mark_color);
            push_line(quads, x + 6.0, y, x + 6.0, y + 4.5, 1.2, mark_color);
            push_line(quads, x + 6.0, y + 4.5, x + 2.5, y + 4.5, 1.2, mark_color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Fonts;
    use serde_json::json;
    use std::fs;
    use winit::dpi::PhysicalSize;

    #[test]
    fn dirty_tabs_carry_a_line_counter() {
        let mut fonts = Fonts::load();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.25,
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        std::fs::write(&path, b"one\ntwo\nthree\n").unwrap();
        ui.open_file(&mut fonts.system, path.clone()).unwrap();

        ui.prepare(&mut fonts.system);
        assert_eq!(
            ui.tabs[1].label_delta,
            (0, 0),
            "an untouched file has no counter"
        );
        assert_eq!(ui.tabs[1].label.lines[0].text(), "sample.txt");
        let width = ui.tabs[1].rect.w;

        let editor = ui.active_editor_mut().unwrap();
        editor.insert_text(&mut fonts.system, "zero\n").unwrap();
        ui.prepare(&mut fonts.system);
        assert_eq!(ui.tabs[1].label_delta, (1, 0));
        assert_eq!(
            ui.tabs[1].label.lines[0].text(),
            "sample.txt  +1 -0",
            "the counter follows the edit"
        );
        assert!(
            ui.tabs[1].rect.w > width,
            "the strip gives the tab room for its counter"
        );

        std::fs::remove_file(&path).ok();
    }

    /// The wave and its card are painted inside the line-number gutter, so a hit
    /// test written against the pane rather than the text answers a mouse over
    /// the gutter and ignores one over the trouble it is asking about.
    #[test]
    fn a_mouse_on_the_wave_opens_the_report_under_it() {
        let mut fonts = Fonts::load();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.25,
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.c");
        std::fs::write(&path, b"int main(void) {\n  int a\n  return 0;\n}\n").unwrap();
        ui.open_file(&mut fonts.system, path).unwrap();
        ui.active_editor_mut().unwrap().set_issues(&[lsp::RawDiagnostic {
            start: (1, 2),
            end: (1, 7),
            error: true,
            message: "Expected ';' at end of declaration".to_string(),
            note: "clang expected_semi_declaration".to_string(),
        }]);
        ui.prepare(&mut fonts.system);

        let content = ui.body_text_rect();
        let (_, x, top, width, height, error) = ui
            .active_editor()
            .unwrap()
            .issue_spans()
            .into_iter()
            .next()
            .expect("the row the report is about is drawn");
        assert!(error, "and drawn as the trouble it is");

        ui.set_mouse(content.x + x + width * 0.5, content.y + top + height * 0.5);
        let (words, _, _, error) = ui.hovered_issue().expect("the wave answers");
        assert!(error);
        assert!(words.contains("Expected ';'"), "{words}");
        assert!(words.contains("clang"), "and says who said it");

        ui.set_mouse(content.x - 4.0, content.y + top + height * 0.5);
        assert!(
            ui.hovered_issue().is_none(),
            "a mouse in the gutter has nothing to say"
        );
    }

    #[test]
    fn panes_paint_with_colors_resolved_from_focus() {
        let mut fonts = Fonts::load();
        theme::init();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.0,
        );
        assert!(ui.strip_rect.h > 0.0, "a tall window keeps the band");
        assert_eq!(
            ui.right_pane.h,
            ui.right.h + ui.strip_rect.h,
            "the right pane's one background spans body and band"
        );
        // The tree starts put away; this is about how both panes paint once it
        // is open, so open it before looking at their colours.
        ui.sidebar.visible = true;
        ui.recompute_layout(&mut fonts.system);
        let quad = |quads: &[QuadVertex], rect: Rect| -> [f32; 4] {
            quads
                .iter()
                .find(|q| q.rect == [rect.x, rect.y, rect.w, rect.h])
                .map(|q| q.color)
                .expect("the pane paints its whole rect")
        };
        let quads = ui.background_quads();
        assert_eq!(
            quad(&quads, ui.right_pane),
            linear_color(theme::pane(true).panel)
        );
        assert_eq!(
            quad(&quads, ui.sidebar_rect),
            linear_color(theme::pane(false).sidebar)
        );

        ui.focus_sidebar(&mut fonts.system);
        let quads = ui.background_quads();
        assert_eq!(
            quad(&quads, ui.right_pane),
            linear_color(theme::pane(false).panel)
        );
        assert_eq!(
            quad(&quads, ui.sidebar_rect),
            linear_color(theme::pane(true).sidebar)
        );

        // The swap property: equal slots, one resolver, so whichever pane is
        // unfocused wears the very same shade on either side.
        theme::debug_edit(|t| t.colors.sidebar = t.colors.panel);
        let unfocused_left = quad(&ui.background_quads(), ui.right_pane);
        ui.focus_right_insert(&mut fonts.system);
        let unfocused_right = quad(&ui.background_quads(), ui.sidebar_rect);
        assert_eq!(unfocused_left, unfocused_right);
        theme::debug_edit(|t| t.colors.sidebar = theme::Colors::default().sidebar);
    }

    #[test]
    fn the_band_collapses_when_the_window_gets_too_short() {
        let mut fonts = Fonts::load();
        let mut tall = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.0,
        );
        assert_eq!(tall.strip_rect.h, 64.0);
        assert_eq!(tall.strip_rect.bottom(), 1000.0 - 26.0);
        assert_eq!(tall.right.h, 1000.0 - 40.0 - 26.0 - 64.0);
        tall.visualizer_beat(KeyCode::KeyF);
        assert!(tall.visualizer_live());

        let mut short = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 300),
            1.0,
        );
        assert_eq!(short.strip_rect.h, 0.0, "no room means no band");
        short.visualizer_beat(KeyCode::KeyF);
        assert!(!short.visualizer_live(), "a collapsed band cannot animate");

        short.resize(&mut fonts.system, PhysicalSize::new(1600, 1000));
        assert_eq!(short.strip_rect.h, 64.0);
        assert!(
            !short.visualizer_live(),
            "growing back does not resume the gesture it missed"
        );

        // The line the band collapses to is ten band heights of window away.
        short.resize(&mut fonts.system, PhysicalSize::new(1600, 639));
        assert_eq!(short.strip_rect.h, 0.0, "a short window gets no line");
        short.resize(&mut fonts.system, PhysicalSize::new(1600, 640));
        assert_eq!(short.strip_rect.h, 64.0, "the band is back with the room");
    }

    /// A window opens quiet: nothing is being typed yet, and the file tree is
    /// not in the way until `-` asks for it.
    #[test]
    fn opens_in_normal_with_the_tree_put_away() {
        let mut fonts = Fonts::load();
        let ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.25,
        );
        assert_eq!(ui.mode, Mode::Normal);
        assert_eq!(ui.focus, Focus::Right);
        assert!(!ui.sidebar.visible);
    }

    /// The tree holds the focus as well as the page does, so the same ask works
    /// from either side of the window.
    #[test]
    fn a_file_tab_closes_with_the_tree_in_focus() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("close.txt");
        std::fs::write(&path, b"x").unwrap();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.0,
        );
        ui.open_file(&mut fonts.system, path).unwrap();
        assert_eq!(ui.tabs.len(), 2, "the file sits beside the shell");

        ui.toggle_sidebar(&mut fonts.system);
        assert_eq!(ui.focus, Focus::Sidebar);
        assert!(
            ui.close_active_file(&mut fonts.system),
            "the tree's focus must not stop the close"
        );
        assert_eq!(ui.tabs.len(), 1, "only the shell tab is left");
    }

    /// A command that makes a file changes the tree, so the listing follows the
    /// moment the command is done — not the next time the panel is closed and
    /// opened again.
    #[test]
    fn a_finished_command_puts_the_new_file_in_the_tree() {
        use std::time::Duration;

        let mut fonts = Fonts::load();
        theme::init();
        let dir = tempfile::tempdir().unwrap();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.0,
        );
        ui.sidebar.visible = true;
        ui.sidebar.root = crate::sidebar::Root::Dir(dir.path().to_path_buf());
        ui.sidebar.git_changed(&mut fonts.system);
        let made = dir.path().join("made.txt");
        assert!(
            !ui.sidebar.rows.iter().any(|row| row.path == made),
            "the file is not there before the command runs"
        );

        let shell = ui.active_shell_mut().expect("the shell tab");
        shell.insert_text(
            &mut fonts.system,
            &format!("cd {}", kite_core::display_path(dir.path())),
        );
        shell.submit(&mut fonts.system);
        let shell = ui.active_shell_mut().expect("the shell tab");
        shell.insert_text(&mut fonts.system, "cmd /c copy nul made.txt");
        assert!(shell.submit(&mut fonts.system).is_none());

        let waited = Instant::now();
        while ui.active_shell().is_some_and(|shell| shell.is_running())
            && waited.elapsed() < Duration::from_secs(10)
        {
            ui.poll_shell(&mut fonts.system);
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(made.is_file(), "the command never made the file");
        assert!(
            ui.sidebar.rows.iter().any(|row| row.path == made),
            "the tree still lists the old contents: {:?}",
            ui.sidebar
                .rows
                .iter()
                .map(|row| row.path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        );
    }

    /// Typing on the command line asks the same question the file page asks, and
    /// gets the same shape back: a list, taken with `Tab`, not one faint guess.
    #[test]
    fn the_command_line_answers_with_the_same_list() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("gcc.txt"), b"").unwrap();
        fs::write(dir.path().join("gccd.txt"), b"").unwrap();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.0,
        );
        let shell = ui.active_shell_mut().expect("the shell tab");
        shell.insert_text(
            &mut fonts.system,
            &format!("cd {}", kite_core::display_path(dir.path())),
        );
        shell.submit(&mut fonts.system);

        let shell = ui.active_shell_mut().expect("the shell tab");
        shell.insert_text(&mut fonts.system, "cat g");
        ui.update_completion();
        assert!(ui.completing(), "typing asked and nothing answered");
        // A frame asks for the shapes before the text, and the panel is placed
        // while it does; this follows the same order rather than the other way.
        ui.completer.quads(&mut fonts.system, (1600.0, 1000.0));
        assert!(
            ui.completer.areas().len() > 1,
            "a list writes its rows, not one run of faint text"
        );

        let before = ui.active_shell().unwrap().input.clone();
        assert!(
            ui.accept_completion(&mut fonts.system),
            "Tab had nothing to take"
        );
        let shell = ui.active_shell().unwrap();
        assert_eq!(shell.input, format!("{before}cc.txt "));
        assert!(!ui.completing(), "a taken answer is gone");
    }

    /// A window the size the tests are drawn at, with the shell tab already in it.
    fn stage(fonts: &mut Fonts) -> Ui {
        let mono = fonts.mono;
        Ui::new(&mut fonts.system, mono, PhysicalSize::new(1600, 1000), 1.0)
    }

    /// A file page does not guess. The keystroke posts a question and a later
    /// frame puts the answer under the word, so this walks the whole chain — the
    /// caret, the range the server named, the word the list is scoped to — with
    /// a reply handed in by the test instead of by a server.
    #[test]
    fn an_answer_from_a_server_opens_the_list_on_the_word() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.c");
        fs::write(&path, b"int main(void) {\n  comp\n}\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(1, 6);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));

        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server, it does not answer for itself");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(
            id,
            json!({"items": [{"label": "compute_thing", "kind": 6,
                              "detail": "int compute_thing(void)"}]}),
        );
        ui.awaiting = Some(Awaiting { id, line });

        assert!(ui.poll_lsp(), "the answer did not reach the screen");
        assert!(ui.completing(), "the answer went up as the whole list");
        assert!(
            ui.accept_completion(&mut fonts.system),
            "Tab had nothing to take"
        );
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(1),
            "  compute_thing ",
            "the four letters already typed are replaced, not doubled"
        );
    }

    /// A name this project already took once is a name it talks about, so the
    /// next list opens on it even though the server listed another name first.
    #[test]
    fn a_name_taken_before_is_offered_first() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.c");
        fs::write(&path, b"int main(void) {\n  p\n}\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(1, 3);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        ui.usage.take("c", "printf");
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(
            id,
            json!({"items": [{"label": "puts", "kind": 3}, {"label": "printf", "kind": 3}]}),
        );
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp());
        assert!(
            ui.accept_completion(&mut fonts.system),
            "Tab had nothing to take"
        );
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(1),
            "  printf ",
            "the name this file took before, not the server's first row"
        );
        assert_eq!(
            ui.usage.weight("c", "printf").0,
            2,
            "taking it makes it more likely next time"
        );
    }

    /// The include is the point: a name that only compiles once its header is
    /// there is a name the server knows the file needs, and the edit rides
    /// along with the answer so the reader never types the `#include` himself.
    #[test]
    fn taking_a_name_brings_the_include_it_needs() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("len.c");
        fs::write(&path, b"int main(void) {\n  str\n}\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(1, 5);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(
            id,
            json!({"items": [{
                "label": "strlen", "kind": 3, "insertText": "strlen",
                "additionalTextEdits": [{
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "newText": "#include <string.h>\n"
                }]
            }]}),
        );
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp());
        assert!(
            ui.accept_completion(&mut fonts.system),
            "Tab had nothing to take"
        );
        let editor = ui.active_editor().unwrap();
        assert_eq!(editor.document.line_text(0), "#include <string.h>");
        assert_eq!(
            editor.document.line_text(2),
            "  strlen ",
            "the word is completed on the line the include pushed down"
        );
        assert_eq!(
            editor.selection.cursor,
            Position::new(2, 9),
            "the caret rides with the line it completes, past its space"
        );
    }

    /// A function is taken whole: its brackets and the places inside them, and
    /// Tab walks those places instead of walking the line.
    #[test]
    fn a_taken_function_is_tabbed_through_its_places() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.c");
        fs::write(&path, b"int main(void) {\n  ru\n}\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(1, 4);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(
            id,
            json!({"items": [{
                "label": "run", "kind": 3, "insertTextFormat": 2,
                "insertText": "run(${1:one}, ${2:two})"
            }]}),
        );
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp());
        assert!(ui.accept_completion(&mut fonts.system));
        {
            let editor = ui.active_editor().unwrap();
            assert_eq!(editor.document.line_text(1), "  run(one, two)");
            assert_eq!(
                editor.selection.ordered(),
                (Position::new(1, 6), Position::new(1, 9)),
                "the first placeholder is selected, ready to be typed over"
            );
        }
        // Typing over the first place leaves the walk intact: the next place
        // moves left with the text it replaced.
        if let Some(editor) = ui.active_editor_mut() {
            editor
                .insert_typed(&mut fonts.system, 'X')
                .expect("a letter");
            assert_eq!(editor.document.line_text(1), "  run(X, two)");
        }
        assert!(ui.snippet_pending(), "the walk ended on the first letter");
        assert!(ui.snippet_tab(&mut fonts.system));
        assert_eq!(
            ui.active_editor().unwrap().selection.ordered(),
            (Position::new(1, 9), Position::new(1, 12)),
            "Tab reaches the second placeholder, counted from the new text"
        );
        assert!(ui.snippet_tab(&mut fonts.system));
        assert_eq!(
            ui.active_editor().unwrap().selection.cursor,
            Position::new(1, 13),
            "the last Tab lands after the call"
        );
        assert!(!ui.snippet_pending(), "the walk is over");
    }

    /// The include answer is a whole directive: it carries the quote that
    /// closes it, and the range reaches over the quote already on the row. The
    /// taken header must end up with one pair of quotes, not two.
    #[test]
    fn a_header_taken_between_the_quotes_closes_them_once() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        // A quoted include is only offered what the folder really has, so this
        // one has the header it is about to name.
        fs::write(dir.path().join("stdio.h"), b"").unwrap();
        let path = dir.path().join("header.c");
        fs::write(&path, b"#include \"\"\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(0, 10);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(
            id,
            json!({"items": [{"label": " stdio.h\"", "kind": 17,
                "textEdit": {"newText": "stdio.h\"", "range": {
                    "start": {"line": 0, "character": 10},
                    "end": {"line": 0, "character": 11}}}}]}),
        );
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp(), "typing the quote asked and nothing showed");
        assert!(
            matches!(ui.completer.target(), Target::Editor { start, end }
                if start == Position::new(0, 10) && end == Position::new(0, 11)),
            "the answer replaces the empty header, quotes included"
        );
        assert!(ui.accept_completion(&mut fonts.system));
        let editor = ui.active_editor().unwrap();
        assert_eq!(
            editor.document.line_text(0),
            "#include \"stdio.h\"",
            "the quote the server sent closes the one already on the row"
        );
        assert_eq!(editor.selection.cursor, Position::new(0, 18));
    }

    /// The header list asked from a file page: the caret is parked where the
    /// reader has just typed the given row, and the answer is handed in by the
    /// test rather than by a server.
    fn ask_headers(ui: &mut Ui, fonts: &mut Fonts, caret: Position, items: serde_json::Value) {
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(id, items);
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp(), "the answer did not reach the screen");
    }

    fn header(new_text: &str, from: usize, to: usize) -> serde_json::Value {
        json!({"items": [{"label": format!(" {new_text}"), "kind": 17,
            "textEdit": {"newText": new_text, "range": {
                "start": {"line": 0, "character": from},
                "end": {"line": 0, "character": to}}}}]})
    }

    /// A bare quote lists what the server answered, in the order it answered.
    /// The client does not second-guess the search path by looking for the files
    /// beside the page: that is the server's job, and a client that filters it
    /// is one more guess the reader has to know about.
    #[test]
    fn a_bare_quote_lists_what_the_server_answered() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("widget.h"), b"int widget(void);\n").unwrap();
        let path = dir.path().join("main.c");
        fs::write(&path, b"#include \"\"\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        ask_headers(
            &mut ui,
            &mut fonts,
            Position::new(0, 10),
            json!({"items": [
                {"label": " stdio.h\"", "kind": 17, "textEdit": {
                    "newText": "stdio.h\"", "range": {
                        "start": {"line": 0, "character": 10},
                        "end": {"line": 0, "character": 11}}}},
                {"label": " widget.h\"", "kind": 17, "textEdit": {
                    "newText": "widget.h\"", "range": {
                        "start": {"line": 0, "character": 10},
                        "end": {"line": 0, "character": 11}}}}
            ]}),
        );
        assert!(ui.completing(), "the quote asked, the list answers");
        assert!(ui.accept_completion(&mut fonts.system), "Tab had nothing to take");
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(0),
            "#include \"stdio.h\"",
            "the server's own first row, with nothing filtered out of it"
        );
    }

    /// The angle bracket is the system header path, and it keeps everything the
    /// search path holds — `stdio.h` is not a file beside the reader's `.c`.
    #[test]
    fn an_angle_bracket_keeps_the_whole_search_path() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.c");
        fs::write(&path, b"#include <>\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        ask_headers(
            &mut ui,
            &mut fonts,
            Position::new(0, 10),
            header("stdio.h>", 10, 11),
        );
        assert!(ui.accept_completion(&mut fonts.system));
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(0),
            "#include <stdio.h>",
            "one pair of brackets, no quotes anywhere"
        );
    }

    /// The word already on the row is the row the reader means: clangd answers
    /// `int` with `inline`, `int` and `intptr_t` in that alphabetical order, and
    /// a list that drops the whole match to avoid repeating the page ends up
    /// offering `inline` for `int`.
    #[test]
    fn the_word_being_typed_is_the_first_row() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("typed.c");
        fs::write(&path, b"int main(void) {\n  int\n}\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(1, 5);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(
            id,
            json!({"items": [{"label": "inline", "kind": 14},
                             {"label": "int", "kind": 13},
                             {"label": "intptr_t", "kind": 9}]}),
        );
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp());
        assert!(ui.accept_completion(&mut fonts.system));
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(1),
            "  int ",
            "the word typed, not the name that merely begins with the same letters"
        );
    }

    /// The answer is a pool, not a one-shot ghost. Each letter typed after it
    /// lands narrows that pool where it stands — no second question, no waiting
    /// — which is the difference between a list that keeps up with typing and
    /// one that arrives after the word is finished.
    #[test]
    fn a_list_narrows_with_the_next_letter_without_asking_again() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("narrow.c");
        fs::write(&path, b"int main(void) {\n  i\n}\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(1, 3);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(
            id,
            json!({"items": [{"label": "inline", "kind": 14},
                             {"label": "int", "kind": 13},
                             {"label": "intptr_t", "kind": 9}]}),
        );
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp());
        assert!(ui.completing());

        for letter in ['n', 't'] {
            ui.active_editor_mut()
                .expect("the file tab")
                .insert_typed(&mut fonts.system, letter)
                .expect("a letter");
            ui.update_completion();
            assert!(ui.completing(), "the list went away on {letter}");
            assert!(
                !ui.lsp_pending(),
                "{letter} asked the server again instead of narrowing the answer it had"
            );
        }
        assert!(ui.accept_completion(&mut fonts.system));
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(1),
            "  int ",
            "the list followed the typing all the way to the word"
        );
    }

    /// A caret that has left the row takes the answer with it. A span taken
    /// from a row the reader is no longer standing in is not a word, and
    /// splicing a name across two rows is worse than doing nothing.
    #[test]
    fn an_answer_is_not_spliced_into_a_row_the_caret_has_left() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moved.c");
        fs::write(&path, b"comp\nsecond\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(0, 4);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { style, .. }) = ui.completion_where() else {
            panic!("a C page asks its server");
        };
        let pool = vec![Item::new("compute_thing", crate::complete::Kind::Word)];
        ui.completer.show(
            &style,
            pool,
            "comp",
            Target::Editor {
                start: Position::new(0, 0),
                end: Position::new(0, 4),
            },
        );
        assert!(ui.completing(), "the list was up when the caret moved");

        let away = Position::new(1, 2);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(away, away));
        assert!(ui.accept_completion(&mut fonts.system), "the key was still eaten");
        let editor = ui.active_editor().unwrap();
        assert_eq!(editor.document.line_text(0), "comp");
        assert_eq!(editor.document.line_text(1), "second");
    }

    /// The server's range can reach past the caret, because the reader is often
    /// standing inside the word he wants changed. Replacing only up to the caret
    /// would leave the old ending glued to the new one.
    #[test]
    fn an_answer_replaces_the_whole_of_a_word_the_caret_is_inside() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.c");
        fs::write(&path, b"strncpy(from, to, n);\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(0, 4);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));

        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(
            id,
            json!({"items": [{"label": "strncat", "insertText": "strncat",
                "textEdit": {"newText": "strncat", "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 7}}}}]}),
        );
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp());
        assert!(
            ui.accept_completion(&mut fonts.system),
            "the list was up but had nothing highlighted"
        );
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(0),
            "strncat(from, to, n);",
            "the whole word was replaced, not the part behind the caret"
        );
    }

    /// The answer is a pool for the span it was asked about, not a ghost for one
    /// column, so typing does not invalidate it — it narrows it. Throwing an
    /// answer away because the caret moved a letter is what made a list arrive
    /// after the word was already finished.
    #[test]
    fn an_answer_that_arrives_while_the_word_grows_still_lands() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("late.c");
        fs::write(&path, b"comp\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(0, 4);
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        ui.servers.fake_answer(id, json!({"items": [{"label": "compute_thing"}]}));
        ui.awaiting = Some(Awaiting { id, line });

        ui.active_editor_mut()
            .expect("the file tab")
            .insert_text(&mut fonts.system, "u")
            .unwrap();
        assert!(ui.poll_lsp(), "the word only grew, so the answer still fits it");
        assert!(ui.accept_completion(&mut fonts.system));
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(0),
            "compute_thing ",
            "and it is filtered against what is on the row now, not what was there"
        );
    }

    /// The screenshot this answers: the caret inside `#include <std>`, where the
    /// row already ends in a `>`. The list may open there — the word is real —
    /// but the server sends the closing bracket along with the name, and the row
    /// must not come out with two of them.
    #[test]
    fn a_name_that_closes_the_bracket_does_not_close_it_twice() {
        let mut fonts = Fonts::load();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("over.c");
        fs::write(&path, b"#include <std>\n").unwrap();
        let mut ui = stage(&mut fonts);
        ui.open_file(&mut fonts.system, path).unwrap();
        let caret = Position::new(0, "#include <std".chars().count());
        ui.active_editor_mut()
            .expect("the file tab")
            .set_selection(&mut fonts.system, Selection::new(caret, caret));
        let Some(Where::File { line, .. }) = ui.completion_where()
        else {
            panic!("a C page asks its server");
        };
        let id = ui.servers.fake_question();
        // clangd's range reaches past the caret and takes the `>` with it, and
        // the name it puts back carries a bracket of its own.
        ui.servers.fake_answer(
            id,
            json!({"items": [{"label": " stdio.h>", "kind": 17,
                "textEdit": {"newText": "stdio.h>", "range": {
                    "start": {"line": 0, "character": 10},
                    "end": {"line": 0, "character": 14}}}}]}),
        );
        ui.awaiting = Some(Awaiting { id, line });
        assert!(ui.poll_lsp(), "a word is being typed, so the list belongs up");
        assert!(ui.accept_completion(&mut fonts.system), "the list had no row");
        assert_eq!(
            ui.active_editor().unwrap().document.line_text(0),
            "#include <stdio.h>",
            "the typed header and the bracket it closes were both replaced"
        );
    }


    #[test]
    fn the_status_bar_counts_characters_only_on_a_file_page() {
        let mut fonts = Fonts::load();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.25,
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("count.txt");
        // "one", "two" and "three" without their separators.
        std::fs::write(&path, b"one\ntwo\nthree\n").unwrap();
        ui.open_file(&mut fonts.system, path.clone()).unwrap();

        ui.prepare(&mut fonts.system);
        assert_eq!(ui.status_right.lines[0].text(), "11 chars");
        assert!(
            ui.status_right_x() + ui.status_right_w
                > ui.status_rect.x + ui.status_rect.w - 40.0 * ui.scale,
            "the count belongs in the corner"
        );
        assert!(
            !ui.status.lines[0].text().contains("chars"),
            "the left text no longer carries the count"
        );

        ui.toggle_sidebar(&mut fonts.system);
        ui.prepare(&mut fonts.system);
        assert!(
            ui.status.lines[0].text().ends_with("[NORMAL]"),
            "the sidebar focus is plain normal: {}",
            ui.status.lines[0].text()
        );

        ui.activate_tab(&mut fonts.system, 0);
        ui.prepare(&mut fonts.system);
        assert_eq!(ui.status_right_w, 0.0, "the shell has no character count");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn digits_count_from_the_shell_and_zero_is_the_tenth() {
        let mut fonts = Fonts::load();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.25,
        );
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"one\n").unwrap();
            ui.open_file(&mut fonts.system, path).unwrap();
        }

        assert!(ui.activate_digit(&mut fonts.system, 1));
        assert_eq!(ui.active, 0, "1 is the shell");
        assert!(ui.activate_digit(&mut fonts.system, 3));
        assert_eq!(ui.active, 2, "b.txt is the third tab");
        assert!(
            !ui.activate_digit(&mut fonts.system, 4),
            "there is no fourth tab"
        );

        ui.open_miku(&mut fonts.system);
        assert_eq!(ui.active, 1);
        assert!(ui.activate_digit(&mut fonts.system, 2));
        assert!(ui.active_miku().is_some(), "2 is the board while it exists");
        assert!(ui.activate_digit(&mut fonts.system, 4));
        assert_eq!(
            ui.tabs[ui.active].title, "b.txt",
            "the board takes a slot, it does not steal one"
        );
        assert!(!ui.activate_digit(&mut fonts.system, 0), "no tenth tab yet");

        for name in [
            "c.txt", "d.txt", "e.txt", "f.txt", "g.txt", "h.txt", "i.txt",
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"one\n").unwrap();
            ui.open_file(&mut fonts.system, path).unwrap();
        }
        assert_eq!(ui.tabs.len(), 11);
        assert!(ui.activate_digit(&mut fonts.system, 0));
        assert_eq!(ui.active, 9, "0 is the tenth tab");
        assert_eq!(ui.tabs[ui.active].title, "h.txt");
    }

    /// The glide belongs to a caret that moved. Scrolling moves the view, and
    /// the caret belongs to the text, so it has to travel with it in the same
    /// frame — easing there is the trail the user reported.
    #[test]
    fn a_scrolled_view_moves_the_caret_without_a_glide() {
        let mut fonts = Fonts::load();
        let mut ui = Ui::new(
            &mut fonts.system,
            fonts.mono,
            PhysicalSize::new(1600, 1000),
            1.25,
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.txt");
        std::fs::write(&path, "line of text\n".repeat(400)).unwrap();
        ui.open_file(&mut fonts.system, path).unwrap();

        ui.caret_tick();
        assert!(!ui.caret_flying(), "the first frame only aims");
        let before = ui.caret.y;

        // One wheel notch: the caret's row slides up under the view, but the
        // caret did not move.
        ui.active_editor_mut().unwrap().scroll_by(&mut fonts.system, 8.0);
        ui.caret_tick();
        assert!(
            !ui.caret_flying(),
            "a scroll is not a caret move, so nothing may ease"
        );
        assert!(
            ui.caret.y < before,
            "the caret travels with the text it sits beside: {before} -> {}",
            ui.caret.y
        );

        // A real move still glides, which is the whole point of the animation.
        ui.active_editor_mut()
            .unwrap()
            .move_right(&mut fonts.system);
        ui.caret_tick();
        assert!(ui.caret_flying(), "a caret move must still glide");
    }

    #[test]
    fn the_caret_glides_rather_than_snapping() {
        // One 90 ms frame closes ~95% of the gap: the caret is well past the
        // midpoint (it is gliding, not stepping) but has not teleported.
        let after_one = caret_step(0.0, 100.0, 0.090);
        assert!(
            after_one > 90.0 && after_one < 100.0,
            "90 ms should cover most but not all of the distance, got {after_one}"
        );

        // A single frame is monotonic and never overshoots the target.
        assert!(caret_step(0.0, 100.0, 0.016) < after_one);
        assert!(caret_step(0.0, 100.0, 10.0) <= 100.0);

        // Repeated frames converge onto the target to within the landing band.
        let mut pos = 0.0;
        for _ in 0..30 {
            pos = caret_step(pos, 100.0, 0.016);
        }
        assert!(
            (100.0 - pos).abs() <= CARET_LANDED,
            "the glide must settle on the caret, stopped at {pos}"
        );
    }
}
