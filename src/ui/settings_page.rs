//! The Settings page (⚙ / ⌘,), styled like macOS / Maccy preferences:
//! tabs on top, rounded cards with label-left / control-right rows and
//! switches. Every change is saved immediately.

use super::{Picker, Theme};
use crate::keys::{self, Keymap};
use crate::settings::{SearchMode, Settings, ThemeChoice};
use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, TextEdit, vec2};

const RED: Color32 = Color32::from_rgb(220, 80, 70);

#[derive(Clone, Copy, PartialEq, Default)]
enum Page {
    #[default]
    General,
    Storage,
    Ignore,
    Shortcuts,
    About,
}

const PAGES: &[(Page, &str)] = &[
    (Page::General, "General"),
    (Page::Storage, "Storage"),
    (Page::Ignore, "Ignore"),
    (Page::Shortcuts, "Shortcuts"),
    (Page::About, "About"),
];

/// Text being edited on the page (kept between frames).
#[derive(Default)]
pub struct Draft {
    page: Page,
    apps: String,
    patterns: String,
    /// Shortcut text per `keys::EDITABLE` entry, and its validation error.
    keys: Vec<(String, String)>,
    key_errors: Vec<Option<String>>,
    confirm_clear: bool,
    message: Option<String>,
    login: bool,
}

impl Draft {
    pub fn new(settings: &Settings, keymap: &Keymap) -> Self {
        let keys: Vec<(String, String)> =
            keys::EDITABLE.iter().map(|(name, _)| (name.to_string(), keymap.raw(name))).collect();
        Self {
            page: Page::General,
            apps: settings.ignore_apps.join("\n"),
            patterns: settings.ignore_patterns.join("\n"),
            key_errors: vec![None; keys.len()],
            keys,
            confirm_clear: false,
            message: None,
            login: login::enabled(),
        }
    }
}

impl Draft {
    /// Development aid (CLIPR_OPEN=settings-storage): start on a given tab.
    pub fn open(&mut self, name: &str) {
        if let Some((p, _)) = PAGES.iter().find(|(_, l)| l.eq_ignore_ascii_case(name)) {
            self.page = *p;
        }
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()
}

/// Start at login, per platform.
mod login {
    #[cfg(target_os = "macos")]
    fn agent() -> std::path::PathBuf {
        dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents/dev.clipr.plist")
    }

    #[cfg(target_os = "linux")]
    fn autostart() -> std::path::PathBuf {
        dirs::config_dir().unwrap_or_default().join("autostart/clipr.desktop")
    }

    /// Linux: whether the Hyprland config starts clipr itself (then the
    /// switch can't turn that off).
    #[cfg(target_os = "linux")]
    pub fn by_compositor() -> bool {
        let dir = dirs::config_dir().unwrap_or_default().join("hypr");
        ["autostart.lua", "hyprland.lua", "hyprland.conf"].iter().any(|f| {
            std::fs::read_to_string(dir.join(f)).is_ok_and(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.starts_with('#') && !l.starts_with("--"))
                    .any(|l| l.contains("clipr") && (l.contains("launch_on_start") || l.contains("exec-once")))
            })
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn by_compositor() -> bool {
        false
    }

    pub fn enabled() -> bool {
        #[cfg(target_os = "macos")]
        return agent().exists();
        #[cfg(windows)]
        return crate::platform::login::enabled();
        #[cfg(target_os = "linux")]
        return autostart().exists() || by_compositor();
    }

    #[cfg(not(windows))]
    fn remove(path: &std::path::Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    #[cfg(not(windows))]
    fn write(path: &std::path::Path, text: String) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)
    }

    #[cfg(windows)]
    pub fn set(on: bool) -> std::io::Result<()> {
        crate::platform::login::set(on)
    }

