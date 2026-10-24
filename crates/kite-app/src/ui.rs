use std::path::{Path, PathBuf};
use std::sync::Arc;

use glyphon::cosmic_text::{
    Attrs, AttrsList, Buffer, BufferLine, Color, LineEnding, Metrics, Shaping, Weight, Wrap,
};
use glyphon::{FontSystem, TextArea, TextBounds};
use kite_core::{Document, Position, Selection, file_key};
use winit::dpi::PhysicalSize;

use crate::editor::Editor;
use crate::miku::Miku;
use crate::renderer::QuadVertex;
use crate::shell::{Shell, ShellEffect};
use crate::sidebar::{Sidebar, SidebarAction};
use crate::syntax::Engine;
use crate::text::{attrs, make_label, measure};
use crate::theme;

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
    sidebar_rect: Rect,
    status_rect: Rect,
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

        let body_height = (size.height as f32 - titlebar_h - status_h).max(1.0);
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
            13.5 * scale,
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
        );
        let mut ui = Self {
            tabs: vec![shell_tab],
            active: 0,
            mode: Mode::Insert,
            focus: Focus::Right,
            sidebar,
            size,
            scale,
            body_font_size,
            cursor_blink: true,
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
            sidebar_rect: Rect::default(),
            status_rect: Rect::default(),
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

    /// Open the board, or move to the one already open. It always sits at index
    /// one, right after the shell, so the file tabs shift along with it.
    pub fn open_miku(&mut self, font_system: &mut FontSystem) {
        if let Some(index) = self.miku_tab() {
            self.activate_tab(font_system, index);
            self.focus = Focus::Right;
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
        );
        self.tabs.insert(1, tab);
        self.active = 1;
        self.focus = Focus::Right;
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
        self.sidebar.set_font_size(font_system, 13.5 * scale);
        for tab in &mut self.tabs {
            match &mut tab.content {
                TabContent::Shell(shell) => shell.set_font_size(font_system, self.body_font_size),
                TabContent::Editor(editor) => {
                    editor.set_font_size(font_system, self.body_font_size)
                }
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
            self.focus = Focus::Right;
            self.mode = Mode::Normal;
            self.recompute_layout(font_system);
        } else {
            self.sidebar.visible = true;
            self.focus = Focus::Sidebar;
            self.mode = Mode::Normal;
            self.recompute_layout(font_system);
            // Reopening is the moment to ask git again; the viewport size has not
            // changed, so nothing else would rebuild the listing.
            self.sidebar.git_changed(font_system);
            self.sidebar.shape(font_system);
        }
    }

    pub fn focus_right_insert(&mut self, font_system: &mut FontSystem) {
        self.focus = Focus::Right;
        self.mode = Mode::Insert;
        if let Some(editor) = self.active_editor_mut() {
            editor.ensure_cursor_visible(font_system);
        }
    }

    pub fn focus_sidebar(&mut self, font_system: &mut FontSystem) {
        if self.sidebar.visible {
            self.focus = Focus::Sidebar;
            self.mode = Mode::Normal;
            self.sidebar.shape(font_system);
        }
    }

    pub fn escape(&mut self, font_system: &mut FontSystem) {
        if self.mode == Mode::Insert {
            self.mode = Mode::Normal;
            self.focus = if self.sidebar.visible {
                Focus::Sidebar
            } else {
                Focus::Right
            };
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
        let previous = self.active;
        self.active = index;
        self.message = None;
        self.refresh_label(font_system, previous);
        self.refresh_label(font_system, index);
        self.recompute_layout(font_system);
    }

    pub fn activate_digit(&mut self, font_system: &mut FontSystem, digit: u8) -> bool {
        if digit == 0 {
            if self.active != 0 {
                self.activate_tab(font_system, 0);
                return true;
            }
            return false;
        }
        let mut file_index = 0;
        for (index, tab) in self.tabs.iter().enumerate() {
            if tab.is_shell() {
                continue;
            }
            file_index += 1;
            if file_index == digit as usize {
                if self.active != index {
                    self.activate_tab(font_system, index);
                }
                return true;
            }
        }
        false
    }

    pub fn close_tab(&mut self, font_system: &mut FontSystem, index: usize) -> bool {
        if index == 0 || index >= self.tabs.len() {
            return false;
        }
        let was_active = index == self.active;
        self.tabs.remove(index);
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
        if self.focus != Focus::Right || self.mode != Mode::Normal {
            return false;
        }
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
            self.body_font_size,
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
        );
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.focus = Focus::Right;
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
    /// is not the page in front.
    fn note_save(&mut self, font_system: &mut FontSystem, path: &Path) {
        let text = kite_core::display_path(path);
        for tab in &mut self.tabs {
            if let TabContent::Miku(miku) = &mut tab.content {
                miku.record_save(font_system, &text);
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
        self.focus = Focus::Right;
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

    pub fn adjust_body_font(&mut self, font_system: &mut FontSystem, delta: f32) {
        let min = theme::MIN_FONT_SIZE * self.scale;
        let max = theme::MAX_FONT_SIZE * self.scale;
        let size = (self.body_font_size + delta * self.scale).clamp(min, max);
        if (size - self.body_font_size).abs() < 0.01 {
            return;
        }
        self.body_font_size = size;
        for tab in &mut self.tabs {
            match &mut tab.content {
                TabContent::Shell(shell) => shell.set_font_size(font_system, size),
                TabContent::Editor(editor) => editor.set_font_size(font_system, size),
                // The board sizes its own type from the panel, so Ctrl++/Ctrl+-
                // does not reach it.
                TabContent::Miku(_) => {}
            }
        }
        self.message = Some(format!("text size {:.0}px", size / self.scale));
    }

    pub fn poll_shell(&mut self, font_system: &mut FontSystem) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            if let TabContent::Shell(shell) = &mut tab.content {
                if shell.poll(font_system) {
                    changed = true;
                }
            }
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
    /// text, scroll and caret are left exactly as they were.
    pub fn retheme(&mut self, font_system: &mut FontSystem) {
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
                TabContent::Shell(shell) => shell.retheme(font_system),
                TabContent::Editor(editor) => editor.retheme(font_system),
                TabContent::Miku(miku) => miku.retheme(font_system),
            }
        }
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
                    theme::text()
                } else {
                    theme::text_dim()
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
                    default_color: theme::text_dim(),
                    custom_glyphs: &[],
                });
                let text = self.body_text_rect();
                areas.push(TextArea {
                    buffer: &editor.buffer,
                    left: text.x,
                    top: text.y,
                    scale: 1.0,
                    bounds: bounds_of(text),
                    default_color: theme::text(),
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
                    default_color: theme::text(),
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
                        default_color: theme::text(),
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
                default_color: theme::text(),
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

        areas
    }

    pub fn background_quads(&self) -> Vec<QuadVertex> {
        let mut quads = Vec::new();
        push_rect(&mut quads, self.right, theme::panel());
        push_rect(&mut quads, self.status_rect, theme::status());
        push_rect(
            &mut quads,
            Rect {
                x: self.right.x,
                y: self.titlebar_h,
                w: self.size.width as f32,
                h: 1.0 * self.scale,
            },
            theme::border(),
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
            push_rect(&mut quads, self.sidebar_rect, theme::sidebar());
            push_rect(
                &mut quads,
                Rect {
                    x: self.sidebar_rect.right() - 1.0 * self.scale,
                    y: self.sidebar_rect.y,
                    w: 1.0 * self.scale,
                    h: self.sidebar_rect.h,
                },
                theme::border(),
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
                            theme::accent_dim(),
                        );
                    }
                    let _ = row;
                }
            }
        }

        push_rect(&mut quads, self.tabbar_rect(), theme::panel_dim());
        for (index, tab) in self.tabs.iter().enumerate() {
            let shown = tab.rect.clip_x(self.tab_strip.x, self.tab_strip.right());
            if shown.w <= 1.0 {
                continue;
            }
            let color = if index == self.active {
                theme::tab_active()
            } else if self.hover_tab(index) {
                theme::tab_hover()
            } else {
                theme::panel_dim()
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
                push_rect(&mut quads, underline, theme::accent());
            }
            if let Some(close) = tab.close_rect {
                let color = if self.hover_close(index) {
                    theme::red()
                } else {
                    theme::text_faint()
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
                        theme::selection(),
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
                        theme::selection(),
                    );
                }
            }
        }

        quads
    }

    pub fn foreground_quads(&self) -> Vec<QuadVertex> {
        let mut quads = Vec::new();
        let content = self.body_text_rect();

        if self.show_whitespace {
            match &self.active_tab().content {
                TabContent::Editor(editor) => {
                    let line_height = editor.line_height();
                    let breaks = |line: usize| editor.document.has_line_break(line);
                    push_whitespace_markers(
                        &mut quads,
                        &editor.buffer,
                        content,
                        self.body_font_size,
                        line_height,
                        &breaks,
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
                    );
                }
                // The board has no text of its own to mark up.
                TabContent::Miku(_) => {}
            }
        }

        if self.focus == Focus::Sidebar && self.sidebar.visible {
            push_rect(&mut quads, self.right, theme::overlay_dim());
        } else if self.focus == Focus::Right && self.sidebar.visible {
            push_rect(&mut quads, self.sidebar_rect, theme::overlay_dim());
        }

        if self.focus == Focus::Right && (self.mode == Mode::Normal || self.cursor_blink) {
            let cursor = match &self.active_tab().content {
                TabContent::Editor(editor) => editor.cursor_rect(),
                TabContent::Shell(shell) => shell.cursor_rect(),
                TabContent::Miku(_) => None,
            };
            if let Some((x, y, height)) = cursor {
                push_rect(
                    &mut quads,
                    Rect {
                        x: content.x + x,
                        y: content.y + y + 2.0 * self.scale,
                        w: 2.0 * self.scale,
                        h: (height - 4.0 * self.scale).max(2.0),
                    },
                    theme::cursor(),
                );
            }
        }

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
                format!(
                    "{}   {}   Ln {}, Col {}",
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
        let color = if active {
            theme::text()
        } else {
            theme::text_dim()
        };
        let font_size = self.tab_font_size;
        let delta = match &mut tab.content {
            TabContent::Editor(editor) => editor.line_delta(),
            TabContent::Shell(_) => (0, 0),
            TabContent::Miku(_) => (0, 0),
        };
        let title = tab.title.clone();
        tab.label_delta = delta;
        tab.label = make_counted_label(font_system, self.family, &title, delta, color, font_size);
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
        let body_h = (height - self.titlebar_h - self.status_h).max(1.0);
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
            self.sidebar_w = (280.0 * self.scale).min(width * 0.45);
            self.sidebar_rect = Rect {
                x: 0.0,
                y: self.titlebar_h,
                w: self.sidebar_w,
                h: body_h,
            };
            self.right = Rect {
                x: self.sidebar_w,
                y: self.titlebar_h,
                w: (width - self.sidebar_w).max(1.0),
                h: body_h,
            };
        } else {
            self.sidebar_w = 0.0;
            self.sidebar_rect = Rect::default();
            self.right = Rect {
                x: 0.0,
                y: self.titlebar_h,
                w: width.max(1.0),
                h: body_h,
            };
        }

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

fn make_tab(
    font_system: &mut FontSystem,
    family: &'static str,
    font_size: f32,
    tab_pad: f32,
    close_w: f32,
    shell_pad: f32,
    title: &str,
    content: TabContent,
) -> Tab {
    let attr = attrs(family, theme::text_dim());
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
        list.add_span(plus..plus_end, &attrs(family, theme::green()));
        list.add_span(minus..minus_end, &attrs(family, theme::red()));
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

fn push_rect(quads: &mut Vec<QuadVertex>, rect: Rect, color: Color) {
    push_round_rect(quads, rect, color, 0.0);
}

fn push_round_rect(quads: &mut Vec<QuadVertex>, rect: Rect, color: Color, radius: f32) {
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let [r, g, b, a] = color.as_rgba();
    let color = [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ];
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

fn push_line(
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
    let [r, g, b, a] = color.as_rgba();
    let color = [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ];
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

fn push_whitespace_markers(
    quads: &mut Vec<QuadVertex>,
    buffer: &Buffer,
    origin: Rect,
    font_size: f32,
    line_height: f32,
    has_break: &dyn Fn(usize) -> bool,
) {
    let dot_color = theme::whitespace_dot();
    let mark_color = theme::whitespace_mark();
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
}
