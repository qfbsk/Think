use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use glyphon::FontSystem;
use glyphon::cosmic_text::{Buffer, Metrics, Scroll, Wrap};

use crate::text::{attrs, buffer_line, force_relayout};
use crate::theme;

const FOLDER_ICON: char = '\u{f07b}';
const FILE_ICON: char = '\u{f15b}';

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Root {
    Drives,
    Dir(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub path: PathBuf,
    pub is_dir: bool,
    pub label: String,
    pub is_drive: bool,
}

pub enum SidebarAction {
    Open(PathBuf),
}

pub struct Sidebar {
    pub visible: bool,
    pub root: Root,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub buffer: Buffer,
    expanded: HashSet<PathBuf>,
    family: &'static str,
    font_size: f32,
    line_height: f32,
    width: f32,
    height: f32,
}

impl Sidebar {
    pub fn new(
        font_system: &mut FontSystem,
        family: &'static str,
        font_size: f32,
        width: f32,
        height: f32,
    ) -> Self {
        let root = initial_root();
        let line_height = (font_size * 1.4).ceil();
        let buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
        let mut sidebar = Self {
            visible: false,
            root,
            rows: Vec::new(),
            selected: 0,
            buffer,
            expanded: HashSet::new(),
            family,
            font_size,
            line_height,
            width: width.max(1.0),
            height: height.max(1.0),
        };
        sidebar.rebuild(font_system);
        sidebar
    }

    pub fn set_viewport(&mut self, font_system: &mut FontSystem, width: f32, height: f32) {
        let width = width.max(1.0);
        let height = height.max(1.0);
        if (width - self.width).abs() < 0.01 && (height - self.height).abs() < 0.01 {
            return;
        }
        self.width = width;
        self.height = height;
        self.rebuild(font_system);
    }

    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    pub fn set_font_size(&mut self, font_system: &mut FontSystem, font_size: f32) {
        if (font_size - self.font_size).abs() < 0.01 {
            return;
        }
        self.font_size = font_size;
        self.line_height = (font_size * 1.4).ceil();
        self.rebuild(font_system);
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn move_selection(&mut self, font_system: &mut FontSystem, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        let selected = self.selected as isize + delta;
        self.selected = selected.clamp(0, last as isize) as usize;
        self.ensure_visible(font_system);
    }

    pub fn select_row(&mut self, font_system: &mut FontSystem, index: usize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = index.min(self.rows.len() - 1);
        self.ensure_visible(font_system);
    }

    pub fn enter(&mut self, font_system: &mut FontSystem) -> Option<SidebarAction> {
        let row = self.rows.get(self.selected)?.clone();
        match &self.root {
            Root::Drives => {
                self.set_root(font_system, Root::Dir(row.path));
                None
            }
            Root::Dir(_) => {
                if row.is_dir {
                    let path = row.path.clone();
                    if !self.expanded.remove(&path) {
                        self.expanded.insert(path);
                    }
                    self.rebuild(font_system);
                    self.select_path(font_system, &row.path);
                    None
                } else {
                    Some(SidebarAction::Open(row.path))
                }
            }
        }
    }

    pub fn back(&mut self, font_system: &mut FontSystem) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        let Root::Dir(root) = &self.root else {
            return;
        };
        if row.is_dir {
            if self.expanded.remove(&row.path) {
                self.rebuild(font_system);
                self.select_path(font_system, &row.path);
            }
            return;
        }
        let Some(parent) = row.path.parent() else {
            return;
        };
        if parent == root {
            return;
        }
        let parent = parent.to_path_buf();
        if self.expanded.remove(&parent) {
            self.rebuild(font_system);
            self.select_path(font_system, &parent);
        }
    }

    pub fn left(&mut self, font_system: &mut FontSystem) {
        let Root::Dir(root) = &self.root else {
            return;
        };
        let old_root = root.clone();
        let drive = old_root.ancestors().last().map(Path::to_path_buf);
        if drive.as_deref() == Some(old_root.as_path()) {
            let drive_path = old_root.clone();
            self.set_root(font_system, Root::Drives);
            self.select_path(font_system, &drive_path);
            return;
        }
        let Some(parent) = old_root.parent() else {
            self.set_root(font_system, Root::Drives);
            return;
        };
        let parent = parent.to_path_buf();
        self.root = Root::Dir(parent);
        self.rebuild(font_system);
        self.select_path(font_system, &old_root);
    }

    pub fn right(&mut self, font_system: &mut FontSystem) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        match &self.root {
            Root::Drives => self.set_root(font_system, Root::Dir(row.path)),
            Root::Dir(_) => {
                if row.is_dir {
                    self.set_root(font_system, Root::Dir(row.path));
                }
            }
        }
    }

    pub fn scroll_by(&mut self, font_system: &mut FontSystem, delta_lines: f32) {
        if delta_lines.abs() < f32::EPSILON {
            return;
        }
        let scroll = self.buffer.scroll();
        let max_line = self.rows.len().saturating_sub(1) as f32;
        let line = (scroll.line as f32 + delta_lines).clamp(0.0, max_line) as usize;
        self.buffer.set_scroll(Scroll {
            line,
            vertical: 0.0,
            horizontal: 0.0,
        });
        self.buffer.shape_until_scroll(font_system, true);
    }

    pub fn row_at(&self, y: f32) -> Option<usize> {
        if !(0.0..self.height).contains(&y) {
            return None;
        }
        let index = self.buffer.scroll().line + (y / self.line_height) as usize;
        if index < self.rows.len() {
            Some(index)
        } else {
            None
        }
    }

    pub fn shape(&mut self, font_system: &mut FontSystem) {
        self.buffer.shape_until_scroll(font_system, true);
    }

    fn ensure_visible(&mut self, font_system: &mut FontSystem) {
        let visible = (self.height / self.line_height).floor().max(1.0) as usize;
        let start = self.buffer.scroll().line;
        let line = if self.selected < start {
            self.selected
        } else if self.selected >= start + visible {
            self.selected + 1 - visible
        } else {
            start
        };
        if line != start {
            self.buffer.set_scroll(Scroll {
                line,
                vertical: 0.0,
                horizontal: 0.0,
            });
        }
        self.buffer.shape_until_scroll(font_system, true);
    }

    fn select_path(&mut self, font_system: &mut FontSystem, path: &Path) {
        if let Some(index) = self.rows.iter().position(|row| row.path == path) {
            self.selected = index;
        } else if !self.rows.is_empty() {
            self.selected = self.selected.min(self.rows.len() - 1);
        }
        self.ensure_visible(font_system);
    }

    fn set_root(&mut self, font_system: &mut FontSystem, root: Root) {
        self.root = root;
        self.expanded.clear();
        self.selected = 0;
        self.rebuild(font_system);
    }

    fn rebuild(&mut self, font_system: &mut FontSystem) {
        self.rows = match &self.root {
            Root::Drives => drives()
                .into_iter()
                .map(|path| {
                    let label = path.to_string_lossy().trim_end_matches('\\').to_string();
                    Row {
                        path,
                        is_dir: true,
                        label,
                        is_drive: true,
                    }
                })
                .collect(),
            Root::Dir(root) => {
                let mut rows = Vec::new();
                append_directory(&mut rows, root, 0, &self.expanded);
                rows
            }
        };
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }

        let metrics = Metrics::new(self.font_size, self.line_height);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_tab_width(4);
        buffer.set_size(Some(self.width), Some(self.height));
        for row in &self.rows {
            let color = if row.is_drive {
                theme::GREEN
            } else if row.is_dir {
                theme::ACCENT
            } else {
                theme::TEXT
            };
            let line_attrs = attrs(self.family, color);
            buffer.lines.push(buffer_line(&row.label, &line_attrs));
        }
        if buffer.lines.is_empty() {
            let line_attrs = attrs(self.family, theme::TEXT_DIM);
            buffer.lines.push(buffer_line(" ", &line_attrs));
        }
        let line = self
            .buffer
            .scroll()
            .line
            .min(buffer.lines.len().saturating_sub(1));
        buffer.set_scroll(Scroll {
            line,
            vertical: 0.0,
            horizontal: 0.0,
        });
        self.buffer = buffer;
        force_relayout(&mut self.buffer);
        self.buffer.shape_until_scroll(font_system, true);
        self.ensure_visible(font_system);
    }
}

