use std::collections::HashSet;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontData, FontFamily, FontId, Key, Modifiers, Rect, RichText, Sense, Stroke,
    TextStyle,
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
    vec2,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::backend::{Cmd, Ev};
use crate::model::*;

const ROW_H: f32 = 24.0;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
const ARTIST_VIEW: &str = "artist";

// Terminal palette.
const BG: Color32 = Color32::from_rgb(10, 13, 10);
const BG_INPUT: Color32 = Color32::from_rgb(6, 8, 6);
const FG: Color32 = Color32::from_rgb(201, 209, 195);
const DIM: Color32 = Color32::from_rgb(110, 122, 107);
const ACCENT: Color32 = Color32::from_rgb(57, 211, 83);
const HOVER: Color32 = Color32::from_rgb(20, 27, 20);
const SELECT: Color32 = Color32::from_rgb(22, 48, 28);
const BORDER: Color32 = Color32::from_rgb(34, 51, 34);

const BANNER: &str = r"  ____  ____                 _
 / ___|/ ___|___ _ __   ___ | |_
| |  _| |  _/ __| '_ \ / _ \| __|
| |_| | |_| \__ \ |_) | (_) | |_
 \____|\____|___/ .__/ \___/ \__|
                |_|";

fn add_font(ctx: &egui::Context, name: &str, path: &str, priority: FontPriority) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    let mut data = FontData::from_owned(bytes);
    // CJK fonts have tall ascenders; nudge them onto the mono font's baseline.
    if matches!(name, "jp" | "kr") {
        data.tweak.y_offset_factor = 0.32;
    }
    ctx.add_font(FontInsert {
        name: name.into(),
        data,
        families: vec![
            InsertFontFamily { family: FontFamily::Proportional, priority },
            InsertFontFamily { family: FontFamily::Monospace, priority },
        ],
    });
    true
}

/// Monospace everywhere (JetBrains Mono Nerd Font if installed, else Cascadia
/// Mono / Consolas), with Thai as a fallback. Japanese/Korean fonts are ~13 MB
/// each, so they only load (off-thread) once such text shows up.
pub fn install_style(ctx: &egui::Context) {
    let user_fonts = dirs::data_local_dir().map(|d| d.join(r"Microsoft\Windows\Fonts")).unwrap_or_default();
    let mono = [
        user_fonts.join("JetBrainsMonoNerdFontMono-Regular.ttf").to_string_lossy().into_owned(),
        r"C:\Windows\Fonts\CascadiaMono.ttf".into(),
        r"C:\Windows\Fonts\consola.ttf".into(),
    ];
    // Only our fonts: egui's bundled emoji fonts contain stray CJK glyphs that
    // would win over the Japanese font and render misaligned.
    if let Some(bytes) = mono.iter().find_map(|p| std::fs::read(p).ok()) {
        let mut defs = egui::FontDefinitions::empty();
        defs.font_data.insert("mono".into(), std::sync::Arc::new(FontData::from_owned(bytes)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families.insert(family, vec!["mono".into()]);
        }
        ctx.set_fonts(defs);
    }
    add_font(ctx, "thai", r"C:\Windows\Fonts\LeelawUI.ttf", FontPriority::Lowest);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, 12.0),
            (TextStyle::Body, 14.0),
            (TextStyle::Button, 14.0),
            (TextStyle::Monospace, 14.0),
            (TextStyle::Heading, 16.0),
        ]
        .into_iter()
        .map(|(s, size)| (s, FontId::monospace(size)))
        .collect();
        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = BG;
        v.extreme_bg_color = BG_INPUT;
        v.faint_bg_color = BG;
        v.override_text_color = Some(FG);
        v.selection.bg_fill = SELECT;
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = 0.into();
            w.bg_fill = BG;
            w.weak_bg_fill = BG;
            w.bg_stroke = Stroke::new(1.0, BORDER);
            w.fg_stroke = Stroke::new(1.0, FG);
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.hovered.weak_bg_fill = HOVER;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    });
}

#[derive(Default)]
struct ScriptFonts {
    jp: bool,
    kr: bool,
}

