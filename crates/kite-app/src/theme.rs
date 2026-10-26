use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use glyphon::Color;

#[derive(Debug)]
pub struct Theme {
    name: String,
    colors: Colors,
    background: Background,
    prompt: Option<Prompt>,
}

#[derive(Clone, Copy, Debug)]
pub struct Prompt {
    pub path: Color,
    pub arrow: Color,
    pub input: Color,
}

#[derive(Clone, Copy, Debug)]
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
    pub overlay_dim: Color,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wallpaper {
    Blurred,
    Sharp,
}

#[derive(Debug)]
pub struct Background {
    pub image: Option<PathBuf>,
    pub wallpaper: Wallpaper,
    pub dim: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "index".to_string(),
            colors: Colors::default(),
            background: Background::default(),
            prompt: None,
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
            overlay_dim: Color::rgba(0x11, 0x11, 0x1b, 0x59),
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
            wallpaper: Wallpaper::Blurred,
            dim: 0.18,
        }
    }
}

impl Colors {
    fn neo() -> Self {
        Self {
            bg: Color::rgb(0x1e, 0x1e, 0x2e),
            // Deep blue veils: a neutral Catppuccin base let the wallpaper's
            // teal show through as grey-green, the reference look is blue.
            panel: Color::rgba(0x0c, 0x26, 0x44, 0xb4),
            panel_dim: Color::rgba(0x08, 0x1c, 0x34, 0xc0),
            sidebar: Color::rgba(0x0c, 0x2b, 0x53, 0x98),
            status: Color::rgba(0x11, 0x11, 0x1b, 0xe0),
            border: Color::rgba(0x45, 0x47, 0x5a, 0x80),
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
            tab_active: Color::rgba(0x31, 0x32, 0x44, 0xcc),
            tab_hover: Color::rgba(0x45, 0x47, 0x5a, 0x80),
            overlay_dim: Color::rgba(0x06, 0x12, 0x26, 0x80),
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
            wallpaper: Wallpaper::Sharp,
            dim: 0.18,
        }
    }
}

impl Prompt {
    fn neo() -> Self {
        Self {
            path: Color::rgb(0xf5, 0xc2, 0xe7),
            arrow: Color::rgb(0xba, 0xc2, 0xde),
            input: Color::rgb(0xcd, 0xd6, 0xf4),
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
            },
        }
    }
}

static ACTIVE: RwLock<Option<Arc<Theme>>> = RwLock::new(None);
static SOURCE: RwLock<Option<(String, PathBuf)>> = RwLock::new(None);

/// Swap the theme while the app runs; the caller repaints everything that baked
/// colors in (buffers, labels, background texture).
pub fn set_preset(name: &str) -> Result<String, String> {
    let preset = Preset::from_name(name)?;
    let source = SOURCE.read().expect("theme source").clone();
    let theme = match &source {
        Some((text, base)) => Theme::parse(text, base, Some(name))?,
        None => preset.theme(),
    };
    let notice = theme.name.clone();
    set_active(theme);
    Ok(notice)
}

fn set_active(theme: Theme) {
    *ACTIVE.write().expect("theme lock") = Some(Arc::new(theme));
}

