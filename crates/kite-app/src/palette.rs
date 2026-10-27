//! Engineering-only palette overlay: live RGBA sliders for every theme color,
//! dim/blur/unfocus sliders, a background image path field and the theme row
//! (load / apply / save / save-as buttons). Toggled with Ctrl+F12. The whole
//! feature is this file plus the lines marked "engineering palette" elsewhere;
//! it is meant to be ripped out whole.

use std::path::PathBuf;

use arboard::Clipboard;
use glyphon::cosmic_text::{Attrs, Buffer, Family, Weight};
use glyphon::{Color, FontSystem, TextArea, TextBounds};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};

use crate::renderer::QuadVertex;
use crate::text::{attrs, char_to_byte, make_label};
use crate::theme;
use crate::ui::{Rect, push_line, push_rect};

const PANEL_W: f32 = 340.0;
const PAD: f32 = 10.0;
const HEADER_H: f32 = 28.0;
const COLOR_ROW_H: f32 = 47.0;
const CTRL_ROW_H: f32 = 35.0;
const IMG_ROW_H: f32 = 48.0;
/// The theme row carries a field and four buttons.
const THEME_ROW_H: f32 = 72.0;
const SLIDER_X: f32 = 118.0;
const SWATCH_W: f32 = 26.0;
const SCROLLBAR_W: f32 = 4.0;

/// Fixed chrome colours: the panel stays readable whatever the user does to
/// the theme with it.
const PANEL_BG: Color = Color::rgba(0x14, 0x14, 0x1c, 0xf2);
const PANEL_BORDER: Color = Color::rgba(0x6c, 0x70, 0x86, 0x90);
const LABEL: Color = Color::rgb(0xcd, 0xd6, 0xf4);
const VALUE: Color = Color::rgb(0x89, 0xb4, 0xfa);
const TRACK: Color = Color::rgba(0xff, 0xff, 0xff, 0x1c);
const FILL: Color = Color::rgba(0xff, 0xff, 0xff, 0x70);
const THUMB: Color = Color::rgb(0xff, 0xff, 0xff);
const FIELD_BG: Color = Color::rgba(0xff, 0xff, 0xff, 0x10);

struct ColorRow {
    name: &'static str,
    get: fn() -> Color,
    set: fn(&mut theme::Colors, Color),
}

const COLOR_ROWS: &[ColorRow] = &[
    ColorRow { name: "bg", get: theme::bg, set: |c, v| c.bg = v },
    ColorRow { name: "panel", get: theme::panel, set: |c, v| c.panel = v },
    ColorRow { name: "panel_dim", get: theme::panel_dim, set: |c, v| c.panel_dim = v },
    ColorRow { name: "sidebar", get: theme::sidebar, set: |c, v| c.sidebar = v },
    ColorRow { name: "status", get: theme::status, set: |c, v| c.status = v },
    ColorRow { name: "border", get: theme::border, set: |c, v| c.border = v },
    ColorRow { name: "text", get: theme::text, set: |c, v| c.text = v },
    ColorRow { name: "text_dim", get: theme::text_dim, set: |c, v| c.text_dim = v },
    ColorRow { name: "status_text", get: theme::status_text, set: |c, v| c.status_text = v },
    ColorRow { name: "text_faint", get: theme::text_faint, set: |c, v| c.text_faint = v },
    ColorRow { name: "accent", get: theme::accent, set: |c, v| c.accent = v },
    ColorRow { name: "accent_dim", get: theme::accent_dim, set: |c, v| c.accent_dim = v },
    ColorRow { name: "pink", get: theme::pink, set: |c, v| c.pink = v },
    ColorRow { name: "green", get: theme::green, set: |c, v| c.green = v },
    ColorRow { name: "red", get: theme::red, set: |c, v| c.red = v },
    ColorRow { name: "yellow", get: theme::yellow, set: |c, v| c.yellow = v },
    ColorRow { name: "selection", get: theme::selection, set: |c, v| c.selection = v },
    ColorRow { name: "tab_active", get: theme::tab_active, set: |c, v| c.tab_active = v },
    ColorRow { name: "tab_hover", get: theme::tab_hover, set: |c, v| c.tab_hover = v },
    ColorRow { name: "cursor", get: theme::cursor, set: |c, v| c.cursor = v },
    ColorRow { name: "whitespace_dot", get: theme::whitespace_dot, set: |c, v| c.whitespace_dot = v },
    ColorRow { name: "whitespace_mark", get: theme::whitespace_mark, set: |c, v| c.whitespace_mark = v },
    ColorRow { name: "miku_0", get: theme::miku_0, set: |c, v| c.miku_0 = v },
    ColorRow { name: "miku_1", get: theme::miku_1, set: |c, v| c.miku_1 = v },
    ColorRow { name: "miku_2", get: theme::miku_2, set: |c, v| c.miku_2 = v },
    ColorRow { name: "miku_3", get: theme::miku_3, set: |c, v| c.miku_3 = v },
    ColorRow { name: "miku_4", get: theme::miku_4, set: |c, v| c.miku_4 = v },
    ColorRow { name: "miku_5", get: theme::miku_5, set: |c, v| c.miku_5 = v },
];

