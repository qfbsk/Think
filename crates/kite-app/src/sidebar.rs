use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use glyphon::FontSystem;
use glyphon::cosmic_text::{
    AttrsList, Buffer, BufferLine, Color, LineEnding, Metrics, Scroll, Shaping, Wrap,
};
use kite_core::display_path;

use crate::editor::measure_cell;
use crate::text::{bold_attrs, buffer_line, force_relayout};
use crate::theme;
use crate::{archive, git};

const FOLDER_ICON: char = '󰉋';
const EMPTY_FOLDER_ICON: char = '';
const FILE_ICON: char = '󰈔';
const DRIVE_ICON: char = '󰋊';
const GIT_FOLDER_ICON: char = '';

const ICON_BLUE: Color = Color::rgb(0x89, 0xb4, 0xfa);
const ICON_SAPPHIRE: Color = Color::rgb(0x74, 0xc7, 0xec);
const ICON_TEAL: Color = Color::rgb(0x94, 0xe2, 0xd5);
const ICON_GREEN: Color = Color::rgb(0xa6, 0xe3, 0xa1);
const ICON_YELLOW: Color = Color::rgb(0xf9, 0xe2, 0xaf);
const ICON_ORANGE: Color = Color::rgb(0xfa, 0xb3, 0x87);
const ICON_RED: Color = Color::rgb(0xf3, 0x8b, 0xa8);
const ICON_MAUVE: Color = Color::rgb(0xcb, 0xa6, 0xf7);
const ICON_GREY: Color = Color::rgb(0xcd, 0xd6, 0xf4);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Root {
    Drives,
    Dir(PathBuf),
    /// A folder inside an archive file, listed as if it sat on disk. An empty
    /// prefix is the archive's own top level.
    Archive {
        file: PathBuf,
        prefix: Vec<String>,
    },
}

#[derive(Clone, Debug)]
pub struct Row {
    pub path: PathBuf,
    pub is_dir: bool,
    pub label: String,
    pub is_drive: bool,
    /// A git repository's own folder. Its name wears the git colour even while the
    /// folder is collapsed, because a `.git` beside it is a fact of the disk rather
    /// than something the background probe has to answer for.
    repo: bool,
    /// Tracked by git but gone from the work tree: listed, never opened.
    pub missing: bool,
    /// The archive holding this row, for the rows that live inside one. Their
    /// `path` is a name in that listing, not something the disk can answer for.
    pub archive: Option<PathBuf>,
    icon: Range<usize>,
    icon_color: Color,
    /// git's own letter for the state, in the leftmost column.
    mark: Option<(char, Color)>,
    delta: Option<(Range<usize>, Range<usize>)>,
}

pub enum SidebarAction {
    Open(PathBuf),
    Missing,
    /// A name inside an archive: the structure is all we show.
    ReadOnly,
}

impl Row {
    /// Command the shell input receives when Space sends this row across.
    pub fn quick_command(&self) -> String {
        let path = display_path(&self.path);
        let target = if path.chars().any(char::is_whitespace) {
            format!("\"{path}\"")
        } else {
            path
        };
        if self.is_dir {
            format!("cd {target}")
        } else if self
            .path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            target
        } else {
            format!("kkk {target}")
        }
    }
}
pub struct Sidebar {
    pub visible: bool,
    pub root: Root,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub buffer: Buffer,
    /// Whether this pane holds focus; the ui flips it and re-themes, and the
    /// listing bakes its colors through `theme::pane`.
    pub focused: bool,
    expanded: HashSet<PathBuf>,
    git: git::Probe,
    archive: archive::Probe,
    family: &'static str,
    font_size: f32,
    line_height: f32,
    cell_width: f32,
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
        let cell_width = measure_cell(font_system, family, font_size, line_height);
        let mut sidebar = Self {
            // The tree is asked for with `-`; opening a window is not asking.
            visible: false,
            root,
            rows: Vec::new(),
            selected: 0,
            buffer,
            focused: false,
            expanded: HashSet::new(),
            git: git::Probe::new(),
            archive: archive::Probe::new(),
            family,
            font_size,
            line_height,
            cell_width,
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
        self.cell_width = measure_cell(font_system, self.family, font_size, self.line_height);
        self.rebuild(font_system);
    }