impl ScriptFonts {
    /// `now`: load on this thread (first frame), so no glyph is ever drawn
    /// with a system fallback font first.
    fn scan<'a>(&mut self, ctx: &egui::Context, texts: impl Iterator<Item = &'a str>, now: bool) {
        if self.jp && self.kr {
            return;
        }
        let (mut jp, mut kr) = (false, false);
        for c in texts.flat_map(str::chars) {
            match c as u32 {
                0x3040..=0x30FF | 0x4E00..=0x9FFF | 0xFF66..=0xFF9F => jp = true,
                0xAC00..=0xD7AF | 0x1100..=0x11FF => kr = true,
                _ => {}
            }
        }
        let load = |name: &'static str, path: &'static str| {
            if now {
                add_font(ctx, name, path, FontPriority::Lowest);
                return;
            }
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                add_font(&ctx, name, path, FontPriority::Lowest);
                ctx.request_repaint();
            });
        };
        if jp && !self.jp {
            self.jp = true;
            load("jp", r"C:\Windows\Fonts\YuGothR.ttc");
        }
        if kr && !self.kr {
            self.kr = true;
            load("kr", r"C:\Windows\Fonts\malgun.ttf");
        }
    }
}

fn track_texts(t: &[Track]) -> impl Iterator<Item = &str> {
    t.iter().flat_map(|t| [t.name.as_str(), t.artists.as_str()])
}

/// "[=========>---------]" style bar; returns the clicked/dragged fraction.
fn ascii_bar(ui: &mut egui::Ui, id: &str, frac: f32, width_chars: usize) -> Option<f32> {
    let font = FontId::monospace(14.0);
    let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '='));
    let n = width_chars.max(4);
    let (rect, resp) = ui.allocate_exact_size(vec2(cw * (n + 2) as f32, ROW_H), Sense::click_and_drag());
    let filled = ((frac.clamp(0.0, 1.0)) * n as f32).round() as usize;
    let head = if filled > 0 && filled < n { ">" } else { "" };
    let done = "=".repeat(filled.saturating_sub(head.len()));
    let rest = "-".repeat(n - filled);
    let p = ui.painter();
    let y = rect.center().y;
    let mut x = rect.left();
    for (s, c) in [("[", DIM), (done.as_str(), ACCENT), (head, ACCENT), (rest.as_str(), DIM), ("]", DIM)] {
        p.text(egui::pos2(x, y), Align2::LEFT_CENTER, s, font.clone(), c);
        x += cw * s.chars().count() as f32;
    }
    let _ = id;
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() || resp.dragged() {
        let px = resp.interact_pointer_pos()?.x;
        return Some(((px - rect.left() - cw) / (cw * n as f32)).clamp(0.0, 1.0));
    }
    None
}

fn text_button(ui: &mut egui::Ui, label: &str) -> bool {
    let r = ui.add(egui::Button::new(RichText::new(label).color(ACCENT)).frame(false));
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r.clicked()
}

struct ArtistPage {
    uri: String,
    name: String,
    tracks: Vec<Track>,
    back_to: String,
}

pub struct App {
    lib: Library,
    view: String,
    query: String,
    query_edited: Option<Instant>,
    sent_query: String,
    local_index: Option<Vec<(String, Track)>>,
    local_hits: Vec<Track>,
    remote: (String, Vec<Track>),
    artist: Option<ArtistPage>,
    now: Option<Track>,
    playing: bool,
    pos: (u32, Instant),
    seek: Option<f32>,
    volume: f32,
    status: String,
    loading: Option<String>,
    need_login: bool,
    logging_in: bool,
    selected: Option<usize>,
    fonts: ScriptFonts,
    fonts_scanned: bool,
    cmd: UnboundedSender<Cmd>,
    ev: Receiver<Ev>,
}

impl App {
    pub fn new(lib: Library, cfg: &Config, cmd: UnboundedSender<Cmd>, ev: Receiver<Ev>) -> Self {
        Self {
            view: cfg.last_view.clone().filter(|v| v != ARTIST_VIEW).unwrap_or_else(|| LIKED.into()),
            lib,
            query: String::new(),
            query_edited: None,
            sent_query: String::new(),
            local_index: None,
            local_hits: Vec::new(),
            remote: (String::new(), Vec::new()),
            artist: None,
            now: None,
            playing: false,
            pos: (0, Instant::now()),
            seek: None,
            volume: cfg.volume.unwrap_or(40000) as f32 / 65535.0,
            status: String::new(),
            loading: None,
            need_login: false,
            logging_in: false,
            selected: None,
            fonts: ScriptFonts::default(),
            fonts_scanned: false,
            cmd,
            ev,
        }
    }

