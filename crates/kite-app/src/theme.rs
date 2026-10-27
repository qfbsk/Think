use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use glyphon::Color;

#[derive(Debug)]
pub struct Theme {
    pub name: String,
    // pub for the engineering palette's debug_edit; remove with it
    pub colors: Colors,
    pub background: Background,
    pub prompt: Option<Prompt>,
    /// How far an unfocused pane's whole style set is pulled toward `bg`.
    // pub for the engineering palette's debug_edit; remove with it
    pub unfocus: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prompt {
    /// The `thndr@清枫不识客` that opens every line.
    pub user: Color,
    /// The path *and* the `>` that closes it — one group, one colour.
    pub path: Color,
    pub input: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub bg: Color,
    pub panel: Color,
    pub panel_dim: Color,
    pub sidebar: Color,
    pub status: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub status_text: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub accent_dim: Color,
    pub pink: Color,
    pub green: Color,
    pub red: Color,
    pub yellow: Color,
    pub selection: Color,
    pub tab_active: Color,
    pub tab_hover: Color,
    pub cursor: Color,
    pub whitespace_dot: Color,
    pub whitespace_mark: Color,
    /// The miku board's heat levels, emptiest first: a transparent placeholder
    /// and then five steps of green depth.
    pub miku_0: Color,
    pub miku_1: Color,
    pub miku_2: Color,
    pub miku_3: Color,
    pub miku_4: Color,
    pub miku_5: Color,
}

#[derive(Debug)]
pub struct Background {
    pub image: Option<PathBuf>,
    pub dim: f32,
    pub blur: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "index".to_string(),
            colors: Colors::default(),
            background: Background::default(),
            prompt: None,
            unfocus: 89.0 / 255.0,
        }
    }
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            bg: Color::rgb(0x11, 0x11, 0x1b),
            panel: Color::rgba(0x1e, 0x1e, 0x2e, 0xc4),
            panel_dim: Color::rgba(0x18, 0x18, 0x25, 0xd2),
            sidebar: Color::rgba(0x18, 0x18, 0x25, 0xcd),
            status: Color::rgba(0x11, 0x11, 0x1b, 0xd8),
            border: Color::rgba(0x45, 0x47, 0x5a, 0x9e),
            text: Color::rgb(0xcd, 0xd6, 0xf4),
            text_dim: Color::rgb(0x7f, 0x84, 0x9c),
            status_text: Color::rgb(0xa6, 0xad, 0xc8),
            text_faint: Color::rgb(0x6c, 0x70, 0x86),
            accent: Color::rgb(0x89, 0xb4, 0xfa),
            accent_dim: Color::rgba(0x89, 0xb4, 0xfa, 0x38),
            pink: Color::rgb(0xf5, 0xc2, 0xe7),
            green: Color::rgb(0xa6, 0xe3, 0xa1),
            red: Color::rgb(0xf3, 0x8b, 0xa8),
            yellow: Color::rgb(0xf9, 0xe2, 0xaf),
            selection: Color::rgba(0x58, 0x5b, 0x70, 0xc8),
            tab_active: Color::rgba(0x31, 0x32, 0x44, 0xcc),
            tab_hover: Color::rgba(0x45, 0x47, 0x5a, 0x70),
            cursor: Color::rgb(0xf5, 0xe0, 0xdc),
            whitespace_dot: Color::rgba(0x8a, 0x93, 0xb5, 0x72),
            whitespace_mark: Color::rgba(0x7a, 0xa2, 0xf7, 0x8c),
            miku_0: Color::rgba(0x45, 0x47, 0x5a, 0x80),
            miku_1: Color::rgb(0xa6, 0xe3, 0xa1),
            miku_2: Color::rgb(0x8b, 0xd7, 0x94),
            miku_3: Color::rgb(0x71, 0xc4, 0x7f),
            miku_4: Color::rgb(0x58, 0xb0, 0x6a),
            miku_5: Color::rgb(0x3f, 0x9c, 0x56),
        }
    }
}

impl Default for Background {
    fn default() -> Self {
        Self {
            image: None,
            dim: 0.18,
            // The wallpaper ships sharp; this dial is the only blur, so 0 is the picture as authored.
            blur: 0,
        }
    }
}

