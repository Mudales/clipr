//! The picker popup: fuzzy search over history / saved clips, fully keyboard driven.

use crate::db::{Clip, Db};
use crate::images::parse_key;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use crate::ipc;
use crate::keys::{self, Action, Keymap};
use crate::settings::{SearchMode, Settings, ThemeChoice};

mod settings_page;
use anyhow::{Result, anyhow};
use eframe::egui::{
    self, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, Modifiers, Sense,
    Stroke, TextEdit, ViewportCommand, pos2, vec2,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

const ROW_H: f32 = 28.0;
/// The list column; the preview pane (if on) adds `PREVIEW_W` to its right.
const LIST_W: f32 = 480.0;
const PREVIEW_W: f32 = 330.0;
const WINDOW_H: f32 = 440.0;

fn window_size(preview: bool) -> egui::Vec2 {
    vec2(if preview { LIST_W + PREVIEW_W } else { LIST_W }, WINDOW_H)
}
const CORNER: f32 = 12.0;
/// Transparent window with our own rounded corners on macOS; on Linux the
/// compositor rounds the (opaque) window itself.
const TRANSPARENT: bool = cfg!(target_os = "macos");
const PREVIEW_CHARS: usize = 160;
/// Only the start of very long clips is searched, to keep typing instant.
const SEARCH_CHARS: usize = 4096;

/// Preferred UI font (the macOS system font); egui's default is used otherwise.
const UI_FONTS: &[&str] = &["/System/Library/Fonts/SFNS.ttf", "C:\\Windows\\Fonts\\segoeui.ttf"];

/// Fonts tried (in order) as a fallback for Hebrew, Arabic, Cyrillic, etc.
const FALLBACK_FONTS: &[&str] = &[
    "/System/Library/Fonts/SFHebrew.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "C:\\Windows\\Fonts\\arial.ttf",
    "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
];

/// `OneShot`: a short-lived process that exits after picking (Linux).
/// `Resident`: lives inside the daemon and is hidden/shown (macOS and Windows,
/// where a freshly spawned process is not allowed to take focus).
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    OneShot,
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    Resident,
}

static TOGGLE_REQUESTED: AtomicBool = AtomicBool::new(false);
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Close the window and leave the event loop (so the process exits normally).
/// Safe to call from any thread; does nothing if there's no window.
pub fn request_quit() {
    QUIT_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(ctx) = CONTEXT.get() {
        ctx.request_repaint();
    }
}
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// Show/hide the resident picker. Safe to call from any thread.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub fn request_toggle() {
    TOGGLE_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(ctx) = CONTEXT.get() {
        ctx.request_repaint();
    }
}

/// A right-click menu entry.
#[derive(Clone, Copy)]
enum MenuChoice {
    Key(Action),
    ClipAction(usize),
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    List,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    History,
    Saved,
}

struct Item {
    clip: Clip,
    preview: String,
    /// Pixel size, for image clips.
    image: Option<(u32, u32)>,
    /// Starts with Hebrew/Arabic: right-align so the beginning stays visible.
    rtl: bool,
}

struct Picker {
    db: Db,
    mode: Mode,
    visible: bool,
    tab: Tab,
    items: Vec<Item>,
    /// Indices into `items` that match the query, best first.
    filtered: Vec<usize>,
    query: String,
    selected: usize,
    matcher: Matcher,
    status: Option<String>,
    was_focused: bool,
    scroll_to_selected: bool,
    scroll_offset: f32,
    view_height: f32,
    keys: Keymap,
    settings: Settings,
    updater: crate::update::Updater,
    view: View,
    /// Edits in progress on the Settings page.
    page: settings_page::Draft,
    /// Waiting for "Delete N clips?" to be confirmed.
    confirm_delete: bool,
    /// The status line offers Undo (right after a delete).
    offer_undo: bool,
    /// The footer's Undo was clicked (footer takes &self).
    undo_requested: AtomicBool,
    /// The Actions menu (Mod+K) is open, with this entry highlighted.
    actions_menu: Option<usize>,
    /// Multi-selection (⌘A / Shift+↑↓), by clip id.
    marked: HashSet<i64>,
    /// Decoded thumbnails by clip id (`None` = failed to load).
    thumbs: RefCell<HashMap<i64, Option<egui::TextureHandle>>>,
}

pub fn run(mode: Mode) -> Result<()> {
    let db = Db::open()?;
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("clipr")
            .with_app_id("clipr")
            .with_inner_size(window_size(Settings::load().show_preview))
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(TRANSPARENT)
            .with_always_on_top()
            .with_icon(window_icon())
            .with_visible(mode == Mode::OneShot),
        centered: true,
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }));
    }
    eframe::run_native(
        "clipr",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            let _ = CONTEXT.set(cc.egui_ctx.clone());
            let mut picker = Picker::new(db, mode);
            picker.status = crate::update::take_updated_notice().or_else(|| picker.keys.errors.first().cloned());
            picker.apply_settings(&cc.egui_ctx);
            picker.reload();
            // Development aid (with CLIPR_SCREENSHOT): start on the Settings page.
            // Development aid: CLIPR_OPEN=undo shows the "Deleted … Undo" state.
            if std::env::var("CLIPR_OPEN").as_deref() == Ok("undo") {
                picker.status = Some("Deleted 3 clips".into());
                picker.offer_undo = true;
            }
            // Development aid: CLIPR_OPEN=quit quits right away (tests quit_soon).
            if std::env::var("CLIPR_OPEN").as_deref() == Ok("quit") {
                crate::update::quit_soon();
            }
            // Development aid: CLIPR_SELECT=n selects row n.
            if let Some(n) = std::env::var("CLIPR_SELECT").ok().and_then(|v| v.parse().ok()) {
                picker.selected = n;
            }
            // Development aid: CLIPR_OPEN=actions opens the Actions menu.
            if std::env::var("CLIPR_OPEN").as_deref() == Ok("actions") {
                picker.open_actions_menu();
            }
            // Development aid: CLIPR_OPEN=confirm shows the delete dialog.
            if std::env::var("CLIPR_OPEN").as_deref() == Ok("confirm") {
                picker.marked = picker.items.iter().map(|it| it.clip.id).collect();
                picker.request_delete_marked();
            }
            if let Ok(v) = std::env::var("CLIPR_OPEN") {
                if let Some(page) = v.strip_prefix("settings") {
                    picker.enter_settings();
                    picker.page.open(page.trim_start_matches('-'));
                }
            }
            Ok(Box::new(picker))
        }),
    )
    .map_err(|e| anyhow!("{e}"))
}