    /// Take whatever the git worker has finished; `true` means the listing redrew.
    pub fn poll_git(&mut self, font_system: &mut FontSystem) -> bool {
        if !self.git.poll() {
            return false;
        }
        self.rebuild(font_system);
        true
    }

    pub fn git_pending(&self) -> bool {
        self.git.pending()
    }

    /// A save, a shell-tab commit or simply opening the panel changes what git
    /// reports. Ask again and redraw now — a bump alone would wait for a rebuild
    /// that nothing promises to make.
    pub fn git_changed(&mut self, font_system: &mut FontSystem) {
        self.git.invalidate();
        self.rebuild(font_system);
    }

    pub fn poll_archive(&mut self, font_system: &mut FontSystem) -> bool {
        if !self.archive.poll() {
            return false;
        }
        self.rebuild(font_system);
        true
    }

    pub fn archive_pending(&self) -> bool {
        self.archive.pending()
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
        if matches!(self.root, Root::Drives) {
            self.set_root(font_system, Root::Dir(row.path));
            return None;
        }
        if row.missing {
            return Some(SidebarAction::Missing);
        }
        // An archive opens like any folder, whether it is on disk or the root.
        if row.is_dir || archive::is_archive(&row.path) {
            let path = row.path.clone();
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
            self.rebuild(font_system);
            self.select_path(font_system, &row.path);
            None
        } else if row.archive.is_some() {
            Some(SidebarAction::ReadOnly)
        } else {
            Some(SidebarAction::Open(row.path))
        }
    }

    /// The folder the root is showing, virtual paths included.
    fn floor(&self) -> Option<PathBuf> {
        match &self.root {
            Root::Drives => None,
            Root::Dir(path) => Some(path.clone()),
            Root::Archive { file, prefix } => Some(virtual_path(file, prefix, "")),
        }
    }

