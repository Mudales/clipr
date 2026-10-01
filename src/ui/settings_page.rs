//! The Settings page (⚙ / ⌘,): Maccy-style preferences plus shortcut editing.
//! Every change is saved immediately.

use super::{Picker, Theme};
use crate::keys::{self, Keymap};
use crate::settings::{SearchMode, Settings, ThemeChoice};
use eframe::egui::{self, RichText, TextEdit, vec2};

/// Text being edited on the page (kept between frames).
#[derive(Default)]
pub struct Draft {
    apps: String,
    patterns: String,
    /// Shortcut text per `keys::EDITABLE` entry, and its validation error.
    keys: Vec<(String, String)>,
    key_errors: Vec<Option<String>>,
    confirm_clear: bool,
    message: Option<String>,
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    login: bool,
}

impl Draft {
    pub fn new(settings: &Settings, keymap: &Keymap) -> Self {
        let keys: Vec<(String, String)> =
            keys::EDITABLE.iter().map(|(name, _)| (name.to_string(), keymap.raw(name))).collect();
        Self {
            apps: settings.ignore_apps.join("\n"),
            patterns: settings.ignore_patterns.join("\n"),
            key_errors: vec![None; keys.len()],
            keys,
            confirm_clear: false,
            message: None,
            login: login_item_enabled(),
        }
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()
}

#[cfg(target_os = "macos")]
fn login_agent() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents/dev.clipr.plist")
}

fn login_item_enabled() -> bool {
    #[cfg(target_os = "macos")]
    return login_agent().exists();
    #[cfg(windows)]
    return crate::platform::login::enabled();
    #[cfg(target_os = "linux")]
    false
}

#[cfg(windows)]
fn set_login_item(on: bool) -> std::io::Result<()> {
    crate::platform::login::set(on)
}

/// Start at login via a LaunchAgent (the same one install.sh creates).
#[cfg(target_os = "macos")]
fn set_login_item(on: bool) -> std::io::Result<()> {
    let path = login_agent();
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    // …/clipr.app/Contents/MacOS/clipr → …/clipr.app
    let exe = std::env::current_exe()?;
    let app = exe.ancestors().nth(3).unwrap_or(&exe);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>dev.clipr</string>
    <key>ProgramArguments</key>
    <array><string>/usr/bin/open</string><string>-a</string><string>{}</string></array>
    <key>RunAtLoad</key><true/>
</dict>
</plist>
"#,
            app.display()
        ),
    )
}

fn section(ui: &mut egui::Ui, t: &Theme, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong().size(13.0).color(t.muted));
    ui.add_space(2.0);
}

fn hint(ui: &mut egui::Ui, t: &Theme, text: &str) {
    ui.label(RichText::new(text).size(11.5).color(t.muted));
}