const COLOR_COUNT: usize = COLOR_ROWS.len();
const DIM_ROW: usize = COLOR_COUNT;
const BLUR_ROW: usize = COLOR_COUNT + 1;
const UNFOCUS_ROW: usize = COLOR_COUNT + 2;
const IMG_ROW: usize = COLOR_COUNT + 3;
const THEME_ROW: usize = COLOR_COUNT + 4;
const STATUS_ROW: usize = COLOR_COUNT + 5;
const ROW_COUNT: usize = COLOR_COUNT + 6;

const IMAGE_PLACEHOLDER: &str = "path - Enter applies, empty = embedded";
const THEME_PLACEHOLDER: &str = "name - 导入 loads, 另存 saves";

const THEME_BUTTONS: [&str; 4] = ["导入", "应用", "保存", "另存"];

#[derive(Clone, Copy, PartialEq)]
enum DragTarget {
    Scroll,
    Slider { index: usize, channel: usize },
}

struct Row {
    label: Buffer,
    shown: String,
}

/// One editable line of the panel. The image path and the theme name share
/// the mechanism; Enter means something different to each.
struct Field {
    text: String,
    caret: usize,
    focused: bool,
    buffer: Buffer,
    shown: Option<(String, usize, bool)>,
}

impl Field {
    fn rebuild(
        &mut self,
        font_system: &mut FontSystem,
        family: &'static str,
        scale: f32,
        placeholder: &str,
    ) {
        let shown = if self.text.is_empty() && !self.focused {
            placeholder.to_string()
        } else {
            let mut text = self.text.clone();
            if self.focused {
                let byte = char_to_byte(&self.text, self.caret);
                text.insert(byte, '|');
            }
            text
        };
        self.buffer = make_label(font_system, &attrs(family, LABEL), 12.0 * scale, &shown);
        self.shown = Some((self.text.clone(), self.caret, self.focused));
    }

    fn insert(&mut self, text: &str) {
        let byte = char_to_byte(&self.text, self.caret);
        self.text.insert_str(byte, text);
        self.caret += text.chars().count();
    }

    fn backspace(&mut self) {
        if self.caret > 0 {
            self.caret -= 1;
            self.delete();
        }
    }

    fn delete(&mut self) {
        if self.caret < self.text.chars().count() {
            let start = char_to_byte(&self.text, self.caret);
            let end = char_to_byte(&self.text, self.caret + 1);
            self.text.replace_range(start..end, "");
        }
    }

    fn home(&mut self) {
        self.caret = 0;
    }

    fn end(&mut self) {
        self.caret = self.text.chars().count();
    }
}

pub struct Palette {
    pub open: bool,
    family: &'static str,
    scale: f32,
    scroll: f32,
    drag: Option<DragTarget>,
    image_field: Field,
    theme_field: Field,
    status: Option<String>,
    /// Set while a drag recolours: buffers that baked the old colours in are
    /// repainted once on release, not on every mouse move.
    repaint: bool,
    /// Set when the wallpaper itself changed (blur, image): the renderer
    /// reloads the texture once on release.
    reload: bool,
    rows: Vec<Row>,
    row_rects: Vec<Rect>,
    title: Buffer,
    /// The 导入 / 应用 / 保存 / 另存 labels of the theme row.
    buttons: Vec<Buffer>,
}

