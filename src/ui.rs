//! The picker popup: fuzzy search over history / saved clips, fully keyboard driven.

use crate::db::{Clip, Db};
use crate::ipc;
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
const WINDOW: [f32; 2] = [480.0, 440.0];
const CORNER: f32 = 12.0;
const PREVIEW_CHARS: usize = 160;
/// Only the start of very long clips is searched, to keep typing instant.
const SEARCH_CHARS: usize = 4096;

/// Preferred UI font (the macOS system font); egui's default is used otherwise.
const UI_FONTS: &[&str] = &["/System/Library/Fonts/SFNS.ttf"];

/// Fonts tried (in order) as a fallback for Hebrew, Arabic, Cyrillic, etc.
const FALLBACK_FONTS: &[&str] = &[
    "/System/Library/Fonts/SFHebrew.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
];

/// `OneShot`: a short-lived process that exits after picking (Linux).
/// `Resident`: lives inside the daemon and is hidden/shown (macOS, where a
/// freshly spawned process is not allowed to take focus).
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    OneShot,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Resident,
}

static TOGGLE_REQUESTED: AtomicBool = AtomicBool::new(false);
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// Show/hide the resident picker. Safe to call from any thread.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn request_toggle() {
    TOGGLE_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(ctx) = CONTEXT.get() {
        ctx.request_repaint();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    History,
    Saved,
}

struct Item {
    clip: Clip,
    preview: String,
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
}