    fn send(&self, c: Cmd) {
        let _ = self.cmd.send(c);
    }

    fn open(&mut self, id: String) {
        self.view = id.clone();
        self.selected = None;
        self.query.clear();
        self.send(Cmd::View(id.clone()));
        self.send(Cmd::Open(id));
    }

    fn open_artist(&mut self, t: &Track) {
        let Some(uri) = t.artist_uris.first().cloned() else {
            self.status = "artist link not loaded yet".into();
            return;
        };
        let name = t.artists.split(", ").next().unwrap_or("").to_string();
        let back_to = if self.view == ARTIST_VIEW {
            self.artist.as_ref().map_or(LIKED.into(), |a| a.back_to.clone())
        } else {
            self.view.clone()
        };
        self.artist = Some(ArtistPage { uri: uri.clone(), name: name.clone(), tracks: Vec::new(), back_to });
        self.view = ARTIST_VIEW.into();
        self.selected = None;
        self.query.clear();
        self.loading = Some(ARTIST_VIEW.into());
        self.send(Cmd::Artist(uri));
    }

    fn back(&mut self) {
        if let Some(a) = self.artist.take() {
            self.view = a.back_to;
            self.selected = None;
            self.loading = None;
        }
    }

    fn current_tracks(&self) -> &[Track] {
        if self.view == ARTIST_VIEW {
            self.artist.as_ref().map_or(&[], |a| a.tracks.as_slice())
        } else {
            self.lib.tracks(&self.view)
        }
    }

    fn position(&self) -> u32 {
        let p = if self.playing { self.pos.0 + self.pos.1.elapsed().as_millis() as u32 } else { self.pos.0 };
        self.now.as_ref().map_or(p, |t| p.min(t.ms))
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.ev.try_recv() {
            match ev {
                Ev::NeedLogin => {
                    self.need_login = true;
                    self.logging_in = false;
                }
                Ev::LoggedIn => {
                    self.need_login = false;
                    self.logging_in = false;
                }
                Ev::Status(s) => self.status = s,
                Ev::Playlists(p) => {
                    self.fonts.scan(ctx, p.iter().map(|p| p.name.as_str()), false);
                    self.lib.playlists = p;
                }
                Ev::Liked(t) => {
                    self.fonts.scan(ctx, track_texts(&t), false);
                    self.lib.liked = t;
                    self.local_index = None;
                }
                Ev::Contents(id, snap, t) => {
                    self.fonts.scan(ctx, track_texts(&t), false);
                    if self.loading.as_deref() == Some(id.as_str()) {
                        self.loading = None;
                    }
                    self.lib.contents.insert(id, (snap, t));
                    self.local_index = None;
                }
                Ev::Loading(id) => self.loading = Some(id),
                Ev::SearchResults(q, r) => {
                    self.fonts.scan(ctx, track_texts(&r), false);
                    self.remote = (q, r);
                }
                Ev::Artist(uri, r) => {
                    self.fonts.scan(ctx, track_texts(&r), false);
                    if let Some(a) = self.artist.as_mut().filter(|a| a.uri == uri) {
                        a.tracks = r;
                        if self.loading.as_deref() == Some(ARTIST_VIEW) {
                            self.loading = None;
                        }
                    }
                }
                Ev::NowPlaying(t) => {
                    self.now = Some(t);
                    self.pos = (0, Instant::now());
                }
                Ev::State { playing, pos_ms } => {
                    self.playing = playing;
                    self.pos = (pos_ms, Instant::now());
                }
            }
        }
    }

    fn update_local_search(&mut self) {
        let index = self.local_index.get_or_insert_with(|| {
            let mut seen = HashSet::new();
            self.lib
                .liked
                .iter()
                .chain(self.lib.contents.values().flat_map(|c| c.1.iter()))
                .filter(|t| seen.insert(t.uri.clone()))
                .map(|t| (format!("{} {} {}", t.name, t.artists, t.album).to_lowercase(), t.clone()))
                .collect()
        });
        let words: Vec<String> = self.query.to_lowercase().split_whitespace().map(String::from).collect();
        self.local_hits = index
            .iter()
            .filter(|(hay, _)| words.iter().all(|w| hay.contains(w.as_str())))
            .take(50)
            .map(|(_, t)| t.clone())
            .collect();
    }