fn append_directory(rows: &mut Vec<Row>, dir: &Path, depth: usize, expanded: &HashSet<PathBuf>) {
    if depth > 64 {
        return;
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = file_type.is_dir() && !file_type.is_symlink();
        entries.push((!is_dir, name.to_lowercase(), name, path, is_dir));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    for (_, _, name, path, is_dir) in entries {
        let icon = if is_dir { FOLDER_ICON } else { FILE_ICON };
        let indent = "  ".repeat(depth);
        let label = format!("{indent}{icon}  {name}");
        let expanded_here = is_dir && expanded.contains(&path);
        rows.push(Row {
            path: path.clone(),
            is_dir,
            label,
            is_drive: false,
        });
        if expanded_here {
            append_directory(rows, &path, depth + 1, expanded);
        }
    }
}

fn initial_root() -> Root {
    let Ok(cwd) = std::env::current_dir() else {
        return Root::Drives;
    };
    let Some(root) = cwd.ancestors().last() else {
        return Root::Drives;
    };
    if root.is_dir() {
        Root::Dir(root.to_path_buf())
    } else {
        Root::Drives
    }
}

#[cfg(windows)]
fn drives() -> Vec<PathBuf> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) != 0 {
            let letter = (b'A' + index as u8) as char;
            drives.push(PathBuf::from(format!("{letter}:\\")));
        }
    }
    drives
}

#[cfg(not(windows))]
fn drives() -> Vec<PathBuf> {
    Vec::new()
}