/// Colors in the spirit of a native macOS popover.
struct Theme {
    bg: Color32,
    border: Color32,
    text: Color32,
    muted: Color32,
    field: Color32,
    hover: Color32,
    accent: Color32,
    on_accent: Color32,
    gold: Color32,
}

impl Theme {
    fn new(dark: bool) -> Self {
        let rgba = Color32::from_rgba_unmultiplied;
        if dark {
            Self {
                bg: rgba(36, 36, 38, 255),
                border: rgba(255, 255, 255, 30),
                text: Color32::from_rgb(236, 236, 240),
                muted: Color32::from_rgb(145, 145, 152),
                field: rgba(255, 255, 255, 16),
                hover: rgba(255, 255, 255, 12),
                accent: Color32::from_rgb(10, 100, 220),
                on_accent: Color32::WHITE,
                gold: Color32::from_rgb(240, 190, 70),
            }
        } else {
            Self {
                bg: rgba(250, 250, 252, 255),
                border: rgba(0, 0, 0, 36),
                text: Color32::from_rgb(28, 28, 30),
                muted: Color32::from_rgb(118, 118, 126),
                field: rgba(0, 0, 0, 12),
                hover: rgba(0, 0, 0, 8),
                accent: Color32::from_rgb(10, 100, 220),
                on_accent: Color32::WHITE,
                gold: Color32::from_rgb(200, 140, 20),
            }
        }
    }
}

/// The app icon for the window (Dock / taskbar / Alt-Tab).
fn window_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/clipr-256.png"))
        .map(|i| i.into_rgba8())
        .unwrap_or_default();
    egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Some(bytes) = UI_FONTS.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts.font_data.insert("ui".into(), Arc::new(FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
    }
    if let Some(bytes) = FALLBACK_FONTS.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts.font_data.insert("fallback".into(), Arc::new(FontData::from_owned(bytes)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("fallback".into());
        }
    }
    ctx.set_fonts(fonts);
}