    fn login_screen(&mut self, ui: &mut egui::Ui) {
        ui.add_space((ui.available_height() * 0.18).max(8.0));
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(BANNER).color(ACCENT));
            ui.add_space(12.0);
            ui.label(RichText::new("a tiny spotify player for your terminal-loving soul").color(DIM));
            ui.add_space(20.0);
            let label = if self.logging_in { "[ waiting for browser... ]" } else { "[ login with spotify ]" };
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            let r = ui.add_enabled(
                !self.logging_in,
                egui::Button::new(RichText::new(label).size(16.0).color(ACCENT))
                    .stroke(Stroke::new(1.0, ACCENT))
                    .min_size(vec2(260.0, 36.0)),
            );
            if (r.clicked() || (enter && !self.logging_in)) && !self.logging_in {
                self.logging_in = true;
                self.send(Cmd::SignIn);
            }
            ui.add_space(12.0);
            ui.label(RichText::new("opens the normal Spotify login page in your browser").color(DIM));
            ui.label(RichText::new("(email, Google, Apple). Premium is required for playback.").color(DIM));
        });
    }

    /// Virtualised track list: only visible rows are laid out.
    fn track_list(&mut self, ui: &mut egui::Ui, id: &str, tracks: &[Track], max_h: Option<f32>) {
        let now_uri = self.now.as_ref().map(|t| t.uri.clone());
        let mut play = None;
        let mut artist = None;
        let mut area = egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, max_h.is_some()]);
        if let Some(h) = max_h {
            area = area.max_height(h);
        }
        let font = FontId::monospace(14.0);
        area.show_rows(ui, ROW_H, tracks.len(), |ui, range| {
            for i in range {
                let t = &tracks[i];
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let is_now = now_uri.as_deref() == Some(t.uri.as_str());
                let is_sel = id == "main" && self.selected == Some(i);
                let bg = if is_sel {
                    SELECT
                } else if resp.hovered() {
                    HOVER
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, 0.0, bg);
                let w = rect.width();
                let col = |x0: f32, x1: f32| Rect::from_x_y_ranges(rect.left() + x0..=rect.left() + x1, rect.y_range());
                let (c0, c1, c2) = (col(6.0, 50.0), col(54.0, w * 0.55), col(w * 0.57, w - 64.0));
                let p = ui.painter();
                let marker = if is_now { "▶".to_string() } else { format!("{:>3}", i + 1) };
                p.with_clip_rect(c0).text(c0.left_center(), Align2::LEFT_CENTER, marker, font.clone(), if is_now { ACCENT } else { DIM });
                p.with_clip_rect(c1).text(c1.left_center(), Align2::LEFT_CENTER, &t.name, font.clone(), if is_now { ACCENT } else { FG });
                p.text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, fmt_ms(t.ms), font.clone(), DIM);

                // Artist name: clickable, opens the artist page.
                let galley = ui.ctx().fonts_mut(|f| f.layout_no_wrap(t.artists.clone(), font.clone(), DIM));
                let hit = Rect::from_min_size(
                    egui::pos2(c2.left(), rect.center().y - galley.size().y / 2.0),
                    vec2(galley.size().x.min(c2.width()), galley.size().y),
                );
                let ar = ui.interact(hit, ui.scope_id().with((id, i, "artist")), Sense::click());
                let color = if ar.hovered() { ACCENT } else { DIM };
                let p = ui.painter().with_clip_rect(c2);
                p.text(c2.left_center(), Align2::LEFT_CENTER, &t.artists, font.clone(), color);
                if ar.hovered() {
                    p.line_segment([hit.left_bottom(), hit.right_bottom()], Stroke::new(1.0, ACCENT));
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if ar.clicked() {
                    artist = Some(t.clone());
                } else if resp.clicked() && id == "main" {
                    self.selected = Some(i);
                }
                if resp.double_clicked() {
                    play = Some(i);
                }
            }
        });
        if let Some(i) = play {
            self.send(Cmd::Play(tracks.to_vec(), i));
        }
        if let Some(t) = artist {
            self.open_artist(&t);
        }
    }

    fn player_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let mut open_artist = None;
        ui.horizontal(|ui| {
            if text_button(ui, "[|<]") {
                self.send(Cmd::Prev);
            }
            if text_button(ui, if self.playing { "[ || ]" } else { "[ >  ]" }) {
                self.send(Cmd::Toggle);
            }
            if text_button(ui, "[>|]") {
                self.send(Cmd::Next);
            }
            ui.add_space(10.0);
            match &self.now {
                Some(t) => {
                    ui.label(RichText::new(if self.playing { "▶" } else { "‖" }).color(ACCENT));
                    ui.add(egui::Label::new(RichText::new(&t.name).color(FG)).truncate());
                    ui.label(RichText::new("--").color(DIM));
                    let r = ui.add(egui::Label::new(RichText::new(&t.artists).color(DIM)).truncate().sense(Sense::click()));
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if r.clicked() {
                        open_artist = Some(t.clone());
                    }
                }
                None => {
                    ui.label(RichText::new("-- nothing playing --").color(DIM));
                }
            }
        });
        ui.horizontal(|ui| {
            let dur = self.now.as_ref().map_or(0, |t| t.ms);
            let shown = self.seek.map_or(self.position(), |f| (f * dur as f32) as u32);
            ui.label(RichText::new(fmt_ms(shown)).color(DIM));
            let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&FontId::monospace(14.0), '='));
            let chars = ((ui.available_width() - 260.0) / cw).max(10.0) as usize;
            let frac = if dur == 0 { 0.0 } else { shown as f32 / dur as f32 };
            let dragging = ui.input(|i| i.pointer.primary_down());
            match ascii_bar(ui, "progress", frac, chars) {
                Some(f) if dragging => self.seek = Some(f),
                Some(f) => {
                    let ms = (f * dur as f32) as u32;
                    self.send(Cmd::Seek(ms));
                    self.pos = (ms, Instant::now());
                    self.seek = None;
                }
                None => {
                    if let Some(f) = self.seek.take().filter(|_| !dragging) {
                        let ms = (f * dur as f32) as u32;
                        self.send(Cmd::Seek(ms));
                        self.pos = (ms, Instant::now());
                    }
                }
            }
            ui.label(RichText::new(fmt_ms(dur)).color(DIM));
            ui.add_space(16.0);
            ui.label(RichText::new("vol").color(DIM));
            if let Some(v) = ascii_bar(ui, "volume", self.volume, 10) {
                self.volume = v;
                self.send(Cmd::Volume((v * 65535.0) as u16));
            }
        });
        ui.add_space(2.0);
        if let Some(t) = open_artist {
            self.open_artist(&t);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.fonts_scanned {
            self.fonts_scanned = true;
            let lib = &self.lib;
            self.fonts.scan(
                &ctx,
                lib.playlists.iter().map(|p| p.name.as_str())
                    .chain(track_texts(&lib.liked))
                    .chain(lib.contents.values().flat_map(|c| track_texts(&c.1))),
                true,
            );
        }
        self.drain_events(&ctx);

        // Keys: Space play/pause, Ctrl+F search, Esc clear/back, Ctrl+Left/Right prev/next,
        // Up/Down + Enter play.
        let typing = ctx.memory(|m| m.focused().is_some());
        let search_id = egui::Id::unique("search");
        let mut go_back = false;
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::F) {
                ctx.memory_mut(|m| m.request_focus(search_id));
            }
            if i.consume_key(Modifiers::COMMAND, Key::ArrowRight) {
                let _ = self.cmd.send(Cmd::Next);
            }
            if i.consume_key(Modifiers::COMMAND, Key::ArrowLeft) {
                let _ = self.cmd.send(Cmd::Prev);
            }
            if !typing && i.consume_key(Modifiers::NONE, Key::Space) {
                let _ = self.cmd.send(Cmd::Toggle);
            }
            if !typing && (i.consume_key(Modifiers::NONE, Key::Escape) || i.consume_key(Modifiers::NONE, Key::Backspace)) {
                go_back = true;
            }
        });
        if go_back {
            self.back();
        }
        if !typing && self.query.is_empty() && !self.need_login {
            let n = self.current_tracks().len();
            let (down, up, enter) = ctx.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter)));
            if n > 0 && down {
                self.selected = Some(self.selected.map_or(0, |s| (s + 1).min(n - 1)));
            }
            if n > 0 && up {
                self.selected = Some(self.selected.map_or(0, |s| s.saturating_sub(1)));
            }
            if enter && let Some(s) = self.selected {
                self.send(Cmd::Play(self.current_tracks().to_vec(), s));
            }
        }

        let line = egui::Frame::NONE.fill(BG).inner_margin(egui::Margin::symmetric(10, 6));
        egui::Panel::top("top").frame(line.stroke(Stroke::new(1.0, BORDER))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ggspot:~$").color(ACCENT));
                ui.label(RichText::new("search").color(DIM));
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .id(search_id)
                        .frame(egui::Frame::NONE)
                        .hint_text(RichText::new("type a song or artist...  (ctrl+f)").color(DIM))
                        .desired_width((ui.available_width() - 260.0).max(160.0)),
                );
                if r.changed() {
                    self.fonts.scan(&ctx, std::iter::once(self.query.as_str()), false);
                    self.query_edited = Some(Instant::now());
                    self.update_local_search();
                }
                if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.query.clear();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&self.status).color(DIM)).truncate());
                });
            });
        });

        egui::Panel::bottom("player").frame(line.stroke(Stroke::new(1.0, BORDER))).show(ui, |ui| self.player_bar(ui));

        if !self.need_login {
            egui::Panel::left("sidebar")
                .frame(line)
                .resizable(true)
                .default_size(220.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.label(RichText::new("## library").color(DIM));
                        let mut open = None;
                        let in_list = self.query.is_empty() && self.view != ARTIST_VIEW;
                        let mut entry = |ui: &mut egui::Ui, id: &str, name: &str| {
                            let on = in_list && self.view == id;
                            let text = RichText::new(format!("{} {name}", if on { ">" } else { " " }))
                                .color(if on { ACCENT } else { FG });
                            let r = ui.add(egui::Label::new(text).truncate().sense(Sense::click()));
                            if r.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            if r.clicked() {
                                open = Some(id.to_string());
                            }
                        };
                        entry(ui, LIKED, "liked songs");
                        ui.add_space(6.0);
                        ui.label(RichText::new("## playlists").color(DIM));
                        for p in &self.lib.playlists {
                            entry(ui, &p.id, &p.name);
                        }
                        if let Some(id) = open {
                            self.artist = None;
                            self.open(id);
                        }
                    });
                });
        }

        egui::CentralPanel::default_margins().frame(line).show(ui, |ui| {
            if self.need_login {
                self.login_screen(ui);
            } else if !self.query.trim().is_empty() {
                let local = self.local_hits.clone();
                ui.label(RichText::new(format!("-- in your library ({}) --", local.len())).color(DIM));
                self.track_list(ui, "local", &local, Some((ui.available_height() * 0.4).max(ROW_H * 3.0)));
                ui.add_space(6.0);
                let remote = self.remote.1.clone();
                let fresh = self.remote.0 == self.query.trim();
                let head = if fresh || remote.is_empty() && self.query_edited.is_none() {
                    format!("-- spotify ({}) --", remote.len())
                } else {
                    "-- spotify (searching...) --".into()
                };
                ui.label(RichText::new(head).color(DIM));
                self.track_list(ui, "remote", &remote, None);
            } else {
                let tracks = self.current_tracks().to_vec();
                ui.horizontal(|ui| {
                    if self.view == ARTIST_VIEW {
                        if text_button(ui, "[<- back]") {
                            self.back();
                        }
                        let name = self.artist.as_ref().map(|a| a.name.clone()).unwrap_or_default();
                        ui.label(RichText::new(format!("# artist: {name}")).heading().color(ACCENT));
                    } else {
                        let title = if self.view == LIKED {
                            "liked songs".to_string()
                        } else {
                            self.lib.playlists.iter().find(|p| p.id == self.view).map(|p| p.name.clone()).unwrap_or_default()
                        };
                        ui.label(RichText::new(format!("# {title}")).heading().color(ACCENT));
                    }
                    ui.label(RichText::new(format!("({} tracks)", tracks.len())).color(DIM));
                    if self.loading.as_deref() == Some(self.view.as_str()) {
                        ui.label(RichText::new("[loading...]").color(DIM));
                    }
                });
                ui.add_space(4.0);
                self.track_list(ui, "main", &tracks, None);
            }
        });

        // Debounced remote search.
        if let Some(t) = self.query_edited {
            let q = self.query.trim().to_string();
            if t.elapsed() >= SEARCH_DEBOUNCE {
                self.query_edited = None;
                if q.chars().count() >= 2 && q != self.sent_query {
                    self.sent_query = q.clone();
                    self.send(Cmd::Search(q));
                }
            } else {
                ctx.request_repaint_after(SEARCH_DEBOUNCE - t.elapsed());
            }
        }
        if self.playing {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}