impl Colors {
    fn neo() -> Self {
        Self {
            bg: Color::rgb(0x00, 0x00, 0x00),
            // Pure black scrims: a veil carries nothing but its alpha, so the
            // wallpaper keeps its own colour and each layer only darkens it.
            panel: Color::rgba(0x00, 0x00, 0x00, 0xb4),
            panel_dim: Color::rgba(0x00, 0x00, 0x00, 0xc0),
            sidebar: Color::rgba(0x00, 0x00, 0x00, 0x98),
            status: Color::rgba(0x00, 0x00, 0x00, 0xe0),
            border: Color::rgba(0x00, 0x00, 0x00, 0x80),
            text: Color::rgb(0xcd, 0xd6, 0xf4),
            text_dim: Color::rgb(0xa6, 0xad, 0xc8),
            status_text: Color::rgb(0xa6, 0xad, 0xc8),
            text_faint: Color::rgb(0x6c, 0x70, 0x86),
            accent: Color::rgb(0x89, 0xb4, 0xfa),
            accent_dim: Color::rgb(0x58, 0x5f, 0x8c),
            pink: Color::rgb(0xf5, 0xc2, 0xe7),
            green: Color::rgb(0xa6, 0xe3, 0xa1),
            red: Color::rgb(0xf3, 0x8b, 0xa8),
            yellow: Color::rgb(0xf9, 0xe2, 0xaf),
            selection: Color::rgb(0x58, 0x5f, 0x8c),
            tab_active: Color::rgba(0x00, 0x00, 0x00, 0xcc),
            tab_hover: Color::rgba(0x00, 0x00, 0x00, 0x80),
            cursor: Color::rgb(0xf5, 0xe0, 0xdc),
            whitespace_dot: Color::rgba(0x8a, 0x93, 0xb5, 0x72),
            whitespace_mark: Color::rgba(0x7a, 0xa2, 0xf7, 0x8c),
            miku_0: Color::rgba(0x45, 0x47, 0x5a, 0x80),
            miku_1: Color::rgb(0xa6, 0xe3, 0xa1),
            miku_2: Color::rgb(0x8b, 0xd7, 0x94),
            miku_3: Color::rgb(0x71, 0xc4, 0x7f),
            miku_4: Color::rgb(0x58, 0xb0, 0x6a),
            miku_5: Color::rgb(0x3f, 0x9c, 0x56),
        }
    }
}

impl Background {
    fn neo() -> Self {
        Self {
            image: None,
            dim: 0.18,
            blur: 0,
        }
    }
}

impl Prompt {
    fn neo() -> Self {
        Self {
            user: Color::rgb(0xba, 0xc2, 0xde),
            path: Color::rgb(0xf5, 0xc2, 0xe7),
            input: Color::rgb(0xcd, 0xd6, 0xf4),
        }
    }

    /// The prompt of a theme that says nothing about its prompt: the identity
    /// opens in the accent, then the path and its `>` read as one run of pink,
    /// and the body is what was typed.
    fn from_colors(colors: &Colors) -> Self {
        Self {
            user: colors.accent,
            path: colors.pink,
            input: colors.text,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Preset {
    Index,
    Neo,
}

impl Preset {
    fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "index" => Ok(Self::Index),
            "neo" => Ok(Self::Neo),
            other => Err(format!("unknown preset `{other}` (expected index or neo)")),
        }
    }

    fn theme(self) -> Theme {
        match self {
            Self::Index => Theme::default(),
            Self::Neo => Theme {
                name: "neo".to_string(),
                colors: Colors::neo(),
                background: Background::neo(),
                prompt: Some(Prompt::neo()),
                unfocus: 128.0 / 255.0,
            },
        }
    }
}

static ACTIVE: RwLock<Option<Arc<Theme>>> = RwLock::new(None);

/// Startup theme: the applied record wins, then the legacy config file, then
/// the index preset. Only `theme <name>` writes the record — palette loads
/// and saves never touch it, so an unsaved session reopens as it was.
pub fn init() -> Option<String> {
    if let Some(name) = read_active() {
        return match load_named(&name) {
            Ok(theme) => {
                let notice = format!("theme: {}", theme.name);
                set_active(theme);
                Some(notice)
            }
            Err(error) => {
                set_active(Theme::default());
                Some(format!("! theme `{name}` did not load: {error}"))
            }
        };
    }
    let path = config_path()?;
    let text = path
        .is_file()
        .then(|| std::fs::read_to_string(&path).ok())
        .flatten()?;
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    match Theme::from_toml(&text, &base) {
        Ok(theme) => {
            let notice = format!("theme: {}", theme.name);
            set_active(theme);
            Some(notice)
        }
        // A file that is not read leaves the screen on the built-in colours,
        // which looks like the theme was ignored on purpose — say so, loudly.
        Err(error) => {
            set_active(Theme::default());
            Some(format!("! {} ignored: {error}", path.display()))
        }
    }
}

/// The name the running theme carries.
pub fn current_name() -> String {
    current().name.clone()
}

/// Presets first, then the local theme files, alphabetically.
pub fn list() -> Vec<String> {
    let mut names = vec!["index".to_string(), "neo".to_string()];
    let locals = kite_dir().map(|dir| local_names(&dir)).unwrap_or_default();
    names.extend(locals.into_iter().filter(|name| !is_preset(name)));
    names
}

/// The names the files under a kite directory answer to: every theme file in
/// `themes/`, plus the root `theme.toml` under the name it carries itself.
fn local_names(dir: &Path) -> Vec<String> {
    let themes = dir.join("themes");
    let mut names = std::fs::read_dir(&themes)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    if path.extension()?.to_str()? != "toml" {
                        return None;
                    }
                    Some(path.file_stem()?.to_string_lossy().into_owned())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(name) = root_name(dir) {
        if !names.iter().any(|existing| *existing == name) {
            names.push(name);
        }
    }
    names.sort();
    names
}

/// The name the root `theme.toml` carries. Editing that file by hand makes a
/// theme, and a theme you cannot name is a theme you cannot go back to.
fn root_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("theme.toml")).ok()?;
    Theme::from_toml(&text, dir).ok().map(|theme| theme.name)
}

