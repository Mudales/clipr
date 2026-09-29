//! Preferences (Maccy-style), stored in `~/.config/clipr/settings.conf`.
//!
//! Read by the picker when it opens and by the daemon on every copy, so edits
//! apply without restarting.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SearchMode {
    Fuzzy,
    Exact,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ThemeChoice {
    System,
    Light,
    Dark,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    /// Unpinned, unsaved clips kept.
    pub history_size: usize,
    /// Unpinned, unsaved images kept (they are big).
    pub image_limit: usize,
    pub save_images: bool,
    pub search_mode: SearchMode,
    /// Off: choosing a clip only copies it (Maccy's "Paste automatically").
    pub paste_automatically: bool,
    pub close_on_click_away: bool,
    pub theme: ThemeChoice,
    pub show_preview: bool,
    pub show_numbers: bool,
    pub show_footer: bool,
    /// Copies made in these apps are never saved (app name or bundle id on
    /// macOS, window class on Linux; case-insensitive).
    pub ignore_apps: Vec<String>,
    /// Text matching any of these regular expressions is never saved.
    pub ignore_patterns: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            history_size: 1000,
            image_limit: 100,
            save_images: true,
            search_mode: SearchMode::Fuzzy,
            paste_automatically: true,
            // Hyprland moves focus with the mouse, so closing on focus loss
            // would close the picker when the pointer merely leaves it.
            close_on_click_away: !cfg!(target_os = "linux"),
            theme: ThemeChoice::System,
            show_preview: true,
            show_numbers: true,
            show_footer: true,
            ignore_apps: Vec::new(),
            ignore_patterns: Vec::new(),
        }
    }
}

pub fn path() -> PathBuf {
    crate::keys::config_path().with_file_name("settings.conf")
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Ok(text) = std::fs::read_to_string(path()) else { return s };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim(), value.trim());
            let b = parse_bool(value);
            match key {
                "history_size" => s.history_size = value.parse().unwrap_or(s.history_size).clamp(10, 100_000),
                "image_limit" => s.image_limit = value.parse().unwrap_or(s.image_limit).min(10_000),
                "save_images" => s.save_images = b.unwrap_or(s.save_images),
                "search_mode" => {
                    s.search_mode = if value.eq_ignore_ascii_case("exact") { SearchMode::Exact } else { SearchMode::Fuzzy }
                }
                "paste_automatically" => s.paste_automatically = b.unwrap_or(s.paste_automatically),
                "close_on_click_away" => s.close_on_click_away = b.unwrap_or(s.close_on_click_away),
                "theme" => {
                    s.theme = match value.to_ascii_lowercase().as_str() {
                        "light" => ThemeChoice::Light,
                        "dark" => ThemeChoice::Dark,
                        _ => ThemeChoice::System,
                    }
                }
                "show_preview" => s.show_preview = b.unwrap_or(s.show_preview),
                "show_numbers" => s.show_numbers = b.unwrap_or(s.show_numbers),
                "show_footer" => s.show_footer = b.unwrap_or(s.show_footer),
                "ignore_app" if !value.is_empty() => s.ignore_apps.push(value.to_owned()),
                "ignore_pattern" if !value.is_empty() => s.ignore_patterns.push(value.to_owned()),
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        let mut out = String::from(
            "# clipr settings — edited from the picker's Settings page (⌘/Ctrl + ,).\n\
             # One ignore_app / ignore_pattern line per entry.\n\n",
        );
        let mut kv = |k: &str, v: String| out.push_str(&format!("{k} = {v}\n"));
        kv("history_size", self.history_size.to_string());
        kv("image_limit", self.image_limit.to_string());
        kv("save_images", self.save_images.to_string());
        kv("search_mode", if self.search_mode == SearchMode::Exact { "exact" } else { "fuzzy" }.into());
        kv("paste_automatically", self.paste_automatically.to_string());
        kv("close_on_click_away", self.close_on_click_away.to_string());
        kv(
            "theme",
            match self.theme {
                ThemeChoice::System => "system",
                ThemeChoice::Light => "light",
                ThemeChoice::Dark => "dark",
            }
            .into(),
        );
        kv("show_preview", self.show_preview.to_string());
        kv("show_numbers", self.show_numbers.to_string());
        kv("show_footer", self.show_footer.to_string());
        for app in &self.ignore_apps {
            kv("ignore_app", app.clone());
        }
        for pattern in &self.ignore_patterns {
            kv("ignore_pattern", pattern.clone());
        }
        let path = path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, out)
    }

    /// Whether a copy made in `app` (name, bundle id or window class) is ignored.
    pub fn ignores_app(&self, app: &str) -> bool {
        self.ignore_apps.iter().any(|a| a.eq_ignore_ascii_case(app.trim()))
    }

    /// Whether `text` matches one of the ignore patterns. Invalid patterns
    /// are skipped (the Settings page flags them).
    pub fn ignores_text(&self, text: &str) -> bool {
        self.ignore_patterns
            .iter()
            .filter_map(|p| regex_lite::Regex::new(p).ok())
            .any(|re| re.is_match(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_rules() {
        let s = Settings {
            ignore_apps: vec!["1Password".into()],
            ignore_patterns: vec![r"^\d{6}$".into(), "(".into()],
            ..Settings::default()
        };
        assert!(s.ignores_app("1password"));
        assert!(!s.ignores_app("Safari"));
        assert!(s.ignores_text("123456"));
        assert!(!s.ignores_text("hello 123456"));
    }
}