impl Palette {
    pub fn new(font_system: &mut FontSystem, family: &'static str, scale: f32) -> Self {
        let title_attrs = Attrs::new()
            .family(Family::Name(family))
            .color(LABEL)
            .weight(Weight::BOLD);
        let title = make_label(
            font_system,
            &title_attrs,
            13.0 * scale,
            "palette - engineering",
        );
        let buttons = THEME_BUTTONS
            .iter()
            .map(|name| {
                make_label(
                    font_system,
                    &attrs(family, LABEL),
                    12.0 * scale,
                    name,
                )
            })
            .collect();
        let rows = (0..ROW_COUNT)
            .map(|_| Row {
                label: make_label(font_system, &attrs(family, LABEL), 12.0 * scale, ""),
                shown: String::new(),
            })
            .collect();
        let empty = |font_system: &mut FontSystem| {
            make_label(font_system, &attrs(family, LABEL), 12.0 * scale, "")
        };
        let path = theme::background_image()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let name = theme::current_name();
        let image_field = Field {
            caret: path.chars().count(),
            text: path,
            focused: false,
            buffer: empty(font_system),
            shown: None,
        };
        let theme_field = Field {
            caret: name.chars().count(),
            text: name,
            focused: false,
            buffer: empty(font_system),
            shown: None,
        };
        Self {
            open: false,
            family,
            scale,
            scroll: 0.0,
            drag: None,
            image_field,
            theme_field,
            status: None,
            repaint: false,
            reload: false,
            rows,
            row_rects: Vec::new(),
            title,
            buttons,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.drag = None;
        if !self.open {
            self.image_field.focused = false;
            self.theme_field.focused = false;
        }
    }

    /// True while one of the fields eats the keyboard; the app gates on this
    /// so typing a path never reaches the shell.
    pub fn text_active(&self) -> bool {
        self.open && (self.image_field.focused || self.theme_field.focused)
    }

    fn focused_field(&mut self) -> Option<&mut Field> {
        if self.image_field.focused {
            Some(&mut self.image_field)
        } else if self.theme_field.focused {
            Some(&mut self.theme_field)
        } else {
            None
        }
    }

    /// Point the text fields at the theme actually running now.
    pub fn sync(&mut self) {
        if !self.image_field.focused {
            self.image_field.text = theme::background_image()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            self.image_field.caret = self.image_field.text.chars().count();
        }
        if !self.theme_field.focused {
            self.theme_field.text = theme::current_name();
            self.theme_field.caret = self.theme_field.text.chars().count();
        }
    }

    pub fn over(&self, size: (f32, f32), x: f32, y: f32) -> bool {
        self.open && self.panel_rect(size).contains(x, y)
    }

    pub fn panel_rect(&self, size: (f32, f32)) -> Rect {
        let w = (PANEL_W * self.scale).min(size.0);
        Rect {
            x: size.0 - w,
            y: 0.0,
            w,
            h: size.1,
        }
    }

    /// Mouse press while the panel is open. Returns true when the panel took
    /// the click; a click outside it passes through to the app.
    pub fn press(&mut self, font_system: &mut FontSystem, size: (f32, f32), x: f32, y: f32) -> bool {
        if !self.open {
            return false;
        }
        let panel = self.panel_rect(size);
        if !panel.contains(x, y) {
            self.image_field.focused = false;
            self.theme_field.focused = false;
            return false;
        }
        let s = self.scale;
        let local_x = x - panel.x;
        let local_y = y - panel.y;
        if local_x > panel.w - (PAD + SCROLLBAR_W + 2.0) * s {
            self.drag = Some(DragTarget::Scroll);
            self.scroll_to(local_y, panel.h);
            return true;
        }
        self.image_field.focused = false;
        self.theme_field.focused = false;
        if local_y < HEADER_H * s {
            return true;
        }
        let Some(index) = self.row_at(local_y) else {
            return true;
        };
        let row = self.row_rect(index, panel);
        if index < COLOR_COUNT {
            for channel in 0..4 {
                let track = Self::slider_track(row, channel, s);
                if Self::grown(track, 4.0 * s).contains(x, y) {
                    self.drag = Some(DragTarget::Slider { index, channel });
                    self.apply(index, channel, x, track);
                    return true;
                }
            }
            return true;
        }
        if index == DIM_ROW || index == BLUR_ROW || index == UNFOCUS_ROW {
            let track = Self::slider_track(row, 0, s);
            if Self::grown(track, 4.0 * s).contains(x, y) {
                self.drag = Some(DragTarget::Slider { index, channel: 0 });
                self.apply(index, 0, x, track);
            }
            return true;
        }
        if index == IMG_ROW && Self::field_rect(row, s).contains(x, y) {
            self.image_field.focused = true;
            self.image_field.caret = self.image_field.text.chars().count();
            self.image_field
                .rebuild(font_system, self.family, s, IMAGE_PLACEHOLDER);
        } else if index == THEME_ROW {
            for button in 0..THEME_BUTTONS.len() {
                if Self::button_rect(row, button, s).contains(x, y) {
                    self.press_button(button);
                    return true;
                }
            }
            if Self::field_rect(row, s).contains(x, y) {
                self.theme_field.focused = true;
                self.theme_field.caret = self.theme_field.text.chars().count();
                self.theme_field
                    .rebuild(font_system, self.family, s, THEME_PLACEHOLDER);
            }
        }
        true
    }

    fn press_button(&mut self, button: usize) {
        match button {
            0 => self.import_theme(),
            1 => self.apply_current(),
            2 => self.save_theme(),
            _ => self.save_theme_as(),
        }
    }

    pub fn drag_to(&mut self, size: (f32, f32), x: f32, y: f32) {
        let Some(drag) = self.drag else {
            return;
        };
        let panel = self.panel_rect(size);
        match drag {
            DragTarget::Scroll => self.scroll_to(y - panel.y, panel.h),
            DragTarget::Slider { index, channel } => {
                let row = self.row_rect(index, panel);
                let track = Self::slider_track(row, channel, self.scale);
                self.apply(index, channel, x, track);
            }
        }
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    pub fn wheel(&mut self, size: (f32, f32), lines: f32) {
        let panel = self.panel_rect(size);
        self.scroll -= lines * 3.0 * 22.0 * self.scale;
        self.clamp_scroll(panel.h);
    }

    /// Keys while the path field is focused. Returns true when eaten.
    pub fn key(
        &mut self,
        font_system: &mut FontSystem,
        clipboard: &mut Option<Clipboard>,
        event: &KeyEvent,
        ctrl: bool,
    ) -> bool {
        if event.state != ElementState::Pressed || !self.text_active() {
            return false;
        }
        match &event.logical_key {
            Key::Named(named) => self.named_key(font_system, *named),
            Key::Character(value) => self.char_key(clipboard, value.as_str(), ctrl),
            _ => false,
        }
    }

    fn named_key(&mut self, font_system: &mut FontSystem, named: NamedKey) -> bool {
        let _ = font_system;
        match named {
            NamedKey::Escape => {
                self.open = false;
                self.image_field.focused = false;
                self.theme_field.focused = false;
            }
            NamedKey::Enter => self.apply_image(),
            NamedKey::Backspace => self.edit_field(|field| field.backspace()),
            NamedKey::Delete => self.edit_field(|field| field.delete()),
            NamedKey::ArrowLeft => {
                self.edit_field(|field| field.caret = field.caret.saturating_sub(1))
            }
            NamedKey::ArrowRight => self.edit_field(|field| {
                field.caret = (field.caret + 1).min(field.text.chars().count())
            }),
            NamedKey::Home => self.edit_field(|field| field.home()),
            NamedKey::End => self.edit_field(|field| field.end()),
            _ => return false,
        }
        true
    }

    fn edit_field(&mut self, f: impl FnOnce(&mut Field)) {
        if let Some(field) = self.focused_field() {
            f(field);
        }
    }

    fn char_key(
        &mut self,
        clipboard: &mut Option<Clipboard>,
        value: &str,
        ctrl: bool,
    ) -> bool {
        if ctrl && (value == "v" || value == "V") {
            if clipboard.is_none() {
                *clipboard = Clipboard::new().ok();
            }
            if let Some(pasted) = clipboard
                .as_mut()
                .and_then(|clipboard| clipboard.get_text().ok())
            {
                let clean: String = pasted.chars().filter(|ch| !ch.is_control()).collect();
                self.edit_field(|field| field.insert(&clean));
            }
        } else if !ctrl {
            let typed: String = value.chars().filter(|ch| !ch.is_control()).collect();
            if !typed.is_empty() {
                self.edit_field(|field| field.insert(&typed));
            }
        } else {
            return false;
        }
        true
    }

    /// The deferred half of a finished interaction: returns whether baked
    /// buffers need a repaint and whether the wallpaper texture needs a
    /// reload, so a slider drag stays smooth and the cost lands on release.
    pub fn post(&mut self) -> (bool, bool) {
        (
            std::mem::take(&mut self.repaint),
            std::mem::take(&mut self.reload),
        )
    }

    fn apply_image(&mut self) {
        let text = self.image_field.text.trim().to_string();
        if text.is_empty() {
            theme::debug_edit(|theme| theme.background.image = None);
            self.status = Some("embedded wallpaper".to_string());
            self.reload = true;
            return;
        }
        let path = PathBuf::from(&text);
        if !path.is_file() {
            self.status = Some(format!("no such file: {text}"));
            return;
        }
        theme::debug_edit(|theme| theme.background.image = Some(path));
        self.status = Some("wallpaper set".to_string());
        self.reload = true;
    }

    /// 导入: load the named theme into this session only. What the next
    /// start loads stays the 应用 button's (and `theme`'s) business.
    fn import_theme(&mut self) {
        let name = self.theme_field.text.trim().to_string();
        if name.is_empty() {
            self.status = Some("type a theme name first".to_string());
            return;
        }
        match theme::load_live(&name) {
            Ok(applied) => {
                self.status = Some(format!("loaded {applied} for this session"));
                self.repaint = true;
                self.reload = true;
                self.sync();
            }
            Err(error) => self.status = Some(error),
        }
    }

    /// 应用: make the theme running now the one the next start loads.
    fn apply_current(&mut self) {
        let name = theme::current_name();
        match theme::apply_record(&name) {
            Ok(_) => self.status = Some(format!("next start: {name}")),
            Err(error) => self.status = Some(error),
        }
    }

    /// 保存: write the running theme, palette edits included, back over its
    /// own file. A preset has no file to overwrite.
    fn save_theme(&mut self) {
        let name = theme::current_name();
        if theme::is_preset(&name) {
            self.status = Some(format!("`{name}` is a preset; type a name and use 另存"));
            return;
        }
        match theme::save_current(&name) {
            Ok(path) => self.status = Some(format!("saved {}", path.display())),
            Err(error) => self.status = Some(error),
        }
    }

    /// 另存: write the running theme to the name typed in the field. The
    /// field keeps that name, so 导入 can load what was just saved.
    fn save_theme_as(&mut self) {
        let name = self.theme_field.text.trim().to_string();
        if name.is_empty() {
            self.status = Some("type a name for the new theme first".to_string());
            return;
        }
        match theme::save_current(&name) {
            Ok(path) => self.status = Some(format!("saved {}", path.display())),
            Err(error) => self.status = Some(error),
        }
    }

    fn apply(&mut self, index: usize, channel: usize, x: f32, track: Rect) {
        let fraction = ((x - track.x) / track.w.max(1.0)).clamp(0.0, 1.0);
        if index < COLOR_COUNT {
            let row = &COLOR_ROWS[index];
            let [r, g, b, a] = (row.get)().as_rgba();
            let value = (fraction * 255.0).round() as u8;
            let color = match channel {
                0 => Color::rgba(value, g, b, a),
                1 => Color::rgba(r, value, b, a),
                2 => Color::rgba(r, g, value, a),
                _ => Color::rgba(r, g, b, value),
            };
            let set = row.set;
            theme::debug_edit(|theme| set(&mut theme.colors, color));
            // bg doubles as the clear colour, so its alpha never shows; skip
            // the buffer repaint for a channel nobody can see.
            self.repaint |= index != 0 || channel < 3;
        } else if index == DIM_ROW {
            theme::debug_edit(|theme| theme.background.dim = fraction);
        } else if index == BLUR_ROW {
            let blur = (fraction * 64.0).round() as u32;
            if blur != theme::background_blur() {
                theme::debug_edit(|theme| theme.background.blur = blur);
                // Re-filtering the full image is far too slow per mouse move;
                // the texture reloads once when the drag ends.
                self.reload = true;
            }
        } else if index == UNFOCUS_ROW {
            theme::debug_edit(|theme| theme.unfocus = fraction);
            // The resolver paints the baked buffers through this strength, so
            // the repaint on release is what lands the new shades.
            self.repaint = true;
        }
    }

    fn scroll_to(&mut self, local_y: f32, panel_h: f32) {
        let s = self.scale;
        let top = HEADER_H * s;
        let view_h = (panel_h - top).max(1.0);
        let content = self.content_height();
        if content <= view_h {
            self.scroll = 0.0;
            return;
        }
        let track_h = view_h - 8.0 * s;
        let thumb_h = (view_h / content * track_h).max(24.0 * s);
        let free = (track_h - thumb_h).max(1.0);
        let t = (local_y - top - thumb_h / 2.0).clamp(0.0, free) / free;
        self.scroll = t * (content - view_h);
    }

    fn clamp_scroll(&mut self, panel_h: f32) {
        let view_h = (panel_h - HEADER_H * self.scale).max(1.0);
        self.scroll = self
            .scroll
            .clamp(0.0, (self.content_height() - view_h).max(0.0));
    }

    fn content_height(&self) -> f32 {
        let s = self.scale;
        (COLOR_COUNT as f32 * COLOR_ROW_H
            + 3.0 * CTRL_ROW_H
            + IMG_ROW_H
            + THEME_ROW_H
            + CTRL_ROW_H
            + 2.0 * PAD)
            * s
    }

    fn row_height(index: usize) -> f32 {
        if index < COLOR_COUNT {
            COLOR_ROW_H
        } else if index == IMG_ROW {
            IMG_ROW_H
        } else if index == THEME_ROW {
            THEME_ROW_H
        } else {
            CTRL_ROW_H
        }
    }

    fn row_top(&self, index: usize) -> f32 {
        let mut y = 0.0;
        for i in 0..index {
            y += Self::row_height(i);
        }
        y * self.scale
    }

    fn row_at(&self, local_y: f32) -> Option<usize> {
        let s = self.scale;
        let top = HEADER_H * s;
        if local_y < top {
            return None;
        }
        let y = local_y - top + self.scroll;
        if y > self.content_height() {
            return None;
        }
        let mut acc = 0.0;
        for index in 0..ROW_COUNT {
            let h = Self::row_height(index) * s;
            if y < acc + h {
                return Some(index);
            }
            acc += h;
        }
        None
    }

    fn row_rect(&self, index: usize, panel: Rect) -> Rect {
        let s = self.scale;
        Rect {
            x: panel.x + PAD * s,
            y: panel.y + HEADER_H * s - self.scroll + self.row_top(index),
            w: panel.w - (PAD * 2.0 + SCROLLBAR_W + 4.0) * s,
            h: Self::row_height(index) * s,
        }
    }

    fn slider_track(row: Rect, line: usize, s: f32) -> Rect {
        Rect {
            x: row.x + SLIDER_X * s,
            y: row.y + (21.0 + line as f32 * 7.0) * s,
            w: (row.w - SLIDER_X * s).max(1.0),
            h: 5.0 * s,
        }
    }

    fn field_rect(row: Rect, s: f32) -> Rect {
        Rect {
            x: row.x,
            y: row.y + 17.0 * s,
            w: row.w,
            h: 22.0 * s,
        }
    }

    fn button_rect(row: Rect, index: usize, s: f32) -> Rect {
        Rect {
            x: row.x + index as f32 * 66.0 * s,
            y: row.y + 44.0 * s,
            w: 60.0 * s,
            h: 22.0 * s,
        }
    }

    fn grown(rect: Rect, amount: f32) -> Rect {
        Rect {
            x: rect.x - amount,
            y: rect.y - amount,
            w: rect.w + amount * 2.0,
            h: rect.h + amount * 2.0,
        }
    }

    fn value_text(index: usize) -> String {
        if index < COLOR_COUNT {
            let [r, g, b, a] = (COLOR_ROWS[index].get)().as_rgba();
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        } else if index == DIM_ROW {
            format!("dim {:.2}", theme::background_dim())
        } else if index == BLUR_ROW {
            format!("blur {} px", theme::background_blur())
        } else if index == UNFOCUS_ROW {
            format!("unfocus {:.2}", theme::unfocus())
        } else if index == IMG_ROW {
            "image".to_string()
        } else if index == THEME_ROW {
            theme::current_name()
        } else {
            String::new()
        }
    }

    fn row_name(index: usize) -> &'static str {
        if index < COLOR_COUNT {
            COLOR_ROWS[index].name
        } else if index == DIM_ROW {
            "background.dim"
        } else if index == BLUR_ROW {
            "background.blur"
        } else if index == UNFOCUS_ROW {
            "style.unfocus"
        } else if index == IMG_ROW {
            "background.image"
        } else if index == THEME_ROW {
            "theme"
        } else {
            ""
        }
    }

    fn geometry(&mut self, font_system: &mut FontSystem) {
        for index in 0..ROW_COUNT {
            let value = Self::value_text(index);
            if self.rows[index].shown != value {
                let text = if index == IMG_ROW {
                    let shown = if self.image_field.text.is_empty() {
                        "(embedded)".to_string()
                    } else {
                        self.image_field.text.clone()
                    };
                    format!("image  {shown}")
                } else if index == STATUS_ROW {
                    self.status.clone().unwrap_or_default()
                } else {
                    format!("{}  {}", Self::row_name(index), value)
                };
                let color = if index == STATUS_ROW { VALUE } else { LABEL };
                self.rows[index].label = make_label(
                    font_system,
                    &attrs(self.family, color),
                    12.0 * self.scale,
                    &text,
                );
                self.rows[index].shown = value;
            }
        }
        let s = self.scale;
        if self.image_field.shown.as_ref()
            != Some(&(
                self.image_field.text.clone(),
                self.image_field.caret,
                self.image_field.focused,
            ))
        {
            self.image_field
                .rebuild(font_system, self.family, s, IMAGE_PLACEHOLDER);
        }
        if self.theme_field.shown.as_ref()
            != Some(&(
                self.theme_field.text.clone(),
                self.theme_field.caret,
                self.theme_field.focused,
            ))
        {
            self.theme_field
                .rebuild(font_system, self.family, s, THEME_PLACEHOLDER);
        }
    }

    pub fn quads(&mut self, font_system: &mut FontSystem, size: (f32, f32)) -> Vec<QuadVertex> {
        if !self.open {
            return Vec::new();
        }
        self.geometry(font_system);
        let panel = self.panel_rect(size);
        self.row_rects = (0..ROW_COUNT).map(|i| self.row_rect(i, panel)).collect();
        let mut quads = Vec::new();
        let s = self.scale;
        push_rect(&mut quads, panel, PANEL_BG);
        push_line(
            &mut quads,
            panel.x,
            0.0,
            panel.x,
            panel.h,
            1.0 * s,
            PANEL_BORDER,
        );

        let view_top = panel.y + HEADER_H * s;
        let view_h = (panel.h - HEADER_H * s).max(1.0);
        let content = self.content_height();
        for index in 0..ROW_COUNT {
            let row = self.row_rects[index];
            if row.y > view_top + view_h || row.bottom() < view_top || index == STATUS_ROW {
                continue;
            }
            if index < COLOR_COUNT {
                let swatch = Rect {
                    x: row.right() - SWATCH_W * s,
                    y: row.y + 3.0 * s,
                    w: SWATCH_W * s,
                    h: 13.0 * s,
                };
                push_rect(&mut quads, swatch, PANEL_BORDER);
                push_rect(&mut quads, swatch.inset(1.0 * s), (COLOR_ROWS[index].get)());
                for channel in 0..4 {
                    let track = Self::slider_track(row, channel, s);
                    let [r, g, b, a] = (COLOR_ROWS[index].get)().as_rgba();
                    let fraction = [r, g, b, a][channel] as f32 / 255.0;
                    push_rect(&mut quads, track, TRACK);
                    push_rect(
                        &mut quads,
                        Rect {
                            x: track.x,
                            y: track.y,
                            w: track.w * fraction,
                            h: track.h,
                        },
                        FILL,
                    );
                    push_rect(
                        &mut quads,
                        Rect {
                            x: track.x + track.w * fraction - 1.5 * s,
                            y: track.y - 1.0 * s,
                            w: 3.0 * s,
                            h: track.h + 2.0 * s,
                        },
                        THUMB,
                    );
                    if self.drag == Some(DragTarget::Slider { index, channel }) {
                        push_rect(&mut quads, Self::grown(track, 1.0 * s), PANEL_BORDER);
                    }
                }
            } else if index == DIM_ROW || index == BLUR_ROW || index == UNFOCUS_ROW {
                let track = Self::slider_track(row, 0, s);
                let fraction = match index {
                    i if i == DIM_ROW => theme::background_dim(),
                    i if i == BLUR_ROW => theme::background_blur() as f32 / 64.0,
                    _ => theme::unfocus(),
                };
                push_rect(&mut quads, track, TRACK);
                push_rect(
                    &mut quads,
                    Rect {
                        x: track.x,
                        y: track.y,
                        w: track.w * fraction,
                        h: track.h,
                    },
                    FILL,
                );
                push_rect(
                    &mut quads,
                    Rect {
                        x: track.x + track.w * fraction - 1.5 * s,
                        y: track.y - 1.0 * s,
                        w: 3.0 * s,
                        h: track.h + 2.0 * s,
                    },
                    THUMB,
                );
            } else if index == IMG_ROW || index == THEME_ROW {
                let field = Self::field_rect(row, s);
                let focused = if index == IMG_ROW {
                    self.image_field.focused
                } else {
                    self.theme_field.focused
                };
                push_rect(&mut quads, field, FIELD_BG);
                if focused {
                    push_line(
                        &mut quads,
                        field.x,
                        field.bottom(),
                        field.right(),
                        field.bottom(),
                        1.0 * s,
                        VALUE,
                    );
                }
                if index == THEME_ROW {
                    for button in 0..THEME_BUTTONS.len() {
                        push_rect(&mut quads, Self::button_rect(row, button, s), FIELD_BG);
                    }
                }
            }
        }

        if content > view_h {
            let track_h = view_h - 8.0 * s;
            let thumb_h = (view_h / content * track_h).max(24.0 * s);
            let free = (track_h - thumb_h).max(1.0);
            let t = self.scroll / (content - view_h).max(1.0);
            let thumb = Rect {
                x: panel.right() - (PAD + SCROLLBAR_W) * s,
                y: view_top + 4.0 * s + free * t,
                w: SCROLLBAR_W * s,
                h: thumb_h,
            };
            push_rect(
                &mut quads,
                thumb,
                if self.drag == Some(DragTarget::Scroll) {
                    THUMB
                } else {
                    FILL
                },
            );
        }
        quads
    }

    pub fn areas(&self, size: (f32, f32)) -> Vec<TextArea<'_>> {
        if !self.open {
            return Vec::new();
        }
        let panel = self.panel_rect(size);
        let s = self.scale;
        let view_top = panel.y + HEADER_H * s;
        let view_h = (panel.h - HEADER_H * s).max(1.0);
        let panel_bounds = TextBounds {
            left: panel.x as i32,
            top: 0,
            right: panel.right() as i32,
            bottom: panel.bottom() as i32,
        };
        let view_bounds = TextBounds {
            left: panel.x as i32,
            top: view_top as i32,
            right: panel.right() as i32,
            bottom: (view_top + view_h) as i32,
        };
        let mut areas = Vec::new();
        areas.push(TextArea {
            buffer: &self.title,
            left: panel.x + PAD * s,
            top: panel.y + 7.0 * s,
            scale: 1.0,
            bounds: panel_bounds,
            default_color: LABEL,
            custom_glyphs: &[],
        });
        for index in 0..ROW_COUNT {
            if index == IMG_ROW || index == THEME_ROW {
                continue;
            }
            let Some(row) = self.row_rects.get(index) else {
                continue;
            };
            if row.y > view_top + view_h || row.bottom() < view_top {
                continue;
            }
            if index == STATUS_ROW && self.status.is_none() {
                continue;
            }
            areas.push(TextArea {
                buffer: &self.rows[index].label,
                left: row.x,
                top: row.y + 2.0 * s,
                scale: 1.0,
                bounds: view_bounds,
                default_color: LABEL,
                custom_glyphs: &[],
            });
        }
        for (row_index, field) in [(IMG_ROW, &self.image_field), (THEME_ROW, &self.theme_field)] {
            let Some(row) = self.row_rects.get(row_index) else {
                continue;
            };
            let rect = Self::field_rect(*row, s);
            if rect.bottom() > view_top && rect.y < view_top + view_h {
                areas.push(TextArea {
                    buffer: &field.buffer,
                    left: rect.x + 4.0 * s,
                    top: rect.y + 4.0 * s,
                    scale: 1.0,
                    bounds: view_bounds,
                    default_color: LABEL,
                    custom_glyphs: &[],
                });
            }
        }
        if let Some(row) = self.row_rects.get(THEME_ROW) {
            for (index, buffer) in self.buttons.iter().enumerate() {
                let rect = Self::button_rect(*row, index, s);
                if rect.bottom() > view_top && rect.y < view_top + view_h {
                    areas.push(TextArea {
                        buffer,
                        left: rect.x + 4.0 * s,
                        top: rect.y + 4.0 * s,
                        scale: 1.0,
                        bounds: view_bounds,
                        default_color: LABEL,
                        custom_glyphs: &[],
                    });
                }
            }
        }
        areas
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (f32, f32) = (1600.0, 1000.0);

    #[test]
    fn sliders_scroll_and_image_field_work_end_to_end() {
        theme::init();
        let mut font_system = FontSystem::new();
        let mut clipboard = None;
        let mut palette = Palette::new(&mut font_system, "monospace", 1.0);
        palette.toggle();
        assert!(palette.open);

        // a click left of the panel passes through to the app
        assert!(!palette.press(&mut font_system, SIZE, 100.0, 500.0));

        // dragging the bg row's red channel to the track end recolours live
        let before = theme::bg();
        assert!(palette.press(&mut font_system, SIZE, 1390.0, 51.5));
        palette.drag_to(SIZE, 1582.0, 51.5);
        palette.release();
        let [r, g, b, a] = theme::bg().as_rgba();
        assert_eq!(r, 255);
        let [_, g0, b0, a0] = before.as_rgba();
        assert_eq!((g, b, a), (g0, b0, a0));
        let (repaint, reload) = palette.post();
        assert!(repaint);
        assert!(!reload);

        // the scrollbar zone drags the scroll offset
        assert!(palette.press(&mut font_system, SIZE, 1594.0, 500.0));
        let scrolled = palette.scroll;
        palette.drag_to(SIZE, 1594.0, 200.0);
        assert!(palette.scroll != scrolled);
        palette.release();
        palette.scroll = 0.0;

        // wheel scrolls and clamps at both ends
        palette.wheel(SIZE, -3.0);
        assert!(palette.scroll > 0.0);
        palette.wheel(SIZE, -1000.0);
        assert_eq!(palette.scroll, palette.content_height() - (SIZE.1 - HEADER_H));
        palette.wheel(SIZE, 1000.0);
        assert_eq!(palette.scroll, 0.0);
        palette.wheel(SIZE, -1000.0);

        // the image field takes focus, rejects missing files, applies real ones
        let panel = palette.panel_rect(SIZE);
        let field = Palette::field_rect(palette.row_rect(IMG_ROW, panel), 1.0);
        assert!(palette.press(&mut font_system, SIZE, field.x + 5.0, field.y + 10.0));
        assert!(palette.text_active());
        palette.image_field.text.clear();
        palette.image_field.caret = 0;
        for ch in r"C:\nope\kite-pal.png".chars() {
            assert!(palette.char_key(&mut clipboard, &ch.to_string(), false));
        }
        let image_before = theme::background_image();
        assert!(palette.named_key(&mut font_system, NamedKey::Enter));
        assert!(palette.status.is_some());
        assert_eq!(theme::background_image(), image_before);
        assert!(!palette.post().1);

        let file = std::env::temp_dir().join("kite-palette-test-bg.png");
        std::fs::write(&file, [1u8, 2, 3]).unwrap();
        palette.image_field.text.clear();
        palette.image_field.caret = 0;
        for ch in file.to_str().unwrap().chars() {
            palette.char_key(&mut clipboard, &ch.to_string(), false);
        }
        assert!(palette.named_key(&mut font_system, NamedKey::Enter));
        assert_eq!(theme::background_image(), Some(file.clone()));
        assert!(palette.post().1);
        let _ = std::fs::remove_file(&file);

        // the theme row: 导入 loads live and stays session-only, 保存 refuses
        // to overwrite a preset, 另存 wants a name typed in the field
        let row = palette.row_rect(THEME_ROW, panel);
        let field = Palette::field_rect(row, 1.0);
        assert!(palette.press(&mut font_system, SIZE, field.x + 5.0, field.y + 10.0));
        palette.theme_field.text.clear();
        palette.theme_field.caret = 0;
        for ch in "neo".chars() {
            palette.char_key(&mut clipboard, &ch.to_string(), false);
        }
        let import = Palette::button_rect(row, 0, 1.0);
        assert!(palette.press(&mut font_system, SIZE, import.x + 5.0, import.y + 10.0));
        assert_eq!(theme::current_name(), "neo");
        let (repaint, reload) = palette.post();
        assert!(repaint && reload);

        let save = Palette::button_rect(row, 2, 1.0);
        assert!(palette.press(&mut font_system, SIZE, save.x + 5.0, save.y + 10.0));
        assert!(palette.status.as_ref().unwrap().contains("preset"));

        let save_as = Palette::button_rect(row, 3, 1.0);
        let field = Palette::field_rect(row, 1.0);
        assert!(palette.press(&mut font_system, SIZE, field.x + 5.0, field.y + 10.0));
        palette.theme_field.text.clear();
        palette.theme_field.caret = 0;
        assert!(palette.press(&mut font_system, SIZE, save_as.x + 5.0, save_as.y + 10.0));
        assert!(palette.status.as_ref().unwrap().contains("type a name"));

        // 导入 back to the preset the session started on
        let import = Palette::button_rect(row, 0, 1.0);
        assert!(palette.press(&mut font_system, SIZE, import.x + 5.0, import.y + 10.0));
        assert_eq!(theme::current_name(), "neo");
        let field = Palette::field_rect(row, 1.0);
        assert!(palette.press(&mut font_system, SIZE, field.x + 5.0, field.y + 10.0));
        palette.theme_field.text.clear();
        palette.theme_field.caret = 0;
        for ch in "index".chars() {
            palette.char_key(&mut clipboard, &ch.to_string(), false);
        }
        assert!(palette.press(&mut font_system, SIZE, import.x + 5.0, import.y + 10.0));
        assert_eq!(theme::current_name(), "index");
        palette.post();

        // Escape in the field closes the panel
        assert!(palette.named_key(&mut font_system, NamedKey::Escape));
        assert!(!palette.open);
        assert!(!palette.text_active());
    }
}