/// Presets answer first; then `themes/<name>.toml`; then the root file when
/// that is the name it carries. `None` means no file answers to the name at
/// all, while a file that exists but does not parse reports why.
fn theme_file(dir: &Path, name: &str) -> Option<Result<Theme, String>> {
    let themes = dir.join("themes");
    let path = themes.join(format!("{name}.toml"));
    if let Ok(text) = std::fs::read_to_string(&path) {
        return Some(Theme::from_toml(&text, &themes));
    }
    let text = std::fs::read_to_string(dir.join("theme.toml")).ok()?;
    let root = Theme::from_toml(&text, dir).ok()?;
    (root.name == name).then_some(Ok(root))
}

pub fn load_named(name: &str) -> Result<Theme, String> {
    if let Ok(preset) = Preset::from_name(name) {
        return Ok(preset.theme());
    }
    check_name(name)?;
    let dir = kite_dir().ok_or_else(|| "no config directory available".to_string())?;
    theme_file(&dir, name).unwrap_or_else(|| {
        Err(format!(
            "no such theme: {name} (looked in {})",
            dir.join("themes").display()
        ))
    })
}

/// Swap the running theme without touching what the next start loads.
pub fn load_live(name: &str) -> Result<String, String> {
    let theme = load_named(name)?;
    let applied = theme.name.clone();
    set_active(theme);
    Ok(applied)
}

/// The `theme` command's half: swap now *and* make the next start agree.
pub fn apply(name: &str) -> Result<String, String> {
    let applied = load_live(name)?;
    write_record(name)?;
    Ok(applied)
}

/// The 应用 button's half: keep the running theme, but make the next start
/// load this theme's name.
pub fn apply_record(name: &str) -> Result<String, String> {
    check_name(name)?;
    write_record(name)?;
    Ok(name.to_string())
}