pub fn run(mode: Mode) -> Result<()> {
    let db = Db::open()?;
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("clipr")
            .with_app_id("clipr")
            .with_inner_size(WINDOW)
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
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
            picker.reload();
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
                bg: rgba(36, 36, 38, 250),
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
                bg: rgba(250, 250, 252, 252),
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
            status: None,
            was_focused: false,
            scroll_to_selected: true,
            scroll_offset: 0.0,
            view_height: 0.0,
        }
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
                        preview: preview(&clip.content),
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
        self.reload();
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.query.clear();
        self.selected = 0;
        self.status = None;
        self.was_focused = false;
        self.reload();
        self.visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        #[cfg(target_os = "macos")]
        crate::platform::activate_self();
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
        let Some(id) = self.current().map(|c| c.id) else { return };
        match ipc::send(&format!("{action} {id}")) {
            Ok(()) => self.close(ctx, false),
            Err(_) => self.status = Some("clipr daemon is not running — start it with `clipr`".into()),
        }
    }

    fn edit_current(&mut self, f: impl FnOnce(&Db, &Clip) -> Result<()>) {
        if let Some(clip) = self.current().cloned() {
            if let Err(e) = f(&self.db, &clip) {
                self.status = Some(format!("database error: {e}"));
            }
            self.reload();
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let mut action = None;
        let mut quick = None;
        let mut edit = None;
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                action = Some("CLOSE");
            }
            for (key, delta) in [
                (Key::ArrowDown, 1),
                (Key::ArrowUp, -1),
                (Key::PageDown, 10),
                (Key::PageUp, -10),
            ] {
                if i.consume_key(Modifiers::NONE, key) {
                    self.move_selection(delta);
                }
            }
            if i.consume_key(Modifiers::SHIFT, Key::Tab) || i.consume_key(Modifiers::NONE, Key::Tab) {
                action = Some("TAB");
            }
            // Order matters: modified Enter first.
            if i.consume_key(cmd, Key::Enter) {
                action = Some("TYPE");
            } else if i.consume_key(Modifiers::SHIFT, Key::Enter) {
                action = Some("COPY");
            } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                action = Some("PASTE");
            }
            if i.consume_key(cmd, Key::P) {
                edit = Some('p');
            } else if i.consume_key(cmd, Key::S) {
                edit = Some('s');
            } else if i.consume_key(cmd, Key::D) {
                edit = Some('d');
            }
            let digits = [
                Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5,
                Key::Num6, Key::Num7, Key::Num8, Key::Num9,
            ];
            for (n, key) in digits.into_iter().enumerate() {
                if i.consume_key(cmd, key) {
                    quick = Some(n);
                }
            }
        });

        if let Some(n) = quick {
            if n < self.filtered.len() {
                self.selected = n;
                action = Some("PASTE");
            }
        }
        match edit {
            Some('p') => self.edit_current(|db, c| db.set_pinned(c.id, !c.pinned)),
            Some('s') => self.edit_current(|db, c| db.set_saved(c.id, !c.saved)),
            Some('d') => self.edit_current(|db, c| db.delete(c.id)),
            _ => {}
        }
        match action {
            Some("CLOSE") => self.close(ctx, true),
            Some("TAB") => self.switch_tab(),
            Some(a) => self.choose(ctx, a),
            None => {}
        }
    }

    /// Search field with a magnifier glyph, plus a History/Saved segmented control.
    fn header(&mut self, ui: &mut egui::Ui, t: &Theme) {
        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
        let seg_w = 150.0;
        let field = row.with_max_x(row.right() - seg_w - 8.0);
        let seg = row.with_min_x(row.right() - seg_w);

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
        let cmd = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };
        let mut clicked = None;
        let mut double_clicked = false;

        let out = area.show_rows(ui, ROW_H, self.filtered.len(), |ui, range| {
            for row in range {
                let item = &self.items[self.filtered[row]];
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let selected = row == self.selected;
                let painter = ui.painter_at(rect);
                if selected {
                    painter.rect_filled(rect, 6.0, t.accent);
                } else if resp.hovered() {
                    painter.rect_filled(rect, 6.0, t.hover);
                }
                let (fg, dim) = if selected { (t.on_accent, t.on_accent) } else { (t.text, t.muted) };

                // Right side: marks and the ⌘N shortcut, like Maccy.
                let mut x = rect.right() - 10.0;
                if row < 9 {
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

                let text_rect = rect.with_min_x(rect.left() + 10.0).with_max_x(x - 4.0);
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
                if resp.double_clicked() {
                    double_clicked = true;
                }
            }
        });
        self.scroll_offset = out.state.offset.y;
        self.view_height = out.inner_rect.height();

        if let Some(row) = clicked {
            self.selected = row;
        }
        if double_clicked {
            self.choose(ctx, "PASTE");
        }
        if self.filtered.is_empty() {
            let msg = match (self.tab, self.query.is_empty()) {
                (Tab::Saved, true) => "No saved clips yet — select one and press ⌘S / Ctrl+S",
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

    fn footer(&self, ui: &mut egui::Ui, t: &Theme) {
        let text = match &self.status {
            Some(s) => s.clone(),
            None if cfg!(target_os = "macos") => {
                "↩ Paste    ⌘↩ Type    ⇧↩ Copy    ⌘S Save    ⌘P Pin    ⌘D Delete    Tab Switch".into()
            }
            None => "Enter Paste   Ctrl+Enter Type   Shift+Enter Copy   Ctrl+S Save   Ctrl+P Pin   Ctrl+D Delete".into(),
        };
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
        ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, t.border));
        ui.painter().text(
            rect.left_center() + vec2(4.0, 2.0),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(11.5),
            t.muted,
        );
    }
}

/// Development aid: with `CLIPR_SCREENSHOT=/path/shot.bmp`, the window saves an
/// image of itself shortly after it appears.
fn debug_screenshot(ctx: &egui::Context) {
    let Some(path) = std::env::var_os("CLIPR_SCREENSHOT") else { return };
    let frame = ctx.cumulative_pass_nr();
    if frame == 5 {
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
        if TOGGLE_REQUESTED.swap(false, Ordering::SeqCst) {
            if self.visible {
                self.close(ctx, true);
            } else {
                self.show(ctx);
            }
        }
    }

    /// Transparent, so our rounded panel gives the window its shape.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Close when the user clicks away.
        match ctx.input(|i| i.viewport().focused) {
            Some(true) => self.was_focused = true,
            Some(false) if self.was_focused => self.close(&ctx, false),
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
                self.header(ui, &t);
                ui.add_space(8.0);
                let list_h = ui.available_height() - 26.0;
                self.list(ui, &ctx, &t, list_h);
                ui.add_space(4.0);
                self.footer(ui, &t);
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
