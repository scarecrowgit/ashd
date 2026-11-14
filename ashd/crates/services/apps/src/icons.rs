//! Icon name → file path. Current theme through `freedesktop-icons`, then hicolor and
//! pixmaps inside the scanned data dirs (also what makes fixtures deterministic).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Formats GPUI can draw.
const EXTS: [&str; 5] = ["svg", "png", "webp", "jpg", "jpeg"];
const SIZES: [u32; 9] = [48, 64, 128, 256, 32, 512, 24, 22, 16];

fn supported(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// `$ASHD_ICON_THEME`, else `gtk-icon-theme-name` of GTK 4/3 `settings.ini`, else hicolor.
pub fn current_icon_theme() -> String {
    if let Ok(t) = std::env::var("ASHD_ICON_THEME")
        && !t.trim().is_empty()
    {
        return t.trim().to_string();
    }
    if let Some(cfg) = dirs::config_dir() {
        for v in ["gtk-4.0", "gtk-3.0"] {
            if let Ok(text) = std::fs::read_to_string(cfg.join(v).join("settings.ini"))
                && let Some(name) = text.lines().find_map(|l| {
                    let (k, v) = l.split_once('=')?;
                    (k.trim() == "gtk-icon-theme-name").then(|| v.trim().to_string())
                })
                && !name.is_empty()
            {
                return name;
            }
        }
    }
    "hicolor".to_string()
}

pub struct IconResolver {
    theme: String,
    data_dirs: Vec<PathBuf>,
    cache: HashMap<String, Option<PathBuf>>,
}

impl IconResolver {
    pub fn new(theme: &str, data_dirs: &[PathBuf]) -> Self {
        Self { theme: theme.to_string(), data_dirs: data_dirs.to_vec(), cache: HashMap::new() }
    }

    pub fn resolve(&mut self, icon: &str) -> Option<PathBuf> {
        if let Some(hit) = self.cache.get(icon) {
            return hit.clone();
        }
        let found = self.lookup(icon);
        self.cache.insert(icon.to_string(), found.clone());
        found
    }

    fn lookup(&self, icon: &str) -> Option<PathBuf> {
        let p = Path::new(icon);
        if p.is_absolute() {
            return (p.is_file() && supported(p)).then(|| p.to_path_buf());
        }
        let stem = EXTS
            .iter()
            .find_map(|e| icon.strip_suffix(&format!(".{e}")))
            .unwrap_or(icon);
        freedesktop_icons::lookup(stem)
            .with_size(64)
            .with_theme(&self.theme)
            .with_cache()
            .find()
            .filter(|p| supported(p))
            .or_else(|| self.fallback(stem))
    }

    fn fallback(&self, name: &str) -> Option<PathBuf> {
        let mut roots: Vec<PathBuf> = self.data_dirs.clone();
        roots.push("/usr/share".into());
        for root in &roots {
            let icons = root.join("icons");
            let mut themes = vec![self.theme.as_str()];
            if self.theme != "hicolor" {
                themes.push("hicolor");
            }
            for theme in themes {
                let base = icons.join(theme);
                let scalable = base.join("scalable/apps").join(format!("{name}.svg"));
                if scalable.is_file() {
                    return Some(scalable);
                }
                for s in SIZES {
                    for ext in ["svg", "png"] {
                        let c = base.join(format!("{s}x{s}/apps")).join(format!("{name}.{ext}"));
                        if c.is_file() {
                            return Some(c);
                        }
                    }
                }
            }
            for ext in EXTS {
                let c = root.join("pixmaps").join(format!("{name}.{ext}"));
                if c.is_file() {
                    return Some(c);
                }
                let c = icons.join(format!("{name}.{ext}"));
                if c.is_file() {
                    return Some(c);
                }
            }
        }
        None
    }
}