fn write_record(name: &str) -> Result<(), String> {
    let path = active_path().ok_or_else(|| "no config directory available".to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    std::fs::write(&path, format!("{name}\n"))
        .map_err(|error| format!("applied, but the next start will not remember it: {error}"))
}

/// Write the running theme — palette edits included — to the themes
/// directory under `name`. Preset names are off limits: they would only
/// shadow the built-ins, which win on load anyway.
pub fn save_current(name: &str) -> Result<PathBuf, String> {
    check_name(name)?;
    if Preset::from_name(name).is_ok() {
        return Err(format!("`{name}` is a preset; pick another name"));
    }
    let dir = themes_dir().ok_or_else(|| "no themes directory available".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(&path, current().to_toml_as(name)).map_err(|error| error.to_string())?;
    Ok(path)
}

/// Whether a name belongs to a built-in preset rather than a file.
pub fn is_preset(name: &str) -> bool {
    Preset::from_name(name).is_ok()
}

fn check_name(name: &str) -> Result<(), String> {
    let shaped = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && !name.contains("..");
    if shaped {
        Ok(())
    } else {
        Err(format!(
            "`{name}` is not a theme name (letters, digits, _, -, .; no path separators)"
        ))
    }
}

fn read_active() -> Option<String> {
    let path = active_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let name = text.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Swap the theme while the app runs; the caller repaints everything that baked
/// colors in (buffers, labels, background texture).
fn set_active(theme: Theme) {
    *ACTIVE.write().expect("theme lock") = Some(Arc::new(theme));
}

fn current() -> Arc<Theme> {
    ACTIVE
        .read()
        .expect("theme lock")
        .clone()
        .unwrap_or_default()
}

pub fn background_image() -> Option<PathBuf> {
    current().background.image.clone()
}

pub fn background_dim() -> f32 {
    current().background.dim
}

/// Blur radius in display pixels: the source is pre-scaled to the window's
/// cover crop before the blur runs.
pub fn background_blur() -> u32 {
    current().background.blur
}

/// How far an unfocused pane's colors sit from its raw slots, 0..=1.
pub fn unfocus() -> f32 {
    current().unfocus
}

/// Engineering-palette hook: mutate the live theme in place, no file involved.
pub fn debug_edit(f: impl FnOnce(&mut Theme)) {
    let mut active = ACTIVE.write().expect("theme lock");
    if let Some(theme) = active.as_mut().and_then(Arc::get_mut) {
        f(theme);
    }
}

macro_rules! color_accessors {
    ($($name:ident),* $(,)?) => {
        $(
            pub fn $name() -> Color {
                current().colors.$name
            }
        )*
    };
}

color_accessors!(
    bg,
    panel,
    panel_dim,
    sidebar,
    status,
    border,
    text,
    text_dim,
    status_text,
    text_faint,
    accent,
    accent_dim,
    pink,
    green,
    red,
    yellow,
    selection,
    tab_active,
    tab_hover,
    cursor,
    whitespace_dot,
    whitespace_mark,
    miku_0,
    miku_1,
    miku_2,
    miku_3,
    miku_4,
    miku_5,
);

impl Colors {
    /// The heat colour for a day's `level` (0 = nothing saved, 5 = the busiest).
    pub fn miku(self, level: usize) -> Color {
        match level {
            0 => self.miku_0,
            1 => self.miku_1,
            2 => self.miku_2,
            3 => self.miku_3,
            4 => self.miku_4,
            _ => self.miku_5,
        }
    }

    /// Every channel of every slot pulled `k` of the way toward `bg`.
    fn mixed(self, k: f32) -> Self {
        let pull = |color: Color| mix_toward(color, self.bg, k);
        Self {
            bg: pull(self.bg),
            panel: pull(self.panel),
            panel_dim: pull(self.panel_dim),
            sidebar: pull(self.sidebar),
            status: pull(self.status),
            border: pull(self.border),
            text: pull(self.text),
            text_dim: pull(self.text_dim),
            status_text: pull(self.status_text),
            text_faint: pull(self.text_faint),
            accent: pull(self.accent),
            accent_dim: pull(self.accent_dim),
            pink: pull(self.pink),
            green: pull(self.green),
            red: pull(self.red),
            yellow: pull(self.yellow),
            selection: pull(self.selection),
            tab_active: pull(self.tab_active),
            tab_hover: pull(self.tab_hover),
            cursor: pull(self.cursor),
            whitespace_dot: pull(self.whitespace_dot),
            whitespace_mark: pull(self.whitespace_mark),
            miku_0: pull(self.miku_0),
            miku_1: pull(self.miku_1),
            miku_2: pull(self.miku_2),
            miku_3: pull(self.miku_3),
            miku_4: pull(self.miku_4),
            miku_5: pull(self.miku_5),
        }
    }
}

fn mix_toward(color: Color, toward: Color, k: f32) -> Color {
    let [r, g, b, a] = color.as_rgba();
    let [tr, tg, tb, ta] = toward.as_rgba();
    let pull = |from: u8, to: u8| (from as f32 * (1.0 - k) + to as f32 * k).round() as u8;
    Color::rgba(pull(r, tr), pull(g, tg), pull(b, tb), pull(a, ta))
}

/// The style set a pane draws with: its raw slots while it holds focus, every
/// slot pulled toward `bg` by the theme's unfocus strength while it does not.
/// One function for both panes, so equal slots make a focus flip an exact
/// appearance swap.
pub fn pane(focused: bool) -> Colors {
    let theme = current();
    if focused {
        theme.colors
    } else {
        theme.colors.mixed(theme.unfocus)
    }
}

/// A color as an unfocused pane would draw it: pulled toward `bg` by the
/// theme's unfocus strength. For colors that ride along per row, where the
/// slot was picked at build time and the pane only decides at paint time.
pub fn unfocused(color: Color) -> Color {
    let theme = current();
    mix_toward(color, theme.colors.bg, theme.unfocus)
}

/// The prompt colors as a pane sees them, same rule as [`pane`]. A theme that
/// says nothing about the prompt still colors it: the header has no flat state
/// to fall back into, and "I did not mention the prompt" is not "draw it in
/// one dim colour".
pub fn pane_prompt(focused: bool) -> Prompt {
    let theme = current();
    let prompt = theme.prompt_colors();
    if focused {
        return prompt;
    }
    let bg = theme.colors.bg;
    let k = theme.unfocus;
    Prompt {
        user: mix_toward(prompt.user, bg, k),
        path: mix_toward(prompt.path, bg, k),
        input: mix_toward(prompt.input, bg, k),
    }
}

impl Theme {
    /// What the prompt line is painted with: this theme's own `[prompt]` when
    /// it names one, otherwise the palette it already carries.
    pub fn prompt_colors(&self) -> Prompt {
        self.prompt
            .unwrap_or_else(|| Prompt::from_colors(&self.colors))
    }

    pub fn from_toml(text: &str, base: &Path) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text).map_err(|error| error.to_string())?;
        let table = value
            .as_table()
            .ok_or_else(|| "theme must be a table".to_string())?;
        for key in table.keys() {
            if !matches!(
                key.as_str(),
                "preset" | "name" | "colors" | "background" | "prompt" | "style"
            ) {
                return Err(format!("unknown key `{key}`"));
            }
        }
        let preset = match table.get("preset").and_then(|value| value.as_str()) {
            Some(name) => Preset::from_name(name)?,
            None => Preset::Index,
        };
        let mut theme = preset.theme();
        if let Some(value) = table.get("name") {
            theme.name = value
                .as_str()
                .ok_or_else(|| "name must be a string".to_string())?
                .trim()
                .to_string();
        }
        if let Some(value) = table.get("colors") {
            apply_colors(&mut theme.colors, value)?;
        }
        if let Some(value) = table.get("background") {
            apply_background(&mut theme.background, value, base)?;
        }
        if let Some(value) = table.get("style") {
            apply_style(&mut theme.unfocus, value)?;
        }
        if let Some(value) = table.get("prompt") {
            // Seeded from the palette the theme already has, so naming one
            // part leaves the other two as they would have been anyway.
            let seed = theme.prompt_colors();
            let prompt = theme.prompt.get_or_insert(seed);
            apply_prompt(prompt, value)?;
        }
        if theme.name.is_empty() {
            theme.name = "custom".to_string();
        }
        Ok(theme)
    }

    /// Write the theme out in exactly the shape this parser reads, so a palette
    /// session can become a file and round-trip unchanged. The `name` argument
    /// is what the file will report when loaded — what 另存 asks for. It must
    /// match the file's location, otherwise loading it back would report the
    /// theme the sliders happened to start from.
    pub fn to_toml_as(&self, name: &str) -> String {
        let hex = |color: Color| {
            let [r, g, b, a] = color.as_rgba();
            if a == 255 {
                format!("\"#{r:02x}{g:02x}{b:02x}\"")
            } else {
                format!("\"#{r:02x}{g:02x}{b:02x}{a:02x}\"")
            }
        };
        let c = &self.colors;
        let mut out = format!("name = {}\n", toml_str(name));
        out += "\n[colors]\n";
        for (key, value) in [
            ("bg", c.bg),
            ("panel", c.panel),
            ("panel_dim", c.panel_dim),
            ("sidebar", c.sidebar),
            ("status", c.status),
            ("border", c.border),
            ("text", c.text),
            ("text_dim", c.text_dim),
            ("status_text", c.status_text),
            ("text_faint", c.text_faint),
            ("accent", c.accent),
            ("accent_dim", c.accent_dim),
            ("pink", c.pink),
            ("green", c.green),
            ("red", c.red),
            ("yellow", c.yellow),
            ("selection", c.selection),
            ("tab_active", c.tab_active),
            ("tab_hover", c.tab_hover),
            ("cursor", c.cursor),
            ("whitespace_dot", c.whitespace_dot),
            ("whitespace_mark", c.whitespace_mark),
            ("miku_0", c.miku_0),
            ("miku_1", c.miku_1),
            ("miku_2", c.miku_2),
            ("miku_3", c.miku_3),
            ("miku_4", c.miku_4),
            ("miku_5", c.miku_5),
        ] {
            out += &format!("{key} = {}\n", hex(value));
        }
        out += &format!("\n[style]\nunfocus = {}\n", self.unfocus);
        out += "\n[background]\n";
        match &self.background.image {
            Some(path) => out += &format!("image = {}\n", toml_str(&path.display().to_string())),
            None => out += "image = \"\"\n",
        }
        out += &format!(
            "dim = {}\nblur = {}\n",
            self.background.dim, self.background.blur
        );
        if let Some(prompt) = &self.prompt {
            out += &format!(
                "\n[prompt]\nuser = {}\npath = {}\ninput = {}\n",
                hex(prompt.user),
                hex(prompt.path),
                hex(prompt.input)
            );
        }
        out
    }
}

fn toml_str(text: &str) -> String {
    // A literal string keeps Windows backslashes as they are; only the
    // quote itself and control characters force the escaped form.
    let plain = text.chars().all(|ch| ch != '\'' && !ch.is_control());
    if plain {
        format!("'{text}'")
    } else {
        format!("{text:?}")
    }
}

fn apply_colors(colors: &mut Colors, value: &toml::Value) -> Result<(), String> {
    let table = value
        .as_table()
        .ok_or_else(|| "colors must be a table".to_string())?;
    for (key, value) in table {
        let text = value
            .as_str()
            .ok_or_else(|| format!("colors.{key} must be a color string"))?;
        let color = parse_color(text).map_err(|error| format!("colors.{key}: {error}"))?;
        let target = match key.as_str() {
            "bg" => &mut colors.bg,
            "panel" => &mut colors.panel,
            "panel_dim" => &mut colors.panel_dim,
            "sidebar" => &mut colors.sidebar,
            "status" => &mut colors.status,
            "border" => &mut colors.border,
            "text" => &mut colors.text,
            "text_dim" => &mut colors.text_dim,
            "status_text" => &mut colors.status_text,
            "text_faint" => &mut colors.text_faint,
            "accent" => &mut colors.accent,
            "accent_dim" => &mut colors.accent_dim,
            "pink" => &mut colors.pink,
            "green" => &mut colors.green,
            "red" => &mut colors.red,
            "yellow" => &mut colors.yellow,
            "selection" => &mut colors.selection,
            "tab_active" => &mut colors.tab_active,
            "tab_hover" => &mut colors.tab_hover,
            "cursor" => &mut colors.cursor,
            "whitespace_dot" => &mut colors.whitespace_dot,
            "whitespace_mark" => &mut colors.whitespace_mark,
            "miku_0" => &mut colors.miku_0,
            "miku_1" => &mut colors.miku_1,
            "miku_2" => &mut colors.miku_2,
            "miku_3" => &mut colors.miku_3,
            "miku_4" => &mut colors.miku_4,
            "miku_5" => &mut colors.miku_5,
            other => return Err(format!("colors: unknown key `{other}`")),
        };
        *target = color;
    }
    Ok(())
}

fn apply_background(
    background: &mut Background,
    value: &toml::Value,
    base: &Path,
) -> Result<(), String> {
    let table = value
        .as_table()
        .ok_or_else(|| "background must be a table".to_string())?;
    for (key, value) in table {
        match key.as_str() {
            "image" => {
                let text = value
                    .as_str()
                    .ok_or_else(|| "background.image must be a string".to_string())?;
                background.image = if text.is_empty() {
                    None
                } else {
                    let path = PathBuf::from(text);
                    Some(if path.is_absolute() {
                        path
                    } else {
                        base.join(path)
                    })
                };
            }
            "dim" => {
                let dim = match value {
                    toml::Value::Float(value) => *value as f32,
                    toml::Value::Integer(value) => *value as f32,
                    _ => return Err("background.dim must be a number".to_string()),
                };
                if !(0.0..=1.0).contains(&dim) {
                    return Err("background.dim must be between 0 and 1".to_string());
                }
                background.dim = dim;
            }
            "blur" => {
                let blur = match value {
                    toml::Value::Integer(value) if *value >= 0 => *value as u32,
                    _ => return Err("background.blur must be a whole number of pixels".to_string()),
                };
                if blur > 64 {
                    return Err("background.blur must be between 0 and 64".to_string());
                }
                background.blur = blur;
            }
            other => return Err(format!("background: unknown key `{other}`")),
        }
    }
    Ok(())
}

fn apply_style(unfocus: &mut f32, value: &toml::Value) -> Result<(), String> {
    let table = value
        .as_table()
        .ok_or_else(|| "style must be a table".to_string())?;
    for (key, value) in table {
        match key.as_str() {
            "unfocus" => {
                let k = match value {
                    toml::Value::Float(value) => *value as f32,
                    toml::Value::Integer(value) => *value as f32,
                    _ => return Err("style.unfocus must be a number".to_string()),
                };
                if !(0.0..=1.0).contains(&k) {
                    return Err("style.unfocus must be between 0 and 1".to_string());
                }
                *unfocus = k;
            }
            other => return Err(format!("style: unknown key `{other}`")),
        }
    }
    Ok(())
}

fn apply_prompt(prompt: &mut Prompt, value: &toml::Value) -> Result<(), String> {
    let table = value
        .as_table()
        .ok_or_else(|| "prompt must be a table".to_string())?;
    for (key, value) in table {
        let text = value
            .as_str()
            .ok_or_else(|| format!("prompt.{key} must be a color string"))?;
        let color = parse_color(text).map_err(|error| format!("prompt.{key}: {error}"))?;
        match key.as_str() {
            "user" => prompt.user = color,
            "path" => prompt.path = color,
            "input" => prompt.input = color,
            // The `>` now rides with the path, so the old `arrow` key has
            // nothing left to colour. Still taken, so an older file loads.
            "arrow" => {}
            other => return Err(format!("prompt: unknown key `{other}`")),
        }
    }
    Ok(())
}

fn parse_color(text: &str) -> Result<Color, String> {
    let hex = text
        .strip_prefix('#')
        .ok_or_else(|| format!("`{text}` is not #rrggbb or #rrggbbaa"))?;
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("`{text}` is not #rrggbb or #rrggbbaa"));
    }
    let channel = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).unwrap();
    match hex.len() {
        6 => Ok(Color::rgb(channel(0), channel(2), channel(4))),
        8 => Ok(Color::rgba(channel(0), channel(2), channel(4), channel(6))),
        _ => Err(format!("`{text}` is not #rrggbb or #rrggbbaa")),
    }
}