    /// A LaunchAgent (the same one install.sh creates).
    #[cfg(target_os = "macos")]
    pub fn set(on: bool) -> std::io::Result<()> {
        if !on {
            return remove(&agent());
        }
        // …/clipr.app/Contents/MacOS/clipr → …/clipr.app
        let exe = std::env::current_exe()?;
        let app = exe.ancestors().nth(3).unwrap_or(&exe);
        write(
            &agent(),
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

    /// An XDG autostart entry (run by uwsm/systemd on Omarchy, and by most desktops).
    #[cfg(target_os = "linux")]
    pub fn set(on: bool) -> std::io::Result<()> {
        if !on {
            return remove(&autostart());
        }
        let exe = std::env::current_exe()?;
        write(
            &autostart(),
            format!(
                "[Desktop Entry]\nType=Application\nName=clipr\nComment=Clipboard history\nExec={}\nIcon=clipr\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
                exe.display()
            ),
        )
    }
}

// ---------------------------------------------------------------- widgets

/// iOS/macOS-style switch.
fn switch(ui: &mut egui::Ui, t: &Theme, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let k = ui.ctx().animate_bool_responsive(resp.id, *on);
    let off = if ui.visuals().dark_mode { Color32::from_gray(80) } else { Color32::from_gray(200) };
    let track = off.lerp_to_gamma(t.accent, k);
    let alpha = if ui.is_enabled() { 1.0 } else { 0.45 };
    let p = ui.painter();
    p.rect_filled(rect, 10.0, track.gamma_multiply(alpha));
    let x = egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), k);
    p.circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE.gamma_multiply(alpha));
    resp
}

/// Segmented control, like History / Saved in the picker.
fn segmented<T: Copy + PartialEq>(ui: &mut egui::Ui, t: &Theme, value: &mut T, options: &[(T, &str)]) -> bool {
    let font = FontId::proportional(12.5);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, l)| ui.painter().layout_no_wrap(l.to_string(), font.clone(), t.text).size().x + 20.0)
        .collect();
    let (rect, _) = ui.allocate_exact_size(vec2(widths.iter().sum::<f32>() + 4.0, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, 7.0, t.field);
    let mut x = rect.left() + 2.0;
    let mut changed = false;
    for ((v, label), w) in options.iter().zip(widths) {
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 2.0), vec2(w, rect.height() - 4.0));
        x += w;
        let resp = ui.interact(r, ui.id().with(label), Sense::click());
        let active = *value == *v;
        if active {
            ui.painter().rect_filled(r, 5.0, t.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 5.0, t.hover);
        }
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, font.clone(), if active { t.on_accent } else { t.text });
        if resp.clicked() && !active {
            *value = *v;
            changed = true;
        }
    }
    changed
}

/// A rounded group of rows.
fn card<R>(ui: &mut egui::Ui, t: &Theme, title: Option<&str>, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    if let Some(title) = title {
        ui.add_space(4.0);
        ui.label(RichText::new(title).size(12.0).strong().color(t.muted));
    }
    let r = egui::Frame::new()
        .fill(t.field)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        })
        .inner;
    ui.add_space(10.0);
    r
}

fn divider(ui: &mut egui::Ui, t: &Theme) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, t.border));
}

/// Label (and optional hint) on the left, control on the right.
fn row(ui: &mut egui::Ui, t: &Theme, label: &str, hint: Option<&str>, control: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        // Fixed height, so labels centre on the (taller) controls.
        ui.set_min_height(26.0);
        match hint {
            Some(h) => {
                ui.vertical(|ui| {
                    ui.label(RichText::new(label).size(13.5).color(t.text));
                    ui.label(RichText::new(h).size(11.5).color(t.muted));
                });
            }
            // A single line is centred on the control.
            None => {
                ui.label(RichText::new(label).size(13.5).color(t.text));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
    ui.add_space(6.0);
}

fn note(ui: &mut egui::Ui, t: &Theme, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).size(11.5).color(t.muted));
    ui.add_space(4.0);
}