impl Picker {
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, t: &Theme) {
        // Title bar.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Settings").size(17.0).strong().color(t.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Done").clicked() {
                    self.leave_settings(ctx);
                }
                ui.label(RichText::new("Esc").size(11.5).color(t.muted));
            });
        });
        if let Some(msg) = &self.page.message {
            ui.label(RichText::new(msg).size(12.0).color(t.accent));
        }
        ui.separator();

        let before = self.settings.clone();
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if std::env::var("CLIPR_OPEN").as_deref() == Ok("settings-bottom") {
            area = area.vertical_scroll_offset(900.0); // development aid
        }
        area.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            self.general(ui, t);
            self.storage(ui, t);
            self.appearance(ui, t);
            self.ignore(ui, t);
            self.shortcuts(ui, t);
            self.updates(ui, ctx, t);
            ui.add_space(8.0);
        });
        if self.settings != before {
            self.page.message = match self.settings.save() {
                Ok(()) => None,
                Err(e) => Some(format!("Couldn't save settings: {e}")),
            };
            if self.settings.theme != before.theme {
                self.apply_settings(ctx);
            }
        }
    }

    fn general(&mut self, ui: &mut egui::Ui, t: &Theme) {
        section(ui, t, "GENERAL");
        let s = &mut self.settings;
        ui.checkbox(&mut s.paste_automatically, "Paste automatically");
        hint(ui, t, "Off: choosing a clip only copies it; paste it yourself.");
        ui.checkbox(&mut s.close_on_click_away, "Close when clicking outside the window");
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.radio_value(&mut s.search_mode, SearchMode::Fuzzy, "Fuzzy");
            ui.radio_value(&mut s.search_mode, SearchMode::Exact, "Exact");
        });
        #[cfg(any(target_os = "macos", windows))]
        {
            let mut on = self.page.login;
            if ui.checkbox(&mut on, "Open clipr at login").changed() {
                match set_login_item(on) {
                    Ok(()) => self.page.login = on,
                    Err(e) => self.page.message = Some(format!("Couldn't change login item: {e}")),
                }
            }
        }
        #[cfg(target_os = "linux")]
        hint(ui, t, "Autostart and the global shortcut are set in your Hyprland config.");
    }

    fn storage(&mut self, ui: &mut egui::Ui, t: &Theme) {
        section(ui, t, "STORAGE");
        let s = &mut self.settings;
        ui.horizontal(|ui| {
            ui.label("History size");
            ui.add(egui::DragValue::new(&mut s.history_size).range(10..=100_000).speed(10));
            ui.label(RichText::new("clips").color(t.muted));
        });
        ui.checkbox(&mut s.save_images, "Save images");
        ui.add_enabled_ui(s.save_images, |ui| {
            ui.horizontal(|ui| {
                ui.label("Keep up to");
                ui.add(egui::DragValue::new(&mut s.image_limit).range(0..=10_000));
                ui.label(RichText::new("images").color(t.muted));
            });
        });
        hint(ui, t, "Pinned and saved clips are never removed automatically.");
        let label = if self.page.confirm_clear {
            "Click again to clear the history"
        } else {
            "Clear history…"
        };
        if ui.button(label).clicked() {
            if self.page.confirm_clear {
                self.page.message = Some(match self.db.clear() {
                    Ok(n) => format!("Cleared {n} clips (kept pinned & saved)"),
                    Err(e) => format!("Couldn't clear: {e}"),
                });
                self.page.confirm_clear = false;
            } else {
                self.page.confirm_clear = true;
            }
        }
    }

    fn appearance(&mut self, ui: &mut egui::Ui, t: &Theme) {
        section(ui, t, "APPEARANCE");
        let s = &mut self.settings;
        ui.horizontal(|ui| {
            ui.label("Theme");
            ui.radio_value(&mut s.theme, ThemeChoice::System, "System");
            ui.radio_value(&mut s.theme, ThemeChoice::Light, "Light");
            ui.radio_value(&mut s.theme, ThemeChoice::Dark, "Dark");
        });
        ui.checkbox(&mut s.show_preview, "Show image preview");
        ui.checkbox(&mut s.show_numbers, "Show 1–9 shortcuts on rows");
        ui.checkbox(&mut s.show_footer, "Show shortcut hints at the bottom");
    }

    fn ignore(&mut self, ui: &mut egui::Ui, t: &Theme) {
        section(ui, t, "IGNORE");
        ui.label("Apps (one per line)");
        hint(
            ui,
            t,
            if cfg!(target_os = "macos") {
                "App name or bundle id, e.g. 1Password or com.bitwarden.desktop"
            } else if cfg!(windows) {
                "Program name, e.g. 1Password or KeePass.exe"
            } else {
                "Window class, e.g. Bitwarden (see `hyprctl activewindow`)"
            },
        );
        let apps = ui.add(TextEdit::multiline(&mut self.page.apps).desired_rows(2).desired_width(f32::INFINITY));
        if apps.changed() {
            self.settings.ignore_apps = lines(&self.page.apps);
        }
        ui.add_space(4.0);
        ui.label("Text matching a regular expression (one per line)");
        hint(ui, t, r"e.g. ^\d{6}$ skips 6-digit codes");
        let pats = ui.add(TextEdit::multiline(&mut self.page.patterns).desired_rows(2).desired_width(f32::INFINITY));
        if pats.changed() {
            self.settings.ignore_patterns = lines(&self.page.patterns);
        }
        for p in &self.settings.ignore_patterns {
            if let Err(e) = regex_lite::Regex::new(p) {
                let first = e.to_string().lines().last().unwrap_or("").to_owned();
                ui.label(RichText::new(format!("Invalid: {p}  ({first})")).size(11.5).color(egui::Color32::from_rgb(220, 80, 70)));
            }
        }
    }

    fn updates(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, t: &Theme) {
        use crate::update::{Status, current};
        section(ui, t, "UPDATES");
        let status = self.updater.status();
        ui.horizontal(|ui| {
            ui.label(format!("clipr {}", current()));
            let busy = matches!(status, Status::Checking | Status::Updating);
            if ui.add_enabled(!busy, egui::Button::new("Check for updates")).clicked() {
                let ctx = ctx.clone();
                self.updater.check(move || ctx.request_repaint());
            }
        });
        match &status {
            Status::Idle => {}
            Status::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking…");
                });
            }
            Status::UpToDate => hint(ui, t, "You have the latest version."),
            Status::Available(v) => {
                ui.label(RichText::new(format!("Version {v} is available.")).color(t.accent));
                if cfg!(target_os = "macos") {
                    hint(ui, t, "After updating, macOS asks again for Accessibility: allow clipr.");
                }
                if ui.button(format!("Update to {v} now")).clicked() {
                    self.updater.install();
                }
            }
            Status::Updating => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Installing… clipr restarts by itself in a few seconds.");
                });
            }
            Status::Failed(e) => {
                ui.label(RichText::new(format!("Update check failed: {e}")).color(egui::Color32::from_rgb(220, 80, 70)));
            }
        }
    }

    fn shortcuts(&mut self, ui: &mut egui::Ui, t: &Theme) {
        section(ui, t, "KEYBOARD SHORTCUTS");
        hint(ui, t, "Mod = ⌘ on macOS, Ctrl on Windows/Linux. Several shortcuts: separate with commas.");
        let mut changed = false;
        egui::Grid::new("shortcuts").num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
            for (i, (name, label)) in keys::EDITABLE.iter().enumerate() {
                if *name == "hotkey" && cfg!(target_os = "linux") {
                    continue;
                }
                ui.label(*label);
                ui.vertical(|ui| {
                    let edit = ui.add(TextEdit::singleline(&mut self.page.keys[i].1).desired_width(190.0));
                    if edit.changed() {
                        self.page.key_errors[i] = keys::validate(name, self.page.keys[i].1.trim()).err();
                        changed = true;
                    }
                    if let Some(e) = &self.page.key_errors[i] {
                        ui.label(RichText::new(e).size(11.0).color(egui::Color32::from_rgb(220, 80, 70)));
                    }
                });
                ui.end_row();
            }
        });
        if changed && self.page.key_errors.iter().all(Option::is_none) {
            let values: Vec<(String, String)> =
                self.page.keys.iter().map(|(k, v)| (k.clone(), v.trim().to_owned())).collect();
            self.page.message = keys::save(&values).err().map(|e| format!("Couldn't save shortcuts: {e}"));
        }
        ui.horizontal(|ui| {
            if ui.button("Reset shortcuts").clicked() {
                let _ = std::fs::remove_file(keys::config_path());
                self.keys = Keymap::load();
                self.page = Draft::new(&self.settings, &self.keys);
                self.page.message = Some("Shortcuts reset to defaults".into());
            }
            if ui.button("Open keys.conf").clicked() {
                self.open_file(keys::ensure_config());
                self.page.message = self.status.take();
            }
        });
    }
}