fn kite_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".config"))
    })?;
    Some(base.join("kite"))
}

fn config_path() -> Option<PathBuf> {
    Some(kite_dir()?.join("theme.toml"))
}

/// Where `save` writes and `load` reads the local themes.
fn themes_dir() -> Option<PathBuf> {
    Some(kite_dir()?.join("themes"))
}

/// One line: the name `theme <name>` last applied. Nothing else writes it.
fn active_path() -> Option<PathBuf> {
    Some(kite_dir()?.join("active-theme"))
}

pub const FONT_SIZE_BASE: f32 = 15.0;
/// A file opens bigger than the shell: the grid has to keep the terminal's own
/// metrics, but reading code wants more than that.
pub const EDITOR_FONT_RATIO: f32 = 1.15;
pub const LINE_HEIGHT_RATIO: f32 = 1.45;
pub const MIN_FONT_SIZE: f32 = 9.0;
pub const MAX_FONT_SIZE: f32 = 40.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_stand_when_file_is_empty() {
        let theme = Theme::from_toml("", Path::new(".")).unwrap();
        assert_eq!(theme.name, "index");
        assert_eq!(theme.colors.text, Color::rgb(0xcd, 0xd6, 0xf4));
        assert_eq!(theme.background.dim, 0.18);
        assert_eq!(theme.background.blur, 0);
    }

    #[test]
    fn neo_preset_is_transparent_and_keeps_the_wallpaper_sharp() {
        let theme = Theme::from_toml("preset = \"neo\"\n", Path::new(".")).unwrap();
        assert_eq!(theme.name, "neo");
        assert_eq!(theme.background.blur, 0);
        assert_eq!(theme.background.dim, 0.18);
        assert!(theme.colors.panel.a() < Colors::default().panel.a());
        assert!(theme.colors.sidebar.a() < Colors::default().sidebar.a());
    }

    #[test]
    fn neo_highlights_the_prompt_and_input() {
        let theme = Theme::from_toml("preset = \"neo\"\n", Path::new(".")).unwrap();
        let prompt = theme.prompt.unwrap();
        assert_eq!(prompt.user, Color::rgb(0xba, 0xc2, 0xde));
        assert_eq!(prompt.path, Color::rgb(0xf5, 0xc2, 0xe7));
        assert_eq!(prompt.input, Color::rgb(0xcd, 0xd6, 0xf4));
        assert!(Theme::default().prompt.is_none());
    }

    /// Twice now the header has gone flat because "no `[prompt]` section" was
    /// read as "no colours for the header". A theme that says nothing about its
    /// prompt still has one, in the palette it already carries.
    #[test]
    fn a_theme_that_says_nothing_about_its_prompt_still_colors_one() {
        let index = Theme::default();
        let prompt = index.prompt_colors();
        assert_eq!(prompt.user, index.colors.accent);
        assert_eq!(prompt.path, index.colors.pink);
        assert_eq!(prompt.input, index.colors.text);
        // Not index's colours, though: the theme's own.
        let theme = Theme::from_toml(
            "[colors]\naccent = \"#010000\"\npink = \"#020000\"\ntext = \"#030000\"\n",
            Path::new("."),
        )
        .unwrap();
        let prompt = theme.prompt_colors();
        assert_eq!(prompt.user, Color::rgb(1, 0, 0));
        assert_eq!(prompt.path, Color::rgb(2, 0, 0));
        assert_eq!(prompt.input, Color::rgb(3, 0, 0));
        // A prompt of its own still wins over the derived one.
        assert_eq!(
            Preset::Neo.theme().prompt_colors().path,
            Color::rgb(0xf5, 0xc2, 0xe7)
        );
    }

    #[test]
    fn an_unfocused_pane_pulls_every_slot_toward_bg() {
        let colors = Colors::default();
        assert_eq!(colors.mixed(0.0).text, colors.text);
        assert_eq!(colors.mixed(1.0).text, colors.bg);
        assert_eq!(colors.mixed(0.5).bg, colors.bg);
        let pull = |from: u8, to: u8| (from as f32 * 0.5 + to as f32 * 0.5).round() as u8;
        let [pr, pg, pb, pa] = colors.panel.as_rgba();
        let [br, bg, bb, ba] = colors.bg.as_rgba();
        assert_eq!(
            colors.mixed(0.5).panel.as_rgba(),
            [pull(pr, br), pull(pg, bg), pull(pb, bb), pull(pa, ba)]
        );
    }

    #[test]
    fn equal_slots_make_a_focus_flip_an_appearance_swap() {
        // Both panes read the one resolver, so with the sidebar slot set
        // equal to the panel slot the unfocused look is the same on either
        // side and a focus flip is a whole-appearance swap.
        let mut colors = Colors::default();
        colors.sidebar = colors.panel;
        let k = 89.0 / 255.0;
        assert_eq!(colors.mixed(k).sidebar, colors.mixed(k).panel);
    }

    #[test]
    fn style_unfocus_parses_and_range_checks() {
        let theme = Theme::from_toml("[style]\nunfocus = 0.25\n", Path::new(".")).unwrap();
        assert!((theme.unfocus - 0.25).abs() < 1e-6);
        assert!(Theme::from_toml("[style]\nunfocus = 2.0\n", Path::new(".")).is_err());
        assert!(Theme::from_toml("[style]\nveil = 1\n", Path::new(".")).is_err());
    }

    #[test]
    fn a_theme_round_trips_through_its_own_toml() {
        let base = Path::new(".");
        for theme in [Theme::default(), Preset::Neo.theme()] {
            let back = Theme::from_toml(&theme.to_toml_as(theme.name.as_str()), base).unwrap();
            assert_eq!(back.name, theme.name);
            assert_eq!(back.colors, theme.colors);
            assert_eq!(back.unfocus, theme.unfocus);
            assert_eq!(back.prompt, theme.prompt);
            assert_eq!(back.background.dim, theme.background.dim);
            assert_eq!(back.background.blur, theme.background.blur);
        }
        // A Windows path survives the trip through a literal string.
        let mut theme = Theme::default();
        theme.background.image = Some(std::path::PathBuf::from(r"C:\pics\6.jpg"));
        let back = Theme::from_toml(&theme.to_toml_as(theme.name.as_str()), base).unwrap();
        assert_eq!(
            back.background.image,
            Some(std::path::PathBuf::from(r"C:\pics\6.jpg"))
        );
        // Saving under a new name stamps that name, so loading the file back
        // reports the theme the user named rather than the one slid from.
        let back = Theme::from_toml(&Theme::default().to_toml_as("test"), base).unwrap();
        assert_eq!(back.name, "test");
    }

    #[test]
    fn theme_names_stay_one_word() {
        assert!(check_name("my-neo_2").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name("a\\b").is_err());
        assert!(check_name("..").is_err());
    }

    #[test]
    fn presets_answer_before_any_file() {
        assert_eq!(load_named("index").unwrap().name, "index");
        assert_eq!(load_named("neo").unwrap().name, "neo");
        assert!(load_named("nope-not-here").is_err());
    }

    /// The root `theme.toml` is a theme under its own name, so a hand-edited
    /// one is both listed and reachable — otherwise the theme on screen at
    /// startup is the one theme `theme <name>` cannot go back to.
    #[test]
    fn a_root_theme_answers_to_the_name_it_carries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("theme.toml"), "name = 'mine'\n").unwrap();
        let themes = root.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join("magic.toml"), "name = 'magic'\n").unwrap();

        assert_eq!(local_names(root), ["magic", "mine"]);
        assert_eq!(theme_file(root, "mine").unwrap().unwrap().name, "mine");
        assert_eq!(theme_file(root, "magic").unwrap().unwrap().name, "magic");
        assert!(theme_file(root, "absent").is_none());
        // A file that exists but does not parse says why, rather than going
        // missing under a name the user can see on screen.
        std::fs::write(themes.join("broken.toml"), "name = 3\n").unwrap();
        assert!(theme_file(root, "broken").unwrap().is_err());
        // Naming a file after the root theme wins over the root file itself.
        std::fs::write(
            themes.join("mine.toml"),
            "name = 'mine'\n\n[colors]\naccent = \"#010203\"\n",
        )
        .unwrap();
        assert_eq!(
            theme_file(root, "mine").unwrap().unwrap().colors.accent,
            Color::rgb(1, 2, 3)
        );
        // One entry, not two.
        assert_eq!(local_names(root), ["broken", "magic", "mine"]);
    }

    #[test]
    fn prompt_colors_can_be_overridden() {
        let theme = Theme::from_toml(
            "preset = \"neo\"\n[prompt]\ninput = \"#010203\"\n",
            Path::new("."),
        )
        .unwrap();
        let prompt = theme.prompt.unwrap();
        assert_eq!(prompt.input, Color::rgb(1, 2, 3));
        assert_eq!(prompt.path, Color::rgb(0xf5, 0xc2, 0xe7));
    }

    #[test]
    fn preset_overrides_apply_on_top() {
        let theme = Theme::from_toml(
            "preset = \"neo\"\nname = \"my neo\"\n\n[colors]\ntext = \"#010203\"\n\n[background]\ndim = 0.3\n",
            Path::new("."),
        )
        .unwrap();
        assert_eq!(theme.name, "my neo");
        assert_eq!(theme.colors.text, Color::rgb(1, 2, 3));
        assert_eq!(theme.colors.accent, Color::rgb(0x89, 0xb4, 0xfa));
        assert_eq!(theme.background.blur, 0);
        assert_eq!(theme.background.dim, 0.3);
    }

    #[test]
    fn unknown_preset_and_top_level_keys_are_rejected() {
        let error = Theme::from_toml("preset = \"neon\"\n", Path::new(".")).unwrap_err();
        assert!(error.contains("unknown preset"));
        let error = Theme::from_toml("wallpaper = \"x\"\n", Path::new(".")).unwrap_err();
        assert!(error.contains("unknown key"));
        let error = Theme::from_toml("[prompt]\nbranch = \"#ffffff\"\n", Path::new(".")).unwrap_err();
        assert!(error.contains("unknown key"));
    }

    /// The path and the `>` that closes it are one run of colour, and the
    /// identity in front is the other. `arrow` used to colour the `>`; with the
    /// `>` folded into the path there is nothing left for it, but a file that
    /// still carries it must not stop loading over that.
    #[test]
    fn the_path_and_its_arrow_share_one_colour() {
        let theme = Theme::from_toml(
            "[prompt]\nuser = \"#010000\"\npath = \"#020000\"\ninput = \"#030000\"\n",
            Path::new("."),
        )
        .unwrap();
        let prompt = theme.prompt.unwrap();
        assert_eq!(prompt.user, Color::rgb(1, 0, 0));
        assert_eq!(prompt.path, Color::rgb(2, 0, 0));
        assert_eq!(prompt.input, Color::rgb(3, 0, 0));
        // An old file: `arrow` is taken, and the rest still comes from the
        // palette rather than from a placeholder grey.
        let theme = Theme::from_toml(
            "[colors]\npink = \"#040000\"\n\n[prompt]\narrow = \"#050000\"\n",
            Path::new("."),
        )
        .unwrap();
        let prompt = theme.prompt.unwrap();
        assert_eq!(prompt.path, Color::rgb(4, 0, 0));
        assert_eq!(prompt.user, theme.colors.accent);
    }

    #[test]
    fn colors_accept_rgb_and_rgba_hex() {
        let theme = Theme::from_toml(
            "[colors]\ntext = \"#010203\"\npanel = \"#11223344\"\n",
            Path::new("."),
        )
        .unwrap();
        assert_eq!(theme.colors.text, Color::rgb(1, 2, 3));
        assert_eq!(theme.colors.panel, Color::rgba(0x11, 0x22, 0x33, 0x44));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let error =
            Theme::from_toml("[colors]\nnot_a_color = \"#ffffff\"\n", Path::new(".")).unwrap_err();
        assert!(error.contains("not_a_color"));
    }

    #[test]
    fn background_image_is_relative_to_the_theme_file() {
        let theme = Theme::from_toml(
            "[background]\nimage = \"wall.jpg\"\ndim = 0.4\n",
            Path::new("C:/themes"),
        )
        .unwrap();
        assert_eq!(
            theme.background.image.as_deref(),
            Some(Path::new("C:/themes").join("wall.jpg").as_path())
        );
        assert_eq!(theme.background.dim, 0.4);
    }

    #[test]
    fn invalid_colors_and_dim_are_rejected() {
        assert!(Theme::from_toml("[colors]\ntext = \"#fff\"\n", Path::new(".")).is_err());
        assert!(Theme::from_toml("[colors]\ntext = \"#gggggg\"\n", Path::new(".")).is_err());
        assert!(Theme::from_toml("[background]\ndim = 1.5\n", Path::new(".")).is_err());
    }
}