impl Picker {
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, t: &Theme) {
        // Title bar.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Settings").size(17.0).strong().color(t.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let done = egui::Button::new(RichText::new("Done").color(t.on_accent))
                    .fill(t.accent)
                    .corner_radius(6.0);
                if ui.add(done).clicked() {
                    self.leave_settings(ctx);
                }
                ui.label(RichText::new("Esc").size(11.5).color(t.muted));
            });
        });
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            segmented(ui, t, &mut self.page.page, PAGES);
        });
        ui.add_space(6.0);
        if let Some(msg) = &self.page.message {
            ui.label(RichText::new(msg).size(12.0).color(t.accent));
            ui.add_space(4.0);
        }

        let before = self.settings.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.page.page {
            Page::General => self.general(ui, t),
            Page::Storage => self.storage(ui, t),
            Page::Ignore => self.ignore(ui, t),
            Page::Shortcuts => self.shortcuts(ui, t),
            Page::About => self.about(ui, ctx, t),
        });
        if self.settings != before {
            self.page.message = self.settings.save().err().map(|e| format!("Couldn't save settings: {e}"));
            if self.settings.theme != before.theme {
                self.apply_settings(ctx);
            }
        }
    }

    fn general(&mut self, ui: &mut egui::Ui, t: &Theme) {
        let s = &mut self.settings;
        card(ui, t, None, |ui| {
            row(ui, t, "Paste automatically", Some("Off: choosing a clip only copies it"), |ui| {
                switch(ui, t, &mut s.paste_automatically);
            });
            divider(ui, t);
            row(ui, t, "Close when clicking outside", None, |ui| {
                switch(ui, t, &mut s.close_on_click_away);
            });
            divider(ui, t);
            row(ui, t, "Search", None, |ui| {
                segmented(ui, t, &mut s.search_mode, &[(SearchMode::Fuzzy, "Fuzzy"), (SearchMode::Exact, "Exact")]);
            });
        });

        let compositor = login::by_compositor();
        let hint = compositor.then_some("Started by your Hyprland config");
        let mut on = self.page.login;
        let mut toggled = false;
        card(ui, t, None, |ui| {
            row(ui, t, "Open clipr at login", hint, |ui| {
                ui.add_enabled_ui(!compositor, |ui| toggled = switch(ui, t, &mut on).changed());
            });
        });
        if toggled {
            match login::set(on) {
                Ok(()) => self.page.login = login::enabled(),
                Err(e) => self.page.message = Some(format!("Couldn't change login item: {e}")),
            }
        }

        let s = &mut self.settings;
        card(ui, t, Some("APPEARANCE"), |ui| {
            row(ui, t, "Theme", None, |ui| {
                segmented(
                    ui,
                    t,
                    &mut s.theme,
                    &[(ThemeChoice::System, "System"), (ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark")],
                );
            });
            divider(ui, t);
            row(ui, t, "Image preview", None, |ui| {
                switch(ui, t, &mut s.show_preview);
            });
            divider(ui, t);
            row(ui, t, "1–9 shortcuts on rows", None, |ui| {
                switch(ui, t, &mut s.show_numbers);
            });
            divider(ui, t);
            row(ui, t, "Shortcut hints at the bottom", None, |ui| {
                switch(ui, t, &mut s.show_footer);
            });
        });
    }

    fn storage(&mut self, ui: &mut egui::Ui, t: &Theme) {
        let s = &mut self.settings;
        card(ui, t, None, |ui| {
            row(ui, t, "History size", Some("Clips kept, besides pinned & saved"), |ui| {
                ui.add(egui::DragValue::new(&mut s.history_size).range(10..=100_000).speed(10));
            });
            divider(ui, t);
            row(ui, t, "Save images", None, |ui| {
                switch(ui, t, &mut s.save_images);
            });
            divider(ui, t);
            let enabled = s.save_images;
            row(ui, t, "Images kept", None, |ui| {
                ui.add_enabled(enabled, egui::DragValue::new(&mut s.image_limit).range(0..=10_000));
            });
        });
        card(ui, t, None, |ui| {
            row(
                ui,
                t,
                "Clear history after restart",
                Some("After the computer restarts or shuts down"),
                |ui| {
                    switch(ui, t, &mut s.clear_on_restart);
                },
            );
        });
        let label = if self.page.confirm_clear { "Click again to clear the history" } else { "Clear history now…" };
        card(ui, t, None, |ui| {
            row(ui, t, "Pinned and saved clips are always kept", None, |ui| {
                let button = egui::Button::new(RichText::new(label).color(if self.page.confirm_clear {
                    Color32::WHITE
                } else {
                    RED
                }))
                .fill(if self.page.confirm_clear { RED } else { Color32::TRANSPARENT });
                if ui.add(button).clicked() {
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
            });
        });
    }

    fn ignore(&mut self, ui: &mut egui::Ui, t: &Theme) {
        let example = if cfg!(target_os = "macos") {
            "App name or bundle id, one per line, e.g. 1Password"
        } else if cfg!(windows) {
            "Program name, one per line, e.g. KeePass.exe"
        } else {
            "Window class, one per line, e.g. Bitwarden (see hyprctl activewindow)"
        };
        card(ui, t, Some("APPS"), |ui| {
            note(ui, t, &format!("Copies made in these apps are never saved. {example}"));
            let edit = TextEdit::multiline(&mut self.page.apps)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("1Password");
            if ui.add(edit).changed() {
                self.settings.ignore_apps = lines(&self.page.apps);
            }
            ui.add_space(6.0);
        });
        card(ui, t, Some("TEXT (REGULAR EXPRESSIONS)"), |ui| {
            note(ui, t, r"Text matching any line is never saved, e.g. ^\d{6}$ skips 6-digit codes");
            let edit = TextEdit::multiline(&mut self.page.patterns)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .code_editor()
                .hint_text(r"^\d{6}$");
            if ui.add(edit).changed() {
                self.settings.ignore_patterns = lines(&self.page.patterns);
            }
            for p in &self.settings.ignore_patterns {
                if let Err(e) = regex_lite::Regex::new(p) {
                    let why = e.to_string().lines().last().unwrap_or("").to_owned();
                    ui.label(RichText::new(format!("Invalid: {p}  ({why})")).size(11.5).color(RED));
                }
            }
            ui.add_space(6.0);
        });
        note(ui, t, "Copies that password managers mark as secret are always skipped.");
    }

    fn shortcuts(&mut self, ui: &mut egui::Ui, t: &Theme) {
        note(ui, t, "Mod = ⌘ on macOS, Ctrl on Windows/Linux. Several shortcuts: separate with commas.");
        let mut changed = false;
        card(ui, t, None, |ui| {
            let editable: Vec<usize> = (0..keys::EDITABLE.len())
                .filter(|&i| !(keys::EDITABLE[i].0 == "hotkey" && cfg!(target_os = "linux")))
                .collect();
            for (n, &i) in editable.iter().enumerate() {
                let (name, label) = keys::EDITABLE[i];
                if n > 0 {
                    divider(ui, t);
                }
                let error = self.page.key_errors[i].clone();
                row(ui, t, label, error.as_deref(), |ui| {
                    let edit = TextEdit::singleline(&mut self.page.keys[i].1).desired_width(170.0);
                    if ui.add(edit).changed() {
                        self.page.key_errors[i] = keys::validate(name, self.page.keys[i].1.trim()).err();
                        changed = true;
                    }
                });
            }
        });
        if changed && self.page.key_errors.iter().all(Option::is_none) {
            let values: Vec<(String, String)> =
                self.page.keys.iter().map(|(k, v)| (k.clone(), v.trim().to_owned())).collect();
            self.page.message = keys::save(&values).err().map(|e| format!("Couldn't save shortcuts: {e}"));
        }
        ui.horizontal(|ui| {
            if ui.button("Reset to defaults").clicked() {
                let _ = std::fs::remove_file(keys::config_path());
                self.keys = Keymap::load();
                let page = self.page.page;
                self.page = Draft::new(&self.settings, &self.keys);
                self.page.page = page;
                self.page.message = Some("Shortcuts reset to defaults".into());
            }
            if ui.button("Open keys.conf").clicked() {
                self.open_file(keys::ensure_config());
                self.page.message = self.status.take();
            }
        });
    }

    fn about(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, t: &Theme) {
        use crate::update::{Status, current};
        let status = self.updater.status();
        card(ui, t, None, |ui| {
            row(ui, t, &format!("clipr {}", current()), Some("Keyboard-driven clipboard history"), |ui| {
                let busy = matches!(status, Status::Checking | Status::Updating);
                if ui.add_enabled(!busy, egui::Button::new("Check for updates")).clicked() {
                    let ctx = ctx.clone();
                    self.updater.check(move || ctx.request_repaint());
                }
            });
            match &status {
                Status::Idle => {}
                Status::Checking => {
                    divider(ui, t);
                    row(ui, t, "Checking…", None, |ui| {
                        ui.spinner();
                    });
                }
                Status::UpToDate => {
                    divider(ui, t);
                    row(ui, t, "You have the latest version", None, |_| {});
                }
                Status::Available(v) => {
                    divider(ui, t);
                    let hint = cfg!(target_os = "macos").then_some("macOS will ask again for Accessibility");
                    row(ui, t, &format!("Version {v} is available"), hint, |ui| {
                        let b = egui::Button::new(RichText::new("Update now").color(t.on_accent)).fill(t.accent);
                        if ui.add(b).clicked() {
                            self.updater.install();
                        }
                    });
                }
                Status::Updating => {
                    divider(ui, t);
                    row(ui, t, "Installing… clipr restarts by itself", None, |ui| {
                        ui.spinner();
                    });
                }
                Status::Failed(e) => {
                    divider(ui, t);
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("Update check failed: {e}")).size(12.0).color(RED));
                    ui.add_space(6.0);
                }
            }
        });
        card(ui, t, None, |ui| {
            row(ui, t, "Source code", None, |ui| {
                ui.hyperlink_to("github.com/Mudales/clipr", "https://github.com/Mudales/clipr");
            });
        });
    }
}