fn is_rtl(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

/// egui's shaper flips each run of Hebrew/Arabic letters itself, but does not
/// reorder words, so a multi-word Hebrew phrase shows its words backwards.
/// We compute the full visual order (unicode bidi), then un-flip each run of
/// RTL letters so the shaper's flip lands them the right way round.
fn visual_order(s: &str) -> String {
    if !s.chars().any(is_rtl) {
        return s.to_owned();
    }
    let bidi = unicode_bidi::BidiInfo::new(s, None);
    let Some(para) = bidi.paragraphs.first() else { return s.to_owned() };
    let visual: Vec<char> = bidi.reorder_line(para, para.range.clone()).chars().collect();

    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < visual.len() {
        if is_rtl(visual[i]) {
            let start = i;
            while i < visual.len() && is_rtl(visual[i]) {
                i += 1;
            }
            out.extend(visual[start..i].iter().rev());
        } else {
            out.push(visual[i]);
            i += 1;
        }
    }
    out
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
fn time_ago(ms: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(ms);
    let s = (now - ms).max(0) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// A small gear icon (drawn, so it doesn't depend on the font).
fn paint_gear(painter: &egui::Painter, c: egui::Pos2, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = vec2(a.cos(), a.sin());
        painter.line_segment([c + dir * 5.0, c + dir * 7.5], Stroke::new(2.4, color));
    }
    painter.circle_stroke(c, 5.0, stroke);
    painter.circle_filled(c, 1.8, color);
}

/// Largest rect with `size`'s aspect ratio that fits centered in `bounds`.
fn fit_rect(size: egui::Vec2, bounds: egui::Rect) -> egui::Rect {
    let scale = (bounds.width() / size.x).min(bounds.height() / size.y);
    egui::Rect::from_center_size(bounds.center(), size * scale)
}

fn starts_rtl(s: &str) -> bool {
    s.chars().find(|c| c.is_alphabetic()).is_some_and(is_rtl)
}

fn preview(content: &str) -> String {
    let mut out = String::new();
    for word in content.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() > PREVIEW_CHARS {
            break;
        }
    }
    let mut out: String = out.chars().take(PREVIEW_CHARS).collect();
    if content.lines().nth(1).is_some() || out.len() < content.trim().len() {
        out.push_str(" …");
    }
    visual_order(&out)
}

fn search_prefix(s: &str) -> &str {
    match s.char_indices().nth(SEARCH_CHARS) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

impl Picker {
    fn new(db: Db, mode: Mode) -> Self {
        Self {
            db,
            mode,
            visible: mode == Mode::OneShot,
            tab: Tab::History,
            items: Vec::new(),
            filtered: Vec::new(),
            query: String::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
            was_focused: false,
            scroll_to_selected: true,
            scroll_offset: 0.0,
            view_height: 0.0,
            thumbs: RefCell::default(),
            marked: HashSet::new(),
            status: None,
            keys: Keymap::load(),
            settings: Settings::load(),
            updater: Default::default(),
            confirm_delete: false,
            offer_undo: false,
            undo_requested: AtomicBool::new(false),
            actions_menu: None,
            view: View::List,
            page: settings_page::Draft::default(),
        }
    }

    fn thumb(&self, ctx: &egui::Context, id: i64) -> Option<egui::TextureHandle> {
        self.thumbs
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| {
                let png = self.db.thumb(id).ok().flatten()?;
                let img = image::load_from_memory(&png).ok()?.into_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                Some(ctx.load_texture(format!("thumb{id}"), color, egui::TextureOptions::LINEAR))
            })
            .clone()
    }

    fn reload(&mut self) {
        let clips = match self.tab {
            Tab::History => self.db.history(),
            Tab::Saved => self.db.saved(),
        };
        match clips {
            Ok(clips) => {
                self.items = clips
                    .into_iter()
                    .map(|clip| Item {
                        image: clip.is_image.then(|| parse_key(&clip.content)).flatten(),
                        preview: match parse_key(&clip.content) {
                            Some((w, h)) if clip.is_image => format!("Image  {w} × {h}"),
                            _ => preview(&clip.content),
                        },
                        rtl: starts_rtl(&clip.content),
                        clip,
                    })
                    .collect();
            }
            Err(e) => self.status = Some(format!("database error: {e}")),
        }
        self.refilter();
    }

    fn refilter(&mut self) {
        let query = self.query.trim();
        if query.is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else if self.settings.search_mode == SearchMode::Exact {
            let needle = query.to_lowercase();
            self.filtered = (0..self.items.len())
                .filter(|&i| search_prefix(&self.items[i].clip.content).to_lowercase().contains(&needle))
                .collect();
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let hay = Utf32Str::new(search_prefix(&item.clip.content), &mut buf);
                    pattern.score(hay, &mut self.matcher).map(|s| (s, i))
                })
                .collect();
            // Stable sort keeps recency order among equal scores.
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        self.scroll_to_selected = true;
    }

    fn current(&self) -> Option<&Clip> {
        self.filtered.get(self.selected).map(|&i| &self.items[i].clip)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let last = self.filtered.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.scroll_to_selected = true;
    }

    fn switch_tab(&mut self) {
        self.tab = match self.tab {
            Tab::History => Tab::Saved,
            Tab::Saved => Tab::History,
        };
        self.selected = 0;
        self.marked.clear();
        self.reload();
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.keys = Keymap::load(); // pick up edits to keys.conf
        self.settings = Settings::load();
        self.view = View::List;
        self.apply_settings(ctx);
        self.query.clear();
        self.marked.clear();
        let notice = crate::update::take_updated_notice();
        self.selected = 0;
        self.status = notice.or_else(|| self.keys.errors.first().cloned());
        self.was_focused = false;
        self.reload();
        self.visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        #[cfg(any(target_os = "macos", windows))]
        crate::platform::activate_self();
        #[cfg(windows)]
        crate::platform::round_corners();
    }

    /// `restore_focus`: give focus back to the app that was active before
    /// (not wanted when the user clicked away into another app).
    fn close(&mut self, ctx: &egui::Context, restore_focus: bool) {
        match self.mode {
            Mode::OneShot => ctx.send_viewport_cmd(ViewportCommand::Close),
            Mode::Resident => {
                self.visible = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                if restore_focus {
                    crate::daemon::restore_focus();
                }
            }
        }
    }

    /// Hands the chosen clip to the daemon (which outlives this window) and closes.
    fn choose(&mut self, ctx: &egui::Context, action: &str) {
        let ids = self.target_ids();
        if ids.is_empty() {
            return;
        }
        let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
        match ipc::send(&format!("{action} {}", ids.join(","))) {
            Ok(()) => self.close(ctx, false),
            Err(_) => self.status = Some("clipr daemon is not running — start it with `clipr`".into()),
        }
    }

    /// The marked clips in list order, or just the current one.
    fn target_ids(&self) -> Vec<i64> {
        if self.marked.is_empty() {
            return self.current().map(|c| c.id).into_iter().collect();
        }
        self.filtered
            .iter()
            .map(|&i| self.items[i].clip.id)
            .filter(|id| self.marked.contains(id))
            .collect()
    }

    fn select_id(&mut self, id: i64) {
        if let Some(pos) = self.filtered.iter().position(|&i| self.items[i].clip.id == id) {
            self.selected = pos;
            self.scroll_to_selected = true;
        }
    }

    /// Applies `f` to the current clip, keeping the selection on that clip
    /// even if it moves (e.g. pinning moves it to the top).
    fn edit_current(&mut self, f: impl FnOnce(&Db, &Clip) -> Result<()>) {
        if let Some(clip) = self.current().cloned() {
            if let Err(e) = f(&self.db, &clip) {
                self.status = Some(format!("database error: {e}"));
            }
            self.reload();
            self.select_id(clip.id);
        }
    }

    fn undo_delete(&mut self) {
        self.offer_undo = false;
        self.status = Some(match self.db.undo() {
            Ok(0) => "Nothing to undo".into(),
            Ok(1) => "Restored 1 clip".into(),
            Ok(n) => format!("Restored {n} clips"),
            Err(e) => format!("database error: {e}"),
        });
        self.reload();
    }

    fn open_actions_menu(&mut self) {
        if self.settings.actions.is_empty() {
            self.status = Some("No actions yet: add some in Settings → Actions".into());
        } else if self.current().is_some_and(|c| c.is_image) {
            self.status = Some("Actions work on text clips".into());
        } else if self.current().is_some() {
            self.actions_menu = Some(0);
        }
    }

    /// Runs action `n` on the selected clip(s) in the daemon, then closes.
    fn run_clip_action(&mut self, ctx: &egui::Context, n: usize) {
        self.actions_menu = None;
        let ids: Vec<String> = self.target_ids().iter().map(i64::to_string).collect();
        if ids.is_empty() {
            return;
        }
        match ipc::send(&format!("ACTION {n} {}", ids.join(","))) {
            Ok(()) => self.close(ctx, false),
            Err(_) => self.status = Some("clipr daemon is not running — start it with `clipr`".into()),
        }
    }

    fn actions_menu_keys(&mut self, ctx: &egui::Context, highlight: usize) {
        let count = self.settings.actions.len();
        let mut run = None;
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                self.actions_menu = None;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                self.actions_menu = Some((highlight + 1) % count);
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                self.actions_menu = Some((highlight + count - 1) % count);
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) {
                run = Some(highlight);
            }
            let digits = [
                Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5,
                Key::Num6, Key::Num7, Key::Num8, Key::Num9,
            ];
            for (n, key) in digits.into_iter().enumerate() {
                if n < count && i.consume_key(Modifiers::NONE, key) {
                    run = Some(n);
                }
            }
            // Keep the search box from seeing typed text while the menu is open.
            i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
        });
        if let Some(n) = run {
            self.run_clip_action(ctx, n);
        }
    }

    /// The Actions menu, floating next to the selected row.
    fn actions_menu_ui(&mut self, ctx: &egui::Context, t: &Theme, list: egui::Rect) {
        let Some(highlight) = self.actions_menu else { return };
        let row_y = list.top() + self.selected as f32 * ROW_H - self.scroll_offset;
        let below = row_y + ROW_H + 4.0;
        let height = self.settings.actions.len() as f32 * 26.0 + 34.0;
        let y = if below + height > list.bottom() { (row_y - height - 4.0).max(list.top()) } else { below };
        let mut run = None;
        egui::Area::new(egui::Id::new("actions_menu"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos2(list.right() - 250.0, y))
            .fade_in(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(t.bg.to_opaque())
                    .stroke(Stroke::new(1.0, t.border))
                    .corner_radius(10.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.set_width(236.0);
                        ui.label(egui::RichText::new("Actions").size(11.5).color(t.muted));
                        for (n, action) in self.settings.actions.iter().enumerate() {
                            let (rect, resp) = ui.allocate_exact_size(vec2(236.0, 24.0), Sense::click());
                            let on = n == highlight || resp.hovered();
                            if on {
                                ui.painter().rect_filled(rect, 6.0, t.accent);
                            }
                            let fg = if on { t.on_accent } else { t.text };
                            let dim = if on { t.on_accent } else { t.muted };
                            let font = FontId::proportional(13.0);
                            ui.painter().text(rect.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, &action.name, font, fg);
                            let tag = match action.then {
                                crate::actions::Then::Paste => "paste",
                                crate::actions::Then::Copy => "copy",
                                crate::actions::Then::Run => "run",
                            };
                            let hint = if n < 9 { format!("{tag}   {}", n + 1) } else { tag.to_owned() };
                            ui.painter().text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, hint, FontId::proportional(11.5), dim);
                            if resp.clicked() {
                                run = Some(n);
                            }
                        }
                    });
            });
        if let Some(n) = run {
            self.run_clip_action(ctx, n);
        }
    }

    /// Marked clips that a delete would remove (pinned and saved are kept).
    fn deletable_marked(&self) -> Vec<i64> {
        self.items
            .iter()
            .filter(|it| self.marked.contains(&it.clip.id) && !it.clip.pinned && !it.clip.saved)
            .map(|it| it.clip.id)
            .collect()
    }

    /// Delete with a multi-selection: ask first (see `confirm_dialog`).
    fn request_delete_marked(&mut self) {
        if self.deletable_marked().is_empty() {
            self.status = Some("Nothing to delete: pinned and saved clips are kept".into());
        } else {
            self.confirm_delete = true;
        }
    }

    /// "Delete N clips?" with Cancel / Delete (Esc / Enter).
    fn confirm_dialog(&mut self, ctx: &egui::Context, t: &Theme) {
        let doomed = self.deletable_marked().len();
        let kept = self.marked.len() - doomed;
        let (mut yes, mut no) = ctx.input_mut(|i| {
            (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape))
        });
        let id = egui::Id::new("confirm_delete");
        let area = egui::Modal::default_area(id).order(egui::Order::Tooltip).fade_in(false);
        let frame = egui::Frame::new()
            .fill(t.bg.to_opaque())
            .stroke(Stroke::new(1.0, t.border))
            .corner_radius(12.0)
            .inner_margin(16.0);
        let modal = egui::Modal::new(id).area(area).frame(frame).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.label(egui::RichText::new(format!("Delete {doomed} clips?")).size(16.0).strong());
            ui.add_space(4.0);
            ui.label(match kept {
                0 => "Pinned and saved clips are kept.".to_owned(),
                k => format!("{k} pinned/saved clips in the selection are kept."),
            });
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let delete = egui::Button::new(egui::RichText::new("Delete").color(Color32::WHITE))
                    .fill(Color32::from_rgb(220, 70, 60));
                if ui.add(delete).clicked() {
                    yes = true;
                }
                if ui.button("Cancel").clicked() {
                    no = true;
                }
                ui.label(egui::RichText::new("↩ / Esc").size(11.0).color(t.muted));
            });
        });
        if modal.backdrop_response.clicked() {
            no = true;
        }
        if yes {
            self.confirm_delete = false;
            self.delete_marked();
        } else if no {
            self.confirm_delete = false;
        }
    }

    /// Deletes the marked clips, except pinned and saved ones.
    fn delete_marked(&mut self) {
        let doomed = self.deletable_marked();
        let kept = self.marked.len() - doomed.len();
        if let Err(e) = self.db.delete_many(&doomed) {
            self.status = Some(format!("database error: {e}"));
        }
        self.marked.clear();
        self.selected = 0;
        self.reload();
        if self.status.is_none() {
            self.status = Some(match kept {
                0 => format!("Deleted {} clips", doomed.len()),
                _ => format!("Deleted {} clips, kept {kept} pinned/saved", doomed.len()),
            });
            self.offer_undo = true;
        }
    }

    fn toggle_mark(&mut self) {
        if let Some(id) = self.current().map(|c| c.id) {
            if !self.marked.insert(id) {
                self.marked.remove(&id);
            }
        }
    }

    fn apply_settings(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.settings.theme {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        });
        // Wider window when the preview pane is on (Settings stays narrow).
        let want = window_size(self.settings.show_preview && self.view == View::List);
        let have = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
        if have.is_some_and(|h| (h.x - want.x).abs() > 1.0) {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(want));
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.confirm_delete {
            return; // the dialog handles Enter / Esc
        }
        if let Some(highlight) = self.actions_menu {
            self.actions_menu_keys(ctx, highlight);
            return;
        }
        if self.undo_requested.swap(false, Ordering::SeqCst) {
            self.undo_delete();
        }
        if self.view == View::Settings {
            // Let the settings fields have every key except Esc (= back).
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.leave_settings(ctx);
            }
            return;
        }
        let mut actions = Vec::new();
        let mut quick = None;
        ctx.input_mut(|i| {
            actions = self.keys.take_actions(i);
            // Shift+↑/↓ extends the multi-selection (checked before plain arrows).
            for (key, delta) in [(Key::ArrowDown, 1), (Key::ArrowUp, -1)] {
                if i.consume_key(Modifiers::SHIFT, key) {
                    if self.marked.is_empty() {
                        self.toggle_mark();
                    }
                    self.move_selection(delta);
                    if let Some(id) = self.current().map(|c| c.id) {
                        self.marked.insert(id);
                    }
                }
            }
            for (key, delta) in [
                (Key::ArrowDown, 1),
                (Key::ArrowUp, -1),
                (Key::PageDown, 10),
                (Key::PageUp, -10),
                // Home / End: first / last clip (the search box doesn't need them).
                (Key::Home, isize::MIN / 2),
                (Key::End, isize::MAX / 2),
            ] {
                if i.consume_key(Modifiers::NONE, key) {
                    self.move_selection(delta);
                }
            }
            // Plain Tab would move keyboard focus out of the search field.
            i.consume_key(Modifiers::NONE, Key::Tab);
            i.consume_key(Modifiers::SHIFT, Key::Tab);
            let digits = [
                Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5,
                Key::Num6, Key::Num7, Key::Num8, Key::Num9,
            ];
            if self.keys.quick != Modifiers::NONE {
                for (n, key) in digits.into_iter().enumerate() {
                    if i.consume_key(self.keys.quick, key) {
                        quick = Some(n);
                    }
                }
            }
        });

        if let Some(n) = quick {
            if n < self.filtered.len() {
                self.selected = n;
                self.marked.clear();
                actions.push(Action::Paste);
            }
        }
        for action in actions {
            self.run_action(ctx, action);
        }
    }

    fn run_action(&mut self, ctx: &egui::Context, action: Action) {
        if action != Action::Undo {
            self.offer_undo = false;
        }
        {
            if matches!(action, Action::Save | Action::Pin | Action::Delete) {
                self.status = None;
            }
            match action {
                // "Paste automatically" off: choosing a clip only copies it.
                Action::Paste if !self.settings.paste_automatically => self.choose(ctx, "COPY"),
                Action::Paste => self.choose(ctx, "PASTE"),
                Action::Copy => self.choose(ctx, "COPY"),
                Action::Type => self.choose(ctx, "TYPE"),
                Action::Pin => self.edit_current(|db, c| db.set_pinned(c.id, !c.pinned)),
                Action::Save => self.edit_current(|db, c| db.set_saved(c.id, !c.saved)),
                Action::Delete if !self.marked.is_empty() => self.request_delete_marked(),
                Action::Delete => {
                    self.edit_current(|db, c| db.delete(c.id));
                    if self.status.is_none() {
                        self.status = Some("Deleted 1 clip".into());
                        self.offer_undo = true;
                    }
                }
                Action::Undo => self.undo_delete(),
                Action::SelectAll => {
                    let all: HashSet<i64> = self.filtered.iter().map(|&i| self.items[i].clip.id).collect();
                    self.marked = if self.marked == all { HashSet::new() } else { all };
                }
                Action::NextTab | Action::PrevTab => self.switch_tab(),
                Action::Close if !self.marked.is_empty() => self.marked.clear(),
                Action::Close => self.close(ctx, true),
                Action::Settings => self.enter_settings(),
                Action::Actions => self.open_actions_menu(),
            }
        }
    }

    fn enter_settings(&mut self) {
        self.page = settings_page::Draft::new(&self.settings, &self.keys);
        self.view = View::Settings;
        self.status = None;
        if let Some(ctx) = CONTEXT.get() {
            self.apply_settings(ctx); // narrow window for Settings
        }
    }

    fn leave_settings(&mut self, ctx: &egui::Context) {
        self.view = View::List;
        self.keys = Keymap::load();
        self.status = self.keys.errors.first().cloned();
        self.apply_settings(ctx);
        self.reload();
    }

    /// Opens a config file in the default text editor.
    fn open_file(&mut self, path: std::path::PathBuf) {
        let result = if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg("-t").arg(&path).spawn()
        } else if cfg!(windows) {
            std::process::Command::new("notepad.exe").arg(&path).spawn()
        } else {
            std::process::Command::new("xdg-open").arg(&path).spawn()
        };
        self.status = Some(match result {
            Ok(_) => format!("Editing {} — changes apply next time clipr opens", path.display()),
            Err(e) => format!("Couldn't open {}: {e}", path.display()),
        });
    }

    /// Search field with a magnifier glyph, plus a History/Saved segmented control.
    fn header(&mut self, ui: &mut egui::Ui, t: &Theme) {
        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
        let seg_w = 150.0;
        let gear_w = 30.0;
        let field = row.with_max_x(row.right() - seg_w - gear_w - 12.0);
        let seg = row.with_min_x(row.right() - seg_w - gear_w - 4.0).with_max_x(row.right() - gear_w - 4.0);
        let gear = row.with_min_x(row.right() - gear_w);
        let gear_resp = ui.interact(gear, ui.id().with("gear"), Sense::click());
        ui.painter().rect_filled(gear, 8.0, if gear_resp.hovered() { t.hover } else { t.field });
        paint_gear(ui.painter(), gear.center(), t.muted);
        if gear_resp.on_hover_text("Settings").clicked() {
            self.enter_settings();
        }

        let painter = ui.painter();
        painter.rect_filled(field, 8.0, t.field);
        // Magnifier: circle + handle.
        let c = field.left_center() + vec2(15.0, -1.0);
        let stroke = Stroke::new(1.6, t.muted);
        painter.circle_stroke(c, 5.0, stroke);
        painter.line_segment([c + vec2(3.6, 3.6), c + vec2(7.5, 7.5)], stroke);

        let edit_rect = field.with_min_x(field.left() + 30.0).with_max_x(field.right() - 6.0);
        let search = ui.put(
            edit_rect,
            TextEdit::singleline(&mut self.query)
                .frame(egui::Frame::NONE)
                .hint_text(egui::RichText::new("Search").color(t.muted))
                .text_color(t.text)
                .font(FontId::proportional(15.0))
                .vertical_align(egui::Align::Center)
                .desired_width(f32::INFINITY),
        );
        if !search.has_focus() {
            search.request_focus();
        }
        if search.changed() {
            self.selected = 0;
            self.marked.clear();
            self.refilter();
        }

        // Segmented control.
        let painter = ui.painter();
        painter.rect_filled(seg, 8.0, t.field);
        let half = seg_w / 2.0;
        for (i, (tab, label)) in [(Tab::History, "History"), (Tab::Saved, "Saved")].into_iter().enumerate() {
            let r = egui::Rect::from_min_size(seg.min + vec2(i as f32 * half, 0.0), vec2(half, seg.height()))
                .shrink(3.0);
            let resp = ui.interact(r, ui.id().with(label), Sense::click());
            let active = self.tab == tab;
            if active {
                ui.painter().rect_filled(r, 6.0, t.accent);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 6.0, t.hover);
            }
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(13.0),
                if active { t.on_accent } else { t.text },
            );
            if resp.clicked() && !active {
                self.switch_tab();
            }
        }
    }

    fn list(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, t: &Theme, height: f32) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let stride = ROW_H;
        if self.scroll_to_selected && self.view_height > 0.0 {
            let top = self.selected as f32 * stride;
            if top < self.scroll_offset {
                self.scroll_offset = top;
            } else if top + stride > self.scroll_offset + self.view_height {
                self.scroll_offset = top + stride - self.view_height;
            }
        }
        let mut area = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(height)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded);
        if self.scroll_to_selected {
            area = area.vertical_scroll_offset(self.scroll_offset);
            self.scroll_to_selected = false;
        }

        let font = FontId::proportional(14.0);
        let small = FontId::proportional(12.0);
        let cmd = keys::format_mods(self.keys.quick);
        let show_quick = self.keys.quick != Modifiers::NONE && self.settings.show_numbers;
        let mut menu_choice: Option<(usize, MenuChoice)> = None;
        let mut right_clicked = None;
        let mut clicked = None;
        let mut double_clicked = false;

        let out = area.show_rows(ui, ROW_H, self.filtered.len(), |ui, range| {
            for row in range {
                let item = &self.items[self.filtered[row]];
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let selected = row == self.selected;
                let marked = self.marked.contains(&item.clip.id);
                let painter = ui.painter_at(rect);
                if selected {
                    painter.rect_filled(rect, 6.0, t.accent);
                } else if marked {
                    painter.rect_filled(rect, 6.0, t.accent.gamma_multiply(0.35));
                } else if resp.hovered() {
                    painter.rect_filled(rect, 6.0, t.hover);
                }
                let (fg, dim) = if selected { (t.on_accent, t.on_accent) } else { (t.text, t.muted) };

                // Right side: marks and the ⌘N shortcut, like Maccy.
                let mut x = rect.right() - 10.0;
                if row < 9 && show_quick {
                    let g = painter.text(
                        pos2(x, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format!("{cmd}{}", row + 1),
                        small.clone(),
                        dim,
                    );
                    x = g.left() - 8.0;
                }
                for (on, mark) in [(item.clip.pinned, "📌"), (item.clip.saved && self.tab == Tab::History, "★")] {
                    if on {
                        let g = painter.text(
                            pos2(x, rect.center().y),
                            Align2::RIGHT_CENTER,
                            mark,
                            small.clone(),
                            if selected { t.on_accent } else { t.gold },
                        );
                        x = g.left() - 6.0;
                    }
                }

                let mut text_left = rect.left() + 10.0;
                if item.image.is_some() {
                    if let Some(tex) = self.thumb(ctx, item.clip.id) {
                        let box_ = egui::Rect::from_min_size(
                            pos2(text_left, rect.center().y - 10.0),
                            vec2(36.0, 20.0),
                        );
                        let fit = fit_rect(tex.size_vec2(), box_);
                        painter.image(tex.id(), fit, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    text_left += 44.0;
                }
                let text_rect = rect.with_min_x(text_left).with_max_x(x - 4.0);
                let (anchor, align) = if item.rtl {
                    (text_rect.right_center(), Align2::RIGHT_CENTER)
                } else {
                    (text_rect.left_center(), Align2::LEFT_CENTER)
                };
                ui.painter_at(text_rect).text(
                    anchor,
                    align,
                    &item.preview,
                    font.clone(),
                    fg,
                );
                if resp.clicked() {
                    clicked = Some(row);
                }
                if resp.secondary_clicked() {
                    right_clicked = Some(row);
                }
                resp.context_menu(|ui| {
                    if let Some(a) = self.row_menu(ui, item) {
                        menu_choice = Some((row, a));
                        ui.close();
                    }
                });
                if resp.double_clicked() {
                    double_clicked = true;
                }
            }
        });
        self.scroll_offset = out.state.offset.y;
        self.view_height = out.inner_rect.height();
        self.actions_menu_ui(ctx, t, out.inner_rect);

        if let Some(row) = clicked {
            self.selected = row;
        }
        // Right-click selects the row (keeping a multi-selection it's part of).
        if let Some(row) = right_clicked {
            let id = self.items[self.filtered[row]].clip.id;
            if !self.marked.contains(&id) {
                self.marked.clear();
            }
            self.selected = row;
        }
        if let Some((row, choice)) = menu_choice {
            self.selected = row;
            match choice {
                MenuChoice::Key(action) => self.run_action(ctx, action),
                MenuChoice::ClipAction(n) => self.run_clip_action(ctx, n),
            }
        }
        if double_clicked {
            self.choose(ctx, "PASTE");
        }
        if self.filtered.is_empty() {
            let msg = match (self.tab, self.query.is_empty()) {
                (Tab::Saved, true) => "No saved clips yet — select one and press ⌘S / Ctrl+S (Save)",
                (Tab::History, true) => "Copy something to start your history",
                _ => "No matches",
            };
            ui.painter().text(
                out.inner_rect.center(),
                Align2::CENTER_CENTER,
                msg,
                FontId::proportional(13.0),
                t.muted,
            );
        }
    }

    /// Right-click menu for a row; returns the chosen action.
    fn row_menu(&self, ui: &mut egui::Ui, item: &Item) -> Option<MenuChoice> {
        ui.set_min_width(190.0);
        let multi = self.marked.len() > 1 && self.marked.contains(&item.clip.id);
        let n = self.marked.len();
        let entries: Vec<Option<(Action, String)>> = vec![
            Some((Action::Paste, if multi { format!("Paste {n} clips") } else { "Paste".into() })),
            (!item.clip.is_image && !multi).then(|| (Action::Type, "Type out".into())),
            Some((Action::Copy, if multi { format!("Copy {n} clips") } else { "Copy".into() })),
            None,
            (!multi).then(|| (Action::Pin, if item.clip.pinned { "Unpin" } else { "Pin" }.into())),
            (!multi).then(|| (Action::Save, if item.clip.saved { "Unsave" } else { "Save" }.into())),
            Some((Action::Delete, if multi { format!("Delete {n} clips") } else { "Delete".into() })),
            None,
            Some((Action::SelectAll, "Select all".into())),
            (self.db.trash_count() > 0).then(|| (Action::Undo, format!("Undo delete ({})", self.db.trash_count()))),
            Some((Action::Settings, "Settings…".into())),
        ];
        let mut chosen = None;
        if !item.clip.is_image && !self.settings.actions.is_empty() {
            let label = match self.keys.label(Action::Actions) {
                Some(k) => format!("Actions      {k}"),
                None => "Actions".to_owned(),
            };
            ui.menu_button(label, |ui| {
                ui.set_min_width(170.0);
                for (n, action) in self.settings.actions.iter().enumerate() {
                    if ui.button(&action.name).clicked() {
                        chosen = Some(MenuChoice::ClipAction(n));
                    }
                }
            });
            ui.separator();
        }
        for entry in entries {
            match entry {
                None => {
                    ui.separator();
                }
                Some((action, label)) => {
                    let mut button = egui::Button::new(label);
                    if let Some(k) = self.keys.label(action) {
                        button = button.shortcut_text(k);
                    }
                    if ui.add(button).clicked() {
                        chosen = Some(MenuChoice::Key(action));
                    }
                }
            }
        }
        chosen
    }

    /// The preview pane on the right: the whole selected clip, image or text.
    fn preview_pane(&self, ui: &mut egui::Ui, ctx: &egui::Context, t: &Theme) {
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 10.0, t.field);
        ui.painter().rect_stroke(rect, 10.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        let inner = rect.shrink(12.0);
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        let muted = |text: String| egui::RichText::new(text).size(11.5).color(t.muted);

        if self.marked.len() > 1 {
            ui.centered_and_justified(|ui| ui.label(muted(format!("{} clips selected", self.marked.len()))));
            return;
        }
        let Some(&idx) = self.filtered.get(self.selected) else {
            ui.centered_and_justified(|ui| ui.label(muted("Nothing selected".into())));
            return;
        };
        let item = &self.items[idx];
        let ago = time_ago(item.clip.last_used);
        let mut tags = Vec::new();
        if item.clip.pinned {
            tags.push("Pinned");
        }
        if item.clip.saved {
            tags.push("Saved");
        }
        let info_h = 18.0;

        if let Some((w, h)) = item.image {
            let area = egui::Rect::from_min_max(inner.min, pos2(inner.max.x, inner.max.y - info_h - 6.0));
            if let Some(tex) = self.thumb(ctx, item.clip.id) {
                let mut size = tex.size_vec2();
                // Fit the pane; enlarge small images at most 2×.
                let scale = (area.width() / size.x).min(area.height() / size.y).min(2.0);
                size *= scale;
                let r = egui::Rect::from_center_size(area.center(), size);
                ui.painter().image(tex.id(), r, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            let mut info = format!("Image  {w} × {h}  ·  {ago}");
            for tag in &tags {
                info.push_str(&format!("  ·  {tag}"));
            }
            ui.painter().text(pos2(inner.left(), inner.bottom()), Align2::LEFT_BOTTOM, info, FontId::proportional(11.5), t.muted);
            return;
        }

        let text = &item.clip.content;
        let shown: String = text.chars().take(20_000).collect();
        let lines = text.lines().count().max(1);
        let chars = text.chars().count();
        egui::ScrollArea::vertical()
            .id_salt(("preview", item.clip.id))
            .max_height(inner.height() - info_h - 6.0)
            .auto_shrink([false, false])
            .show(&mut ui, |ui| {
                let display: Vec<String> = shown.lines().map(visual_order).collect();
                let job = egui::text::LayoutJob::simple(
                    display.join("\n"),
                    FontId::proportional(13.5),
                    t.text,
                    ui.available_width(),
                );
                ui.label(job);
            });
        let mut info = format!(
            "{chars} character{}  ·  {lines} line{}  ·  {ago}",
            if chars == 1 { "" } else { "s" },
            if lines == 1 { "" } else { "s" }
        );
        for tag in &tags {
            info.push_str(&format!("  ·  {tag}"));
        }
        ui.painter().text(pos2(inner.left(), inner.bottom()), Align2::LEFT_BOTTOM, info, FontId::proportional(11.5), t.muted);
    }

    fn footer(&self, ui: &mut egui::Ui, t: &Theme) {
        let font = FontId::proportional(11.5);
        let max_w = ui.available_width() - 8.0;
        // As many hints as fit, in order of importance.
        let hint = |actions: &[(Action, &str)]| {
            let mut out = String::new();
            for (a, name) in actions {
                let Some(k) = self.keys.label(*a) else { continue };
                let next = if out.is_empty() { format!("{k} {name}") } else { format!("{out}   {k} {name}") };
                if ui.painter().layout_no_wrap(next.clone(), font.clone(), t.muted).size().x > max_w {
                    break;
                }
                out = next;
            }
            out
        };
        let text = match &self.status {
            Some(s) => s.clone(),
            None if !self.marked.is_empty() => format!(
                "{} selected   {}",
                self.marked.len(),
                hint(&[
                    (Action::Delete, "Delete (keeps pinned & saved)"),
                    (Action::Copy, "Copy"),
                    (Action::Paste, "Paste"),
                    (Action::Close, "Cancel"),
                ])
            ),
            None => hint(&[
                (Action::Paste, "Paste"),
                (Action::Type, "Type"),
                (Action::Copy, "Copy"),
                (Action::Save, "Save"),
                (Action::Pin, "Pin"),
                (Action::Delete, "Delete"),
                (Action::NextTab, "Tabs"),
                (Action::Actions, "Actions"),
                (Action::Settings, "Settings"),
            ]),
        };
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
        ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, t.border));
        ui.painter().text(
            rect.left_center() + vec2(4.0, 2.0),
            Align2::LEFT_CENTER,
            text,
            font,
            t.muted,
        );
        if self.offer_undo {
            let label = match self.keys.label(Action::Undo) {
                Some(key) => format!("Undo  {key}"),
                None => "Undo".to_owned(),
            };
            let galley = ui.painter().layout_no_wrap(label, FontId::proportional(12.0), t.accent);
            let r = egui::Rect::from_min_size(
                pos2(rect.right() - galley.size().x - 6.0, rect.center().y + 2.0 - galley.size().y / 2.0),
                galley.size(),
            );
            let resp = ui.interact(r.expand(3.0), ui.id().with("undo"), Sense::click());
            ui.painter().galley(r.min, galley, t.accent);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.undo_requested.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// Development aid: with `CLIPR_SCREENSHOT=/path/shot.bmp`, the window saves an
/// image of itself shortly after it appears.
fn debug_screenshot(ctx: &egui::Context) {
    let Some(path) = std::env::var_os("CLIPR_SCREENSHOT") else { return };
    let frame = ctx.cumulative_pass_nr();
    if frame == 30 {  // after fade-in animations
        ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
    }
    ctx.request_repaint();
    let image = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(img) = image {
        let [w, h] = img.size;
        let mut bmp = Vec::with_capacity(54 + w * h * 4);
        let size = (54 + w * h * 4) as u32;
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&size.to_le_bytes());
        bmp.extend_from_slice(&[0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0]);
        bmp.extend_from_slice(&(w as i32).to_le_bytes());
        bmp.extend_from_slice(&(-(h as i32)).to_le_bytes());
        bmp.extend_from_slice(&[1, 0, 32, 0, 0, 0, 0, 0]);
        bmp.extend_from_slice(&[0; 20]);
        for p in &img.pixels {
            let [r, g, b, a] = p.to_srgba_unmultiplied();
            bmp.extend_from_slice(&[b, g, r, a]);
        }
        let _ = std::fs::write(path, bmp);
    }
}

impl eframe::App for Picker {
    /// Keeps running while the resident window is hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if QUIT_REQUESTED.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        if TOGGLE_REQUESTED.swap(false, Ordering::SeqCst) {
            if self.visible {
                self.close(ctx, true);
            } else {
                self.show(ctx);
            }
        }
    }

    /// Transparent, so our rounded panel gives the window its shape.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if TRANSPARENT {
            [0.0; 4]
        } else {
            Theme::new(visuals.dark_mode).bg.to_opaque().to_normalized_gamma_f32()
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // macOS: close when the user clicks away, like Maccy. Not on Linux:
        // Hyprland moves focus with the mouse, so merely moving the pointer
        // off the popup would close it (Esc / the hotkey close it instead).
        match ctx.input(|i| i.viewport().focused) {
            Some(true) => self.was_focused = true,
            Some(false) if self.was_focused && self.settings.close_on_click_away => self.close(&ctx, false),
            _ => {}
        }

        self.handle_keys(&ctx);
        debug_screenshot(&ctx);

        let t = Theme::new(ui.visuals().dark_mode);
        egui::Frame::new()
            .fill(t.bg)
            .stroke(Stroke::new(1.0, t.border))
            .corner_radius(CORNER)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                if self.view == View::Settings {
                    self.settings_page(ui, &ctx, &t);
                    return;
                }
                let full = ui.available_rect_before_wrap();
                let pane = self.settings.show_preview && full.width() > LIST_W;
                let column = if pane { full.with_max_x(full.left() + LIST_W - 20.0) } else { full };
                let mut left = ui.new_child(egui::UiBuilder::new().max_rect(column).layout(egui::Layout::top_down(egui::Align::Min)));
                self.header(&mut left, &t);
                left.add_space(8.0);
                let footer_h = if self.settings.show_footer || self.status.is_some() { 26.0 } else { 0.0 };
                let list_h = left.available_height() - footer_h;
                self.list(&mut left, &ctx, &t, list_h);
                if footer_h > 0.0 {
                    left.add_space(4.0);
                    self.footer(&mut left, &t);
                }
                if pane {
                    let rect = full.with_min_x(column.right() + 10.0);
                    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
                    self.preview_pane(&mut right, &ctx, &t);
                }
                // Last, so it's drawn over the list.
                if self.confirm_delete {
                    self.confirm_dialog(&ctx, &t);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hebrew_is_reordered_for_display() {
        // Single words are left alone (the shaper handles them)...
        assert_eq!(super::visual_order("רפאל"), "רפאל");
        // ...but word order is flipped for display.
        assert_eq!(super::visual_order("שלום עולם - Hebrew test"), "Hebrew test - עולם שלום");
        assert_eq!(super::visual_order("plain"), "plain");
    }
}