pub fn init() -> Option<String> {
    let path = config_path()?;
    let Some(text) = path
        .is_file()
        .then(|| std::fs::read_to_string(&path).ok())
        .flatten()
    else {
        set_active(Theme::default());
        return None;
    };
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    *SOURCE.write().expect("theme source") = Some((text.clone(), base.clone()));
    match Theme::from_toml(&text, &base) {
        Ok(theme) => {
            let notice = format!("theme: {}", theme.name);
            set_active(theme);
            Some(notice)
        }
        Err(error) => {
            set_active(Theme::default());
            Some(format!("theme.toml: {error}"))
        }
    }
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

pub fn wallpaper() -> Wallpaper {
    current().background.wallpaper
}

pub fn background_dim() -> f32 {
    current().background.dim
}

pub fn prompt() -> Option<Prompt> {
    current().prompt
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
    overlay_dim,
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

/// The heat colour for a day's `level` (0 = nothing saved, 5 = the busiest).
pub fn miku(level: usize) -> Color {
    match level {
        0 => miku_0(),
        1 => miku_1(),
        2 => miku_2(),
        3 => miku_3(),
        4 => miku_4(),
        _ => miku_5(),
    }
}

impl Theme {
    pub fn from_toml(text: &str, base: &Path) -> Result<Self, String> {
        Self::parse(text, base, None)
    }

    /// `preset_override` lets a running session switch presets while keeping the
    /// overrides the config file applies on top of them.
    fn parse(text: &str, base: &Path, preset_override: Option<&str>) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text).map_err(|error| error.to_string())?;
        let table = value
            .as_table()
            .ok_or_else(|| "theme must be a table".to_string())?;
        for key in table.keys() {
            if !matches!(
                key.as_str(),
                "preset" | "name" | "colors" | "background" | "prompt"
            ) {
                return Err(format!("unknown key `{key}`"));
            }
        }
        let preset = match preset_override
            .or_else(|| table.get("preset").and_then(|value| value.as_str()))
        {
            Some(name) => Preset::from_name(name)?,
            None => Preset::Index,
        };
        let mut theme = preset.theme();
        if preset_override.is_none() {
            if let Some(value) = table.get("name") {
                theme.name = value
                    .as_str()
                    .ok_or_else(|| "name must be a string".to_string())?
                    .trim()
                    .to_string();
            }
        }
        if let Some(value) = table.get("colors") {
            apply_colors(&mut theme.colors, value)?;
        }
        if let Some(value) = table.get("background") {
            apply_background(&mut theme.background, value, base)?;
        }
        if let Some(value) = table.get("prompt") {
            let prompt = theme.prompt.get_or_insert(Prompt {
                path: theme.colors.text_dim,
                arrow: theme.colors.text_dim,
                input: theme.colors.yellow,
            });
            apply_prompt(prompt, value)?;
        }
        if theme.name.is_empty() {
            theme.name = "custom".to_string();
        }
        Ok(theme)
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
            "overlay_dim" => &mut colors.overlay_dim,
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
            other => return Err(format!("background: unknown key `{other}`")),
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
            "path" => prompt.path = color,
            "arrow" => prompt.arrow = color,
            "input" => prompt.input = color,
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

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".config"))
    })?;
    Some(base.join("kite").join("theme.toml"))
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
        assert_eq!(theme.background.wallpaper, Wallpaper::Blurred);
    }

    #[test]
    fn neo_preset_is_transparent_and_uses_the_sharp_wallpaper() {
        let theme = Theme::from_toml("preset = \"neo\"\n", Path::new(".")).unwrap();
        assert_eq!(theme.name, "neo");
        assert_eq!(theme.background.wallpaper, Wallpaper::Sharp);
        assert_eq!(theme.background.dim, 0.18);
        assert!(theme.colors.panel.a() < Colors::default().panel.a());
        assert!(theme.colors.sidebar.a() < Colors::default().sidebar.a());
    }

    #[test]
    fn neo_highlights_the_prompt_and_input() {
        let theme = Theme::from_toml("preset = \"neo\"\n", Path::new(".")).unwrap();
        let prompt = theme.prompt.unwrap();
        assert_eq!(prompt.path, Color::rgb(0xf5, 0xc2, 0xe7));
        assert_eq!(prompt.arrow, Color::rgb(0xba, 0xc2, 0xde));
        assert_eq!(prompt.input, Color::rgb(0xcd, 0xd6, 0xf4));
        assert!(Theme::default().prompt.is_none());
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
        assert_eq!(theme.background.wallpaper, Wallpaper::Sharp);
        assert_eq!(theme.background.dim, 0.3);
    }

    #[test]
    fn preset_override_keeps_colors_but_not_the_custom_name() {
        let theme = Theme::parse(
            "preset = \"index\"\nname = \"mine\"\n\n[colors]\ntext = \"#010203\"\n",
            Path::new("."),
            Some("neo"),
        )
        .unwrap();
        assert_eq!(theme.name, "neo");
        assert_eq!(theme.colors.text, Color::rgb(0x01, 0x02, 0x03));
    }

    #[test]
    fn unknown_preset_and_top_level_keys_are_rejected() {
        let error = Theme::from_toml("preset = \"neon\"\n", Path::new(".")).unwrap_err();
        assert!(error.contains("unknown preset"));
        let error = Theme::from_toml("wallpaper = \"x\"\n", Path::new(".")).unwrap_err();
        assert!(error.contains("unknown key"));
        let error = Theme::from_toml("[prompt]\nuser = \"#ffffff\"\n", Path::new(".")).unwrap_err();
        assert!(error.contains("unknown key"));
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