    pub fn back(&mut self, font_system: &mut FontSystem) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        let Some(floor) = self.floor() else {
            return;
        };
        if row.is_dir || archive::is_archive(&row.path) {
            if self.expanded.remove(&row.path) {
                self.rebuild(font_system);
                self.select_path(font_system, &row.path);
            }
            return;
        }
        let Some(parent) = row.path.parent() else {
            return;
        };
        if parent == floor {
            return;
        }
        let parent = parent.to_path_buf();
        if self.expanded.remove(&parent) {
            self.rebuild(font_system);
            self.select_path(font_system, &parent);
        }
    }

    pub fn left(&mut self, font_system: &mut FontSystem) {
        match self.root.clone() {
            Root::Drives => {}
            Root::Archive { file, prefix } => {
                let from = virtual_path(&file, &prefix, "");
                self.root = match prefix.split_last() {
                    Some((_, parent)) => Root::Archive {
                        file,
                        prefix: parent.to_vec(),
                    },
                    None => match file.parent() {
                        Some(dir) => Root::Dir(dir.to_path_buf()),
                        None => Root::Drives,
                    },
                };
                self.rebuild(font_system);
                self.select_path(font_system, &from);
            }
            Root::Dir(root) => {
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
        }
    }

    pub fn right(&mut self, font_system: &mut FontSystem) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        match &self.root {
            Root::Drives => self.set_root(font_system, Root::Dir(row.path)),
            Root::Dir(_) | Root::Archive { .. } => {
                if row.is_dir {
                    let root = match &row.archive {
                        None => Root::Dir(row.path.clone()),
                        Some(file) => Root::Archive {
                            file: file.clone(),
                            prefix: virtual_prefix(file, &row.path),
                        },
                    };
                    self.set_root(font_system, root);
                } else if archive::is_archive(&row.path) {
                    self.set_root(
                        font_system,
                        Root::Archive {
                            file: row.path.clone(),
                            prefix: Vec::new(),
                        },
                    );
                }
            }
        }
    }

    pub fn retheme(&mut self, font_system: &mut FontSystem) {
        self.rebuild(font_system);
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
        let columns = (self.width / self.cell_width.max(1.0)).floor() as usize;
        self.rows = match &self.root {
            Root::Drives => drives()
                .into_iter()
                .map(|path| {
                    let name = path.to_string_lossy().trim_end_matches('\\').to_string();
                    let (label, icon, _) = row_text(None, 0, DRIVE_ICON, &name, None, columns);
                    Row {
                        path,
                        is_dir: true,
                        label,
                        is_drive: true,
                        repo: false,
                        missing: false,
                        archive: None,
                        icon,
                        icon_color: ICON_GREEN,
                        mark: None,
                        delta: None,
                    }
                })
                .collect(),
            Root::Dir(root) => {
                let root = root.clone();
                let expanded = self.expanded.clone();
                // Ask for every directory the walk is about to open, so one rebuild
                // covers them all; the listing itself only ever reads the cache.
                let archives: Vec<PathBuf> = expanded
                    .iter()
                    .filter(|path| archive::is_archive(path))
                    .cloned()
                    .collect();
                self.git.want(&root);
                for dir in &expanded {
                    if archive::is_archive(dir) {
                        self.archive.want(dir);
                    // Under an open archive the paths are names in a listing, so git
                    // has nothing to say about them.
                    } else if !archives.iter().any(|file| dir.starts_with(file)) {
                        self.git.want(dir);
                    }
                }
                let mut rows = Vec::new();
                self.append_directory(&mut rows, &root, 0, &expanded, columns);
                rows
            }
            Root::Archive { file, prefix } => {
                let (file, prefix, expanded) =
                    (file.clone(), prefix.clone(), self.expanded.clone());
                self.archive.want(&file);
                let mut rows = Vec::new();
                self.append_archive(&mut rows, &file, &prefix, 0, &expanded, columns);
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
            let colors = theme::pane(self.focused);
            let draw = |color: Color| {
                if self.focused {
                    color
                } else {
                    theme::unfocused(color)
                }
            };
            let color = if row.missing {
                colors.red
            } else if row.is_drive {
                colors.green
            } else if row.is_dir {
                if row.repo {
                    draw(ICON_ORANGE)
                } else {
                    colors.accent
                }
            } else {
                colors.text
            };
            let line_attrs = bold_attrs(self.family, color);
            let mut spans = AttrsList::new(&line_attrs);
            if let Some((mark, color)) = &row.mark {
                let width = mark.len_utf8();
                spans.add_span(0..width, &bold_attrs(self.family, draw(*color)));
            }
            spans.add_span(row.icon.clone(), &bold_attrs(self.family, draw(row.icon_color)));
            if let Some((added, removed)) = &row.delta {
                spans.add_span(added.clone(), &bold_attrs(self.family, colors.green));
                spans.add_span(removed.clone(), &bold_attrs(self.family, colors.red));
            }
            buffer.lines.push(BufferLine::new(
                row.label.clone(),
                LineEnding::Lf,
                spans,
                Shaping::Advanced,
            ));
        }
        if buffer.lines.is_empty() {
            let line_attrs = bold_attrs(self.family, theme::pane(self.focused).text_dim);
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

    fn append_directory(
        &self,
        rows: &mut Vec<Row>,
        dir: &Path,
        depth: usize,
        expanded: &HashSet<PathBuf>,
        columns: usize,
    ) {
        if depth > 64 {
            return;
        }
        let Ok(read_dir) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<(bool, String, String, PathBuf, bool, Option<git::Entry>)> =
            Vec::new();
        for entry in read_dir.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = file_type.is_dir() && !file_type.is_symlink();
            // Windows keeps a few folders for itself — the recycle bin, the
            // restore-point store — and marks them as its own. Nothing inside
            // is meant to be opened, and most of it cannot even be read.
            if is_dir && system_folder(&path) {
                continue;
            }
            let found = if is_dir {
                self.git.folder(&path).map(|state| git::Entry {
                    state,
                    added: 0,
                    removed: 0,
                })
            } else {
                self.git.entry(&path).copied()
            };
            entries.push((!is_dir, name.to_lowercase(), name, path, is_dir, found));
        }
        // Tracked files gone from the work tree are not in the listing; git says so,
        // and they stay visible until they leave git too. A path can also be reported
        // as deleted while a fresh file of the same name sits there, so the listing
        // itself has the final say.
        {
            let present: HashSet<String> = entries
                .iter()
                .map(|(_, _, name, path, _, _)| {
                    kite_core::identity(&path.parent().unwrap_or(dir).join(name))
                })
                .collect();
            for (path, entry) in self.git.missing_in(dir) {
                if present.contains(&kite_core::identity(path)) {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                entries.push((
                    true,
                    name.to_lowercase(),
                    name.to_string(),
                    path.clone(),
                    false,
                    Some(*entry),
                ));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        for (_, _, name, path, is_dir, found) in entries {
            let missing = found.is_some_and(|entry| entry.state == git::State::Deleted) && !is_dir;
            let repo = is_dir && path.join(".git").exists();
            let (icon, icon_color) = if is_dir {
                directory_icon(&path, &name)
            } else {
                file_icon(&name)
            };
            let mark = mark_of(found.map(|entry| entry.state));
            let delta = found
                .and_then(|entry| entry.delta())
                .map(|(added, removed)| format!("+{added} -{removed}"));
            let (label, icon_range, delta) = row_text(
                mark.map(|(mark, _)| mark),
                depth,
                icon,
                &name,
                delta,
                columns,
            );
            let open = (is_dir || archive::is_archive(&path)) && expanded.contains(&path);
            rows.push(Row {
                path: path.clone(),
                is_dir,
                label,
                is_drive: false,
                repo,
                missing,
                archive: None,
                icon: icon_range,
                icon_color,
                mark,
                delta,
            });
            if open {
                if is_dir {
                    self.append_directory(rows, &path, depth + 1, expanded, columns);
                } else {
                    self.append_archive(rows, &path, &[], depth + 1, expanded, columns);
                }
            }
        }
    }

    /// One folder inside an archive. The listing is a flat set of entry names, so
    /// the tree is read out of them; nothing here touches the disk.
    fn append_archive(
        &self,
        rows: &mut Vec<Row>,
        file: &Path,
        prefix: &[String],
        depth: usize,
        expanded: &HashSet<PathBuf>,
        columns: usize,
    ) {
        if depth > 64 {
            return;
        }
        let Some(listing) = self.archive.listing(file) else {
            return;
        };
        // name -> whether something lives below it at this level
        let mut children: HashMap<String, bool> = HashMap::new();
        for entry in listing.entries() {
            let segments: Vec<&str> = entry.split('/').collect();
            if segments.len() <= prefix.len()
                || !prefix
                    .iter()
                    .zip(&segments)
                    .all(|(want, have)| want == have)
            {
                continue;
            }
            let name = segments[prefix.len()].to_string();
            *children.entry(name).or_default() |= segments.len() > prefix.len() + 1;
        }
        let mut children: Vec<(bool, String, String, bool)> = children
            .into_iter()
            .map(|(name, is_dir)| (!is_dir, name.to_lowercase(), name, is_dir))
            .collect();
        children.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        for (_, _, name, is_dir) in children {
            let path = virtual_path(file, prefix, &name);
            let (icon, icon_color) = if is_dir {
                (FOLDER_ICON, ICON_SAPPHIRE)
            } else {
                file_icon(&name)
            };
            let (label, icon_range, delta) = row_text(None, depth, icon, &name, None, columns);
            rows.push(Row {
                path: path.clone(),
                is_dir,
                label,
                is_drive: false,
                repo: false,
                missing: false,
                archive: Some(file.to_path_buf()),
                icon: icon_range,
                icon_color,
                mark: None,
                delta,
            });
            if is_dir && expanded.contains(&path) {
                let mut deeper = prefix.to_vec();
                deeper.push(name);
                self.append_archive(rows, file, &deeper, depth + 1, expanded, columns);
            }
        }
    }
}

/// Where an entry of `file`'s listing lives in the tree. These paths are names,
/// not disk paths: only the sidebar and the listing itself ever read them.
fn virtual_path(file: &Path, prefix: &[String], name: &str) -> PathBuf {
    let mut path = file.to_path_buf();
    for segment in prefix {
        path.push(segment);
    }
    if !name.is_empty() {
        path.push(name);
    }
    path
}

/// The folder a virtual path names, as entry segments below the archive.
fn virtual_prefix(file: &Path, path: &Path) -> Vec<String> {
    path.strip_prefix(file)
        .map(|rest| {
            rest.components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A status glyph from the codicons set the Nerd Font carries — the same family VS
/// Code's source control uses — so the column reads as icons that match the file-type
/// glyphs beside them. The codepoints are the font's own `cod-*` names, confirmed
/// against its cmap.
fn mark_of(state: Option<git::State>) -> Option<(char, Color)> {
    let (mark, color) = match state? {
        git::State::Added => ('\u{eadc}', ICON_GREEN),
        git::State::Conflicted => ('\u{ec6e}', theme::pink()),
        git::State::Deleted => ('\u{eadf}', theme::red()),
        git::State::Ignored => ('\u{eadd}', theme::text_faint()),
        git::State::Modified => ('\u{eade}', ICON_YELLOW),
        git::State::Renamed => ('\u{eae0}', ICON_BLUE),
        git::State::Untracked => ('\u{ea7f}', ICON_TEAL),
    };
    Some((mark, color))
}

/// The row's text: git's mark, a gutter, the indent, the file icon, the name, and
/// the line counts pushed to the right edge of the panel. The name is what remains
/// when there is no room left for that alignment.
fn row_text(
    mark: Option<char>,
    depth: usize,
    icon: char,
    name: &str,
    delta: Option<String>,
    columns: usize,
) -> (String, Range<usize>, Option<(Range<usize>, Range<usize>)>) {
    let indent = "  ".repeat(depth);
    let mark = mark.unwrap_or(' ');
    let start = mark.len_utf8() + 1 + indent.len();
    let mut label = format!("{mark} {indent}{icon}  {name}");
    let end = start + icon.len_utf8();
    let delta = match delta {
        Some(delta) => {
            let width = delta.chars().count();
            let taken = label.chars().count();
            if taken + width <= columns {
                label.push_str(&" ".repeat(columns - taken - width));
            } else {
                label.push_str("   ");
            }
            let added = label.len();
            label.push_str(&delta);
            let plus = delta.find(' ').unwrap_or(1);
            let removed = added + plus + 1;
            Some((added..added + plus, removed..label.len()))
        }
        None => None,
    };
    (label, start..end, delta)
}

fn directory_icon(path: &Path, name: &str) -> (char, Color) {
    if name.eq_ignore_ascii_case(".git") || path.join(".git").exists() {
        return (GIT_FOLDER_ICON, ICON_ORANGE);
    }
    if is_empty_dir(path) {
        return (EMPTY_FOLDER_ICON, theme::accent());
    }
    match name.to_lowercase().as_str() {
        ".github" => ('', ICON_SAPPHIRE),
        ".cache" => ('󰪺', ICON_TEAL),
        ".config" | ".local" => ('󱁿', ICON_TEAL),
        ".vim" => ('', ICON_GREEN),
        "src" => ('󰴉', ICON_MAUVE),
        "doc" | "docs" => ('󱂷', ICON_MAUVE),
        "bin" => ('󱧺', ICON_YELLOW),
        "build" => ('󱧼', ICON_GREY),
        "node_modules" => ('', ICON_GREEN),
        "test" | "tests" => ('󱞊', ICON_BLUE),
        "lib" => ('󰲂', ICON_YELLOW),
        "desktop" => ('󰚝', ICON_ORANGE),
        "documents" => ('󱧶', ICON_ORANGE),
        "downloads" => ('󰉍', ICON_ORANGE),
        "music" => ('󱍙', ICON_ORANGE),
        "pictures" => ('󰉏', ICON_ORANGE),
        "videos" => ('󱞊', ICON_ORANGE),
        _ => (FOLDER_ICON, theme::accent()),
    }
}

fn is_empty_dir(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

fn file_icon(name: &str) -> (char, Color) {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "readme" | "readme.md" | "readme.txt" => return ('', ICON_YELLOW),
        "license" | "license.md" | "license.txt" | "licence" | "licence.md" | "licence.txt" => {
            return ('', ICON_TEAL);
        }
        "makefile" | "gnumakefile" => return ('󱁤', ICON_GREY),
        "cmakelists.txt" => return ('󱁤', ICON_ORANGE),
        "dockerfile" => return ('󰡨', ICON_BLUE),
        "init.lua" => return ('', ICON_GREEN),
        _ => {}
    }
    let ext = lower.rsplit_once('.').map_or("", |(_, ext)| ext);
    match ext {
        "c" => ('󰙱', ICON_BLUE),
        "h" | "hpp" | "hh" | "hxx" => ('󰫵', ICON_MAUVE),
        "cpp" | "cc" | "cxx" => ('󰙲', ICON_SAPPHIRE),
        // The Arduino mark from this font's devicons set: an infinity sign
        // with a minus in one loop and a plus in the other.
        "ino" => ('', ICON_TEAL),
        "cs" => ('󰌛', ICON_GREEN),
        "rs" => ('󱘗', ICON_ORANGE),
        "go" => ('󰟓', ICON_SAPPHIRE),
        "py" | "pyw" => ('󰌠', ICON_YELLOW),
        "js" | "mjs" | "cjs" => ('󰌞', ICON_YELLOW),
        "jsx" => ('', ICON_SAPPHIRE),
        "ts" => ('󰛦', ICON_SAPPHIRE),
        "tsx" => ('', ICON_BLUE),
        "html" | "htm" => ('󰌝', ICON_ORANGE),
        "css" => ('󰌜', ICON_SAPPHIRE),
        "scss" | "sass" => ('󰟬', ICON_RED),
        "json" | "jsonc" => ('󰘦', ICON_YELLOW),
        "toml" => ('', ICON_ORANGE),
        "yaml" | "yml" => ('', ICON_MAUVE),
        "xml" => ('󰗀', ICON_ORANGE),
        "md" | "markdown" => ('󰍔', ICON_GREY),
        "txt" => ('󰦪', ICON_YELLOW),
        "lua" => ('󰢱', ICON_SAPPHIRE),
        "vim" => ('', ICON_GREEN),
        "sh" | "bash" | "zsh" | "fish" => ('', ICON_GREEN),
        "ps1" | "psm1" => ('󰨊', ICON_BLUE),
        "bat" | "cmd" => ('󰭟', ICON_GREY),
        "java" => ('󰬷', ICON_ORANGE),
        "kt" | "kts" => ('󱈙', ICON_BLUE),
        "rb" => ('󰴭', ICON_RED),
        "php" => ('󰌟', ICON_MAUVE),
        "swift" => ('󰛥', ICON_ORANGE),
        "m" | "mm" => ('󰀵', ICON_ORANGE),
        "sql" | "db" | "sqlite" => ('󰆼', ICON_GREY),
        "exe" | "msi" => ('󰖳', ICON_RED),
        "zip" => ('󰗄', ICON_SAPPHIRE),
        "7z" => ('󰗄', ICON_BLUE),
        "rar" => ('󰗄', ICON_GREEN),
        "gz" | "bz2" | "xz" | "zst" => ('󰗄', ICON_GREY),
        "tar" => ('󰬁', ICON_TEAL),
        "jpg" | "jpeg" => ('󰈥', ICON_ORANGE),
        "png" => ('󰸭', ICON_MAUVE),
        "gif" => ('󰵸', ICON_SAPPHIRE),
        "webp" | "bmp" => ('󰈟', ICON_SAPPHIRE),
        "svg" => ('󰜡', ICON_YELLOW),
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => ('󰈣', ICON_TEAL),
        "mp4" | "mkv" | "mov" | "avi" | "webm" => ('󰈫', ICON_MAUVE),
        "pdf" => ('󰈦', ICON_RED),
        "doc" | "docx" => ('󱎒', ICON_SAPPHIRE),
        "xls" | "xlsx" => ('󱎏', ICON_GREEN),
        "ppt" | "pptx" => ('󱎐', ICON_RED),
        "gitignore" | "gitmodules" => ('󰊢', ICON_MAUVE),
        "gitattributes" => ('󰊢', ICON_YELLOW),
        "gitconfig" => ('󰒓', ICON_ORANGE),
        "editorconfig" => ('', ICON_GREY),
        "cfg" | "conf" | "ini" => ('󰒓', ICON_GREY),
        _ => (FILE_ICON, ICON_GREY),
    }
}

fn initial_root() -> Root {
    let root = kite_core::start_dir();
    if root.is_dir() {
        Root::Dir(root)
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

/// Whether the system owns this folder. On Windows that is the one attribute
/// the recycle bin and the restore-point store carry.
#[cfg(windows)]
fn system_folder(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0002;
    path.metadata()
        .is_ok_and(|meta| meta.file_attributes() & FILE_ATTRIBUTE_SYSTEM != 0)
}

#[cfg(not(windows))]
fn system_folder(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Fonts;

    fn listing(dir: &Path) -> Vec<Row> {
        let mut fonts = Fonts::load();
        let sidebar = Sidebar::new(&mut fonts.system, fonts.mono, 13.5, 280.0, 400.0);
        let mut rows = Vec::new();
        sidebar.append_directory(&mut rows, dir, 0, &HashSet::new(), 40);
        rows
    }

    #[test]
    fn resolves_known_file_icons() {
        assert_eq!(file_icon("main.rs"), ('󱘗', ICON_ORANGE));
        assert_eq!(file_icon("README.md"), ('', ICON_YELLOW));
        assert_eq!(file_icon(".gitignore"), ('󰊢', ICON_MAUVE));
        assert_eq!(file_icon("Cargo.toml"), ('', ICON_ORANGE));
        assert_eq!(file_icon("Blink.ino"), ('', ICON_TEAL));
        assert_eq!(file_icon("no-extension"), (FILE_ICON, ICON_GREY));
    }

    #[test]
    fn resolves_directories_and_git_repos() {
        let base = std::env::temp_dir().join(format!("kite-sidebar-icons-{}", std::process::id()));
        let repo = base.join("repo");
        let plain = base.join("src");
        let empty = base.join("empty");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&plain).unwrap();
        fs::create_dir_all(&empty).unwrap();
        fs::write(plain.join("main.rs"), "").unwrap();

        assert_eq!(
            directory_icon(&repo, "repo"),
            (GIT_FOLDER_ICON, ICON_ORANGE)
        );
        assert_eq!(
            directory_icon(&repo.join(".git"), ".git"),
            (GIT_FOLDER_ICON, ICON_ORANGE)
        );
        assert_eq!(directory_icon(&plain, "src"), ('󰴉', ICON_MAUVE));
        assert_eq!(
            directory_icon(&empty, "empty"),
            (EMPTY_FOLDER_ICON, theme::accent())
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn shows_hidden_entries_and_full_names() {
        let base = std::env::temp_dir().join(format!("kite-sidebar-tree-{}", std::process::id()));
        let hidden = base.join(".hidden_dir");
        let empty = base.join("empty");
        let repo = base.join("repo");
        fs::create_dir_all(&hidden).unwrap();
        fs::create_dir_all(&empty).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(hidden.join("keep"), "x").unwrap();
        fs::write(base.join(".hidden.txt"), "x").unwrap();
        fs::write(base.join("archive.tar.gz"), "x").unwrap();

        let rows = listing(&base);

        fn row<'a>(rows: &'a [Row], name: &str) -> &'a Row {
            rows.iter()
                .find(|row| row.path.file_name().and_then(|value| value.to_str()) == Some(name))
                .unwrap_or_else(|| panic!("missing row for {name}"))
        }
        fn glyph(row: &Row) -> char {
            row.label[row.icon.clone()].chars().next().unwrap()
        }

        assert!(row(&rows, ".hidden.txt").label.ends_with(".hidden.txt"));
        assert!(
            row(&rows, "archive.tar.gz")
                .label
                .ends_with("archive.tar.gz")
        );
        assert_eq!(glyph(row(&rows, ".hidden_dir")), FOLDER_ICON);
        assert_eq!(glyph(row(&rows, "empty")), EMPTY_FOLDER_ICON);
        assert_eq!(glyph(row(&rows, "repo")), GIT_FOLDER_ICON);

        let _ = fs::remove_dir_all(&base);
    }

    /// The recycle bin and the restore-point store are folders the system owns;
    /// they are marked as such, and a file tree has no business showing them.
    #[cfg(windows)]
    #[test]
    fn skips_the_folders_the_system_owns() {
        use std::os::windows::ffi::OsStrExt;

        let base =
            std::env::temp_dir().join(format!("kite-sidebar-system-{}", std::process::id()));
        let owned = base.join("$RECYCLE.BIN");
        let kept = base.join("kept");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&owned).unwrap();
        fs::create_dir_all(&kept).unwrap();

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetFileAttributesW(name: *const u16, attributes: u32) -> i32;
        }
        let wide: Vec<u16> = owned
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM, what Windows itself sets.
        assert_eq!(unsafe { SetFileAttributesW(wide.as_ptr(), 0x1 | 0x2) }, 1);

        let names: Vec<String> = listing(&base)
            .iter()
            .map(|row| {
                row.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["kept".to_string()]);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn icon_range_matches_glyph_bytes() {
        let (label, range, _) = row_text(Some('M'), 2, GLYPH, "main.c", None, 80);
        let glyph = label[range.clone()].chars().next().unwrap();
        assert_eq!(glyph, GLYPH);
        assert_eq!(range.len(), glyph.len_utf8());
        // One git column, a gutter, then two indent levels.
        assert_eq!(range.start, 6);
        assert_eq!(label.chars().next().unwrap(), 'M');
    }

    #[test]
    fn line_counts_sit_on_the_right_edge() {
        let (label, _, delta) = row_text(None, 0, GLYPH, "main.c", Some("+1 -19".into()), 40);
        assert_eq!(label.chars().count(), 40);
        assert!(label.ends_with("+1 -19"));
        let (added, removed) = delta.unwrap();
        assert_eq!(&label[added], "+1");
        assert_eq!(&label[removed], "-19");

        // A name that fills the panel keeps the counts, just not the alignment.
        let (long, _, _) = row_text(None, 0, GLYPH, &"n".repeat(50), Some("+2 -3".into()), 40);
        assert!(long.ends_with("+2 -3"));
        assert!(long.chars().count() > 40);
    }

    #[test]
    fn an_archive_lists_its_tree_like_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("box.zip");
        std::fs::write(&archive, b"never mind the bytes").unwrap();

        let mut fonts = Fonts::load();
        let mut sidebar = Sidebar::new(&mut fonts.system, fonts.mono, 13.5, 280.0, 400.0);
        sidebar.archive.seed(
            &archive,
            vec!["one.txt".into(), "deep/two.txt".into(), "deep/".into()],
        );
        sidebar.root = Root::Dir(dir.path().to_path_buf());
        sidebar.expanded.insert(archive.clone());
        sidebar.rebuild(&mut fonts.system);

        let names = |sidebar: &Sidebar| -> Vec<String> {
            sidebar
                .rows
                .iter()
                .map(|row| display_path(&row.path))
                .collect()
        };
        let inside = archive.join("deep");
        assert!(
            names(&sidebar).contains(&display_path(&inside)),
            "the folder below the archive is its own row: {:?}",
            names(&sidebar)
        );
        assert!(
            !names(&sidebar).iter().any(|name| name.ends_with("two.txt")),
            "one level at a time: {:?}",
            names(&sidebar)
        );
        let folder = sidebar.rows.iter().find(|row| row.path == inside).unwrap();
        assert!(folder.is_dir);
        assert_eq!(folder.archive.as_deref(), Some(archive.as_path()));

        sidebar.expanded.insert(inside.clone());
        sidebar.rebuild(&mut fonts.system);
        let file = sidebar
            .rows
            .iter()
            .find(|row| row.path == inside.join("two.txt"))
            .unwrap();
        assert!(!file.is_dir);
        assert_eq!(file.archive.as_deref(), Some(archive.as_path()));
        assert!(
            file.mark.is_none() && file.delta.is_none(),
            "git has nothing to say about a name in a listing"
        );
    }

    #[test]
    fn an_archive_as_root_shows_its_top_level() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("box.zip");
        std::fs::write(&archive, b"never mind the bytes").unwrap();

        let mut fonts = Fonts::load();
        let mut sidebar = Sidebar::new(&mut fonts.system, fonts.mono, 13.5, 280.0, 400.0);
        sidebar
            .archive
            .seed(&archive, vec!["one.txt".into(), "deep/two.txt".into()]);
        sidebar.root = Root::Archive {
            file: archive.clone(),
            prefix: Vec::new(),
        };
        sidebar.rebuild(&mut fonts.system);
        let names: Vec<String> = sidebar
            .rows
            .iter()
            .map(|row| display_path(&row.path))
            .collect();
        assert_eq!(
            names,
            vec![
                display_path(&archive.join("deep")),
                display_path(&archive.join("one.txt"))
            ],
            "folders first, then files"
        );
    }

    const GLYPH: char = '󰙱';

    #[test]
    fn builds_quick_commands_for_space() {
        let make = |path: &str, is_dir: bool| Row {
            path: PathBuf::from(path),
            is_dir,
            label: String::new(),
            is_drive: false,
            repo: false,
            missing: false,
            archive: None,
            icon: 0..0,
            icon_color: ICON_SAPPHIRE,
            mark: None,
            delta: None,
        };
        assert_eq!(
            make(r"E:\My Tools", true).quick_command(),
            r#"cd "E:\My Tools""#
        );
        assert_eq!(make(r"E:\Think", true).quick_command(), r"cd E:\Think");
        assert_eq!(
            make(r"E:\Think\Cargo.toml", false).quick_command(),
            r"kkk E:\Think\Cargo.toml"
        );
        assert_eq!(
            make(r"E:\learn2test\Main.EXE", false).quick_command(),
            r"E:\learn2test\Main.EXE"
        );
    }
}
