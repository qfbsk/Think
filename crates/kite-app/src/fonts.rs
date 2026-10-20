use std::path::PathBuf;

use glyphon::FontSystem;
use glyphon::fontdb;

pub const MONO_NERD: &str = "JetBrainsMono NFM";
pub const MONO_FALLBACK: &str = "Consolas";

pub struct Fonts {
    pub system: FontSystem,
    pub mono: &'static str,
}

impl Fonts {
    pub fn load() -> Self {
        let mut db = fontdb::Database::new();

        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        if let Some(local) = &local {
            let dir = local.join("Microsoft/Windows/Fonts");
            for name in [
                "JetBrainsMonoNerdFontMono-Regular.ttf",
                "JetBrainsMonoNerdFontMono-Bold.ttf",
            ] {
                let path = dir.join(name);
                if path.exists() {
                    let _ = db.load_font_file(path);
                }
            }
        }

        for path in [
            "C:\\Windows\\Fonts\\consola.ttf",
            "C:\\Windows\\Fonts\\consolab.ttf",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\msyhbd.ttc",
            "C:\\Windows\\Fonts\\simhei.ttf",
            "C:\\Windows\\Fonts\\seguiemj.ttf",
        ] {
            if PathBuf::from(path).exists() {
                let _ = db.load_font_file(path);
            }
        }

        if db.is_empty() {
            db.load_system_fonts();
        }

        let has_nerd = db
            .faces()
            .any(|face| face.families.iter().any(|(name, _)| name == MONO_NERD));
        let mono = if has_nerd { MONO_NERD } else { MONO_FALLBACK };
        db.set_monospace_family(mono);
        db.set_sans_serif_family(mono);

        let system = FontSystem::new_with_locale_and_db("zh-CN".to_string(), db);
        Self { system, mono }
    }
}
