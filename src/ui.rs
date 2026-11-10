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
use crate::command::{self, Action, Suggestion};
use crate::cover;
use crate::disc;
use crate::model::*;
use crate::order::{PlaylistOrder, SortMode};
use crate::queue::Repeat;

const ROW_H: f32 = 24.0;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
const ARTIST_VIEW: &str = "artist";
const QUEUE_VIEW: &str = "queue";

pub const FULL_SIZE: [f32; 2] = [880.0, 580.0];
pub const FULL_MIN: [f32; 2] = [520.0, 320.0];
const MINI_SIZE: [f32; 2] = [340.0, 484.0];
const MINI_MIN: [f32; 2] = [310.0, 340.0];
const ASCII_FONT: f32 = 12.0;
const ASCII_LINE: f32 = 14.0;
const TITLE_H: f32 = 22.0;
const POPUP_ROWS: usize = 10;
const POPUP_ROW_H: f32 = 22.0;
/// Shown under /help, beside the commands.
const KEYS: &[(&str, &str)] = &[
    ("ctrl+g", "command line"),
    ("ctrl+f", "command line, ready to search"),
    ("space", "play / pause"),
    ("ctrl+left / right", "previous / next"),
    ("ctrl+s", "shuffle"),
    ("ctrl+r", "repeat"),
    ("ctrl+o", "sort playlists"),
    ("alt+up / down", "move the open playlist"),
    ("ctrl+i", "album cover"),
    ("ctrl+m", "mini mode"),
    ("up / down, enter", "select a song, play it"),
    ("esc", "clear the command line, go back"),
];
/// Pointing at a song this long shows its cover. Passing over one fetches nothing.
const HOVER_DELAY: Duration = Duration::from_millis(250);
const HOVER_COLS: usize = 44;
const HOVER_FONT: f32 = 10.0;
const HOVER_LINE: f32 = 11.0;
/// How close to the window's edge the pointer can resize it.
const EDGE: f32 = 5.0;
const CLOSE_HOVER: Color32 = Color32::from_rgb(255, 95, 86);

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

/// Accent when on, dim when off.
fn mode_button(ui: &mut egui::Ui, label: &str, on: bool, tip: &str) -> bool {
    let r = ui.add(egui::Button::new(RichText::new(label).color(if on { ACCENT } else { DIM })).frame(false)).on_hover_text(tip);
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r.clicked()
}

/// A small "[x]" window control.
fn title_button(ui: &mut egui::Ui, label: &str, hover: Color32) -> bool {
    let font = FontId::monospace(12.0);
    let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '_'));
    let (rect, r) = ui.allocate_exact_size(vec2(cw * label.chars().count() as f32 + 8.0, TITLE_H), Sense::click());
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, font, if r.hovered() { hover } else { DIM });
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r.clicked()
}

fn is_maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

fn command_id() -> egui::Id {
    egui::Id::unique("command")
}

/// The cover popup for the song under the pointer.
struct HoverArt {
    uri: String,
    since: Instant,
    anchor: egui::Pos2,
    asked: bool,
    galley: Option<std::sync::Arc<egui::Galley>>,
}

fn glyph_width(ctx: &egui::Context, size: f32) -> f32 {
    ctx.fonts_mut(|f| f.glyph_width(&FontId::monospace(size), '#'))
}

/// `disc::ascii` as one block of text, coloured character by character.
fn ascii_galley(ctx: &egui::Context, rgb: &[u8], cols: usize, rows: usize, size: f32, line: f32) -> std::sync::Arc<egui::Galley> {
    let font = FontId::monospace(size);
    let mut job = egui::text::LayoutJob::default();
    let mut buf = [0; 4];
    for (i, (ch, [r, g, b])) in disc::ascii(rgb, cols, rows, line / glyph_width(ctx, size)).into_iter().enumerate() {
        let format = egui::TextFormat {
            font_id: font.clone(),
            color: Color32::from_rgb(r, g, b),
            line_height: Some(line),
            ..Default::default()
        };
        if i > 0 && i % cols == 0 {
            job.append("\n", 0.0, format.clone());
        }
        job.append(ch.encode_utf8(&mut buf), 0.0, format);
    }
    ctx.fonts_mut(|f| f.layout_job(job))
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
    /// The command line as typed.
    command: String,
    cmd_sel: usize,
    /// So clicking a suggestion isn't lost when the line loses focus.
    cmd_popup: Rect,
    show_help: bool,
    /// Waiting for search results to play the top one.
    pending_play: Option<String>,
    /// What `command` is searching for; empty when it isn't.
    query: String,
    query_edited: Option<Instant>,
    sent_query: String,
    local_index: Option<Vec<(String, Track)>>,
    local_hits: Vec<Track>,
    remote: (String, Vec<Track>),
    artist: Option<ArtistPage>,
    now: Option<Track>,
    queue: Vec<Track>,
    queue_idx: usize,
    queue_auto_from: Option<usize>,
    repeat: Repeat,
    shuffle: bool,
    autoplay: bool,
    order: PlaylistOrder,
    scroll_sidebar: bool,
    cover: Option<egui::TextureHandle>,
    show_cover: bool,
    cover_rgb: Option<Vec<u8>>,
    /// Goes up with every new cover, so the art made from it is redrawn.
    cover_gen: u32,
    /// Where the window controls are, so the corner above them doesn't resize.
    title_buttons: Rect,
    mini: bool,
    /// Window to return to when leaving mini mode.
    full_size: Option<egui::Vec2>,
    full_maximized: bool,
    /// Keyed by cover generation and rotation step.
    disc: Option<((u32, u32), egui::TextureHandle)>,
    hovered_row: Option<String>,
    hover: Option<HoverArt>,
    /// Keyed by cover generation and characters across and down.
    ascii: Option<((u32, usize, usize), std::sync::Arc<egui::Galley>)>,
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
            command: String::new(),
            cmd_sel: 0,
            cmd_popup: Rect::NOTHING,
            show_help: false,
            pending_play: None,
            query: String::new(),
            query_edited: None,
            sent_query: String::new(),
            local_index: None,
            local_hits: Vec::new(),
            remote: (String::new(), Vec::new()),
            artist: None,
            now: None,
            queue: Vec::new(),
            queue_idx: 0,
            queue_auto_from: None,
            repeat: cfg.repeat,
            shuffle: cfg.shuffle,
            autoplay: cfg.autoplay.unwrap_or(true),
            order: cfg.order.clone(),
            scroll_sidebar: false,
            cover: None,
            show_cover: cfg.show_cover.unwrap_or(true),
            cover_rgb: None,
            cover_gen: 0,
            title_buttons: Rect::NOTHING,
            mini: false,
            full_size: None,
            full_maximized: false,
            disc: None,
            hovered_row: None,
            hover: None,
            ascii: None,
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
        self.clear_command();
        if id == QUEUE_VIEW {
            return;
        }
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
        self.clear_command();
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
        } else if self.view == QUEUE_VIEW {
            self.queue.get(self.queue_idx..).unwrap_or(&[])
        } else {
            self.lib.tracks(&self.view)
        }
    }

    /// Play row `i` of the main list; in the queue view that moves within the queue.
    fn play_main(&mut self, i: usize) {
        if self.view == QUEUE_VIEW {
            self.send(Cmd::Jump(self.queue_idx + i));
        } else {
            self.send(Cmd::Play(self.current_tracks().to_vec(), i));
            self.mark_view_played();
        }
    }

    /// Playing from a playlist lifts it to the top of the "recent" order.
    fn mark_view_played(&mut self) {
        if self.lib.playlists.iter().any(|p| p.id == self.view) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            self.order.mark_played(&self.view, now);
            self.send(Cmd::SaveOrder(self.order.clone()));
        }
    }

    fn move_open_playlist(&mut self, delta: isize) {
        if self.query.is_empty() && self.order.move_by(&self.lib.playlists, &self.view, delta) {
            self.scroll_sidebar = true;
            self.send(Cmd::SaveOrder(self.order.clone()));
        }
    }

    fn clear_command(&mut self) {
        self.command.clear();
        self.cmd_sel = 0;
        self.query.clear();
        self.query_edited = None;
        self.show_help = false;
    }

    fn on_command_edited(&mut self, ctx: &egui::Context) {
        self.cmd_sel = 0;
        let query = command::live_query(&self.command).unwrap_or("").to_string();
        if query != self.query {
            self.query = query;
            self.show_help = false;
            self.fonts.scan(ctx, std::iter::once(self.query.as_str()), false);
            self.query_edited = Some(Instant::now());
            self.update_local_search();
        }
    }

    /// Put `text` on the command line, with the cursor after it.
    fn fill_command(&mut self, ctx: &egui::Context, text: String) {
        let end = text.chars().count();
        self.command = text;
        self.on_command_edited(ctx);
        let id = command_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(end))));
        state.store(ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
        ctx.request_repaint();
    }

    fn suggestions(&self) -> Vec<Suggestion> {
        let names: Vec<&str> = self.order.sorted(&self.lib.playlists).iter().map(|p| p.name.as_str()).collect();
        command::suggest(&self.command, &names)
    }

    /// Enter: complete what still needs typing, otherwise run the line.
    fn submit_command(&mut self, ctx: &egui::Context) {
        let items = self.suggestions();
        match items.get(self.cmd_sel.min(items.len().saturating_sub(1))) {
            Some(s) if s.wants_more => self.fill_command(ctx, s.fill.clone()),
            Some(s) => self.run_command(ctx, s.fill.clone()),
            None if self.command.trim().is_empty() => {}
            None => self.run_command(ctx, self.command.clone()),
        }
    }

    fn run_command(&mut self, ctx: &egui::Context, line: String) {
        let action = command::parse(&line);
        // A search stays on the line so its results stay on screen.
        if !matches!(action, Ok(Action::Search(_))) {
            self.clear_command();
        }
        match action {
            Ok(a) => self.apply(ctx, a),
            Err(e) => self.status = e,
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Help => self.show_help = true,
            Action::Search(_) => {}
            Action::Play(Some(q)) => self.play_search(q),
            Action::Play(None) if !self.playing => self.send(Cmd::Toggle),
            Action::Play(None) => {}
            Action::Pause if self.playing => self.send(Cmd::Toggle),
            Action::Pause => {}
            Action::Next => self.send(Cmd::Next),
            Action::Prev => self.send(Cmd::Prev),
            Action::Shuffle => self.send(Cmd::ToggleShuffle),
            Action::Repeat => self.send(Cmd::CycleRepeat),
            Action::Autoplay => self.send(Cmd::ToggleAutoplay),
            Action::Art => self.toggle_cover(),
            Action::Volume(v) => {
                self.volume = v as f32 / 100.0;
                self.send(Cmd::Volume((self.volume * 65535.0) as u16));
            }
            Action::Mini => self.set_mini(ctx, true),
            Action::Liked => self.open_view(LIKED.into()),
            Action::Queue => self.open_view(QUEUE_VIEW.into()),
            Action::Open(name) => match self.find_playlist(&name) {
                Some(id) => self.open_view(id),
                None => self.status = format!("no playlist called: {name}"),
            },
            Action::Sort => self.cycle_sort(),
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn open_view(&mut self, id: String) {
        self.artist = None;
        self.open(id);
    }

    fn find_playlist(&self, name: &str) -> Option<String> {
        let name = name.to_lowercase();
        let sorted = self.order.sorted(&self.lib.playlists);
        let lower = |p: &&&Playlist| p.name.to_lowercase();
        sorted
            .iter()
            .find(|p| lower(p) == name)
            .or_else(|| sorted.iter().find(|p| lower(p).contains(&name)))
            .map(|p| p.id.clone())
    }

    /// The line was cleared just now, but its last results are still kept if they match.
    fn play_search(&mut self, q: String) {
        if self.remote.0 == q && !self.remote.1.is_empty() {
            let found = self.remote.1.clone();
            self.play_top(&q, &found);
        } else {
            self.status = format!("searching: {q}");
            self.sent_query = q.clone();
            self.pending_play = Some(q.clone());
            self.send(Cmd::Search(q));
        }
    }

    fn play_top(&mut self, q: &str, found: &[Track]) {
        match found.first() {
            Some(t) => {
                self.status.clear();
                self.send(Cmd::Play(vec![t.clone()], 0));
            }
            None => self.status = format!("no song found: {q}"),
        }
    }

    fn command_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("ggspot:~$").color(ACCENT));
            let id = command_id();
            let focused = ctx.memory(|m| m.has_focus(id));
            if focused {
                // Before the text box, so these keys choose instead of moving its cursor.
                let (up, down, tab) = ctx.input_mut(|i| {
                    (
                        i.consume_key(Modifiers::NONE, Key::ArrowUp),
                        i.consume_key(Modifiers::NONE, Key::ArrowDown),
                        i.consume_key(Modifiers::NONE, Key::Tab),
                    )
                });
                let items = self.suggestions();
                if !items.is_empty() {
                    let n = items.len();
                    let sel = self.cmd_sel.min(n - 1);
                    self.cmd_sel = if up { (sel + n - 1) % n } else if down { (sel + 1) % n } else { sel };
                    if tab {
                        self.fill_command(&ctx, items[self.cmd_sel].fill.clone());
                    }
                }
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.command)
                    .id(id)
                    .frame(egui::Frame::NONE)
                    .lock_focus(true)
                    .hint_text(RichText::new("type / for commands  (ctrl+g)").color(DIM))
                    .desired_width((ui.available_width() - 260.0).max(160.0)),
            );
            if r.changed() {
                self.on_command_edited(&ctx);
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.submit_command(&ctx);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(&self.status).color(DIM)).truncate());
            });
            let over_popup = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| self.cmd_popup.contains(p));
            let focused = ctx.memory(|m| m.has_focus(id));
            let items = if focused || over_popup { self.suggestions() } else { Vec::new() };
            self.command_popup(&ctx, r.rect, &items);
        });
    }

    /// Suggestions under the prompt, scrolled so the chosen one is always visible.
    fn command_popup(&mut self, ctx: &egui::Context, below: Rect, items: &[Suggestion]) {
        if items.is_empty() {
            self.cmd_popup = Rect::NOTHING;
            return;
        }
        let sel = self.cmd_sel.min(items.len() - 1);
        let first = sel.saturating_sub(POPUP_ROWS - 1);
        let width = (ctx.content_rect().right() - below.left() - 24.0).clamp(240.0, 560.0);
        let font = FontId::monospace(14.0);
        let mut clicked = None;
        let area = egui::Area::new(egui::Id::unique("command_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(below.left(), below.bottom() + 6.0))
            .fade_in(false)
            .show(ctx, |ui| {
                egui::Frame::NONE.fill(BG_INPUT).stroke(Stroke::new(1.0, BORDER)).inner_margin(4).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, s) in items.iter().enumerate().skip(first).take(POPUP_ROWS) {
                        let (rect, r) = ui.allocate_exact_size(vec2(width, POPUP_ROW_H), Sense::click());
                        let chosen = i == sel;
                        ui.painter().rect_filled(rect, 0.0, if chosen { SELECT } else if r.hovered() { HOVER } else { Color32::TRANSPARENT });
                        let label = Rect::from_x_y_ranges(rect.left() + 6.0..=rect.left() + 196.0, rect.y_range());
                        let about = Rect::from_x_y_ranges(rect.left() + 204.0..=rect.right() - 6.0, rect.y_range());
                        let color = if chosen { ACCENT } else { FG };
                        ui.painter().with_clip_rect(label).text(label.left_center(), Align2::LEFT_CENTER, &s.label, font.clone(), color);
                        ui.painter().with_clip_rect(about).text(about.left_center(), Align2::LEFT_CENTER, &s.about, font.clone(), DIM);
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if r.clicked() {
                            clicked = Some(i);
                        }
                    }
                    let tip = format!("{}/{}  tab complete  enter run  esc cancel", sel + 1, items.len());
                    ui.add_space(2.0);
                    ui.label(RichText::new(tip).size(12.0).color(DIM));
                });
            });
        self.cmd_popup = area.response.rect;
        if let Some(i) = clicked {
            let fill = items[i].fill.clone();
            if fill.ends_with(' ') {
                self.fill_command(ctx, fill);
            } else {
                self.run_command(ctx, fill);
            }
        }
    }

    fn help_view(&self, ui: &mut egui::Ui) {
        ui.label(RichText::new("# help").heading().color(ACCENT));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new("## commands  (ctrl+g, then type /)").color(DIM));
            for c in command::COMMANDS {
                ui.label(RichText::new(format!("{:<26}{}", c.usage(), c.about)).color(FG));
            }
            ui.add_space(8.0);
            ui.label(RichText::new("## keys").color(DIM));
            for (key, about) in KEYS {
                ui.label(RichText::new(format!("{key:<26}{about}")).color(FG));
            }
        });
    }

    fn toggle_cover(&mut self) {
        self.show_cover = !self.show_cover;
        self.send(Cmd::ShowCover(self.show_cover));
    }

    /// The cover as big square blocks: a whole number of pixels each, so edges stay crisp.
    fn cover_view(&self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let cells = cover::CELLS as f32;
        let block = (ui.available_width() / cells).floor().clamp(3.0, 8.0);
        let (rect, _) = ui.allocate_exact_size(vec2(cells * block, cells * block), Sense::hover());
        match &self.cover {
            Some(tex) => {
                let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
            }
            None => {
                ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "-- no cover --", FontId::monospace(12.0), DIM);
            }
        }
    }

    /// After resting on a song for a moment, its cover in ASCII beside the pointer.
    fn show_hover_art(&mut self, ctx: &egui::Context) {
        let Some(uri) = self.hovered_row.take().filter(|_| self.show_cover) else {
            self.hover = None;
            return;
        };
        if self.hover.as_ref().is_none_or(|h| h.uri != uri) {
            let Some(anchor) = ctx.input(|i| i.pointer.hover_pos()) else { return };
            self.hover = Some(HoverArt { uri: uri.clone(), since: Instant::now(), anchor, asked: false, galley: None });
        }
        let Some(hover) = self.hover.as_mut() else { return };
        if !hover.asked {
            match HOVER_DELAY.checked_sub(hover.since.elapsed()) {
                Some(left) if !left.is_zero() => ctx.request_repaint_after(left),
                _ => {
                    hover.asked = true;
                    let _ = self.cmd.send(Cmd::HoverCover(uri));
                }
            }
        }
        let Some(galley) = hover.galley.clone() else { return };
        let (r, p) = (ctx.content_rect(), hover.anchor);
        let size = galley.size() + vec2(14.0, 14.0);
        let x = if p.x + 18.0 + size.x <= r.right() { p.x + 18.0 } else { (p.x - 18.0 - size.x).max(r.left()) };
        let y = (p.y - size.y / 2.0).min(r.bottom() - size.y).max(r.top() + TITLE_H);
        egui::Area::new(egui::Id::unique("hover_art"))
            .order(egui::Order::Tooltip)
            .fixed_pos(egui::pos2(x, y))
            .interactable(false)
            .fade_in(false)
            .show(ctx, |ui| {
                egui::Frame::NONE.fill(BG_INPUT).stroke(Stroke::new(1.0, BORDER)).inner_margin(6).show(ui, |ui| {
                    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
                    ui.painter().galley(rect.min, galley.clone(), FG);
                });
            });
    }

    /// Our own window frame: a thin terminal line in place of the Windows title bar.
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("titlebar").frame(egui::Frame::NONE.fill(BG_INPUT)).show(ui, |ui| {
            let ctx = ui.ctx().clone();
            let maximized = is_maximized(&ctx);
            let bar = Rect::from_min_size(ui.max_rect().min, vec2(ui.available_width(), TITLE_H));
            let drag = ui.interact(bar, egui::Id::unique("titlebar_drag"), Sense::click_and_drag());
            if drag.double_clicked() && !self.mini {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            } else if drag.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            let font = FontId::monospace(12.0);
            let name = ui.painter().text(bar.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, "ggspot", font.clone(), ACCENT);
            let version = concat!("v", env!("CARGO_PKG_VERSION"));
            ui.painter().text(name.right_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, version, font, DIM);
            ui.allocate_ui_with_layout(bar.size(), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(4.0);
                if title_button(ui, "[x]", CLOSE_HOVER) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if !self.mini && title_button(ui, if maximized { "[=]" } else { "[#]" }, ACCENT) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                if title_button(ui, "[_]", ACCENT) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                self.title_buttons = ui.min_rect();
            });
        });
    }

    /// Without a Windows frame, resizing from the window's edges is up to us.
    fn window_edges(&self, ctx: &egui::Context) {
        use egui::viewport::ResizeDirection as Dir;
        if is_maximized(ctx) {
            return;
        }
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()).filter(|p| !self.title_buttons.contains(*p)) else { return };
        let r = ctx.content_rect();
        let (north, south) = (p.y < r.top() + EDGE, p.y > r.bottom() - EDGE);
        let (west, east) = (p.x < r.left() + EDGE, p.x > r.right() - EDGE);
        let (dir, cursor) = match (north, south, west, east) {
            (true, _, true, _) => (Dir::NorthWest, CursorIcon::ResizeNwSe),
            (true, _, _, true) => (Dir::NorthEast, CursorIcon::ResizeNeSw),
            (_, true, true, _) => (Dir::SouthWest, CursorIcon::ResizeNeSw),
            (_, true, _, true) => (Dir::SouthEast, CursorIcon::ResizeNwSe),
            (true, ..) => (Dir::North, CursorIcon::ResizeVertical),
            (_, true, ..) => (Dir::South, CursorIcon::ResizeVertical),
            (_, _, true, _) => (Dir::West, CursorIcon::ResizeHorizontal),
            (_, _, _, true) => (Dir::East, CursorIcon::ResizeHorizontal),
            _ => return,
        };
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }

    /// Shrink to the record, or go back to the window as it was.
    fn set_mini(&mut self, ctx: &egui::Context, on: bool) {
        if on == self.mini || (on && self.need_login) {
            return;
        }
        self.mini = on;
        if on {
            let (size, maximized) =
                ctx.input(|i| (i.viewport().inner_rect.map(|r| r.size()), i.viewport().maximized.unwrap_or(false)));
            self.full_size = size;
            self.full_maximized = maximized;
            if maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(MINI_MIN.into()));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(MINI_SIZE.into()));
        } else {
            let size = self.full_size.take().unwrap_or(FULL_SIZE.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(FULL_MIN.into()));
            if self.full_maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
    }

    /// The cover in coloured characters. Laid out once per cover and window size.
    fn paint_ascii(&mut self, ui: &mut egui::Ui, area: Rect) {
        let Some(rgb) = &self.cover_rgb else { return };
        let cw = glyph_width(ui.ctx(), ASCII_FONT);
        let (cols, rows) = ((area.width() / cw).ceil() as usize, (area.height() / ASCII_LINE).ceil() as usize);
        let key = (self.cover_gen, cols, rows);
        if self.ascii.as_ref().is_none_or(|(k, _)| *k != key) {
            self.ascii = Some((key, ascii_galley(ui.ctx(), rgb, cols, rows, ASCII_FONT, ASCII_LINE)));
        }
        if let Some((_, galley)) = &self.ascii {
            ui.painter().with_clip_rect(area).galley(area.min, galley.clone(), FG);
        }
    }

    /// The record, centred in `area`, in square blocks like the sidebar cover.
    fn paint_disc(&mut self, ui: &mut egui::Ui, area: Rect) {
        let step = self.position() / disc::STEP_MS;
        let key = (self.cover_gen, step);
        if self.disc.as_ref().is_none_or(|(k, _)| *k != key) {
            let rgba = disc::render(self.cover_rgb.as_deref(), disc::angle(step));
            let image = egui::ColorImage::from_rgba_unmultiplied([disc::SIZE, disc::SIZE], &rgba);
            match &mut self.disc {
                Some((k, tex)) => {
                    tex.set(image, egui::TextureOptions::NEAREST);
                    *k = key;
                }
                None => self.disc = Some((key, ui.ctx().load_texture("disc", image, egui::TextureOptions::NEAREST))),
            }
        }
        let Some((_, tex)) = &self.disc else { return };
        let blocks = disc::SIZE as f32;
        let block = ((area.width().min(area.height()) - 24.0) / blocks).floor().max(2.0);
        let rect = Rect::from_center_size(area.center().round(), vec2(blocks * block, blocks * block));
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
    }

    fn mini_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        match &self.now {
            Some(t) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if self.playing { "▶" } else { "‖" }).color(ACCENT));
                    ui.add(egui::Label::new(RichText::new(&t.name).color(FG)).truncate());
                });
                ui.add(egui::Label::new(RichText::new(&t.artists).color(DIM)).truncate());
            }
            None => {
                ui.label(RichText::new("-- nothing playing --").color(DIM));
            }
        }
        ui.horizontal(|ui| self.progress_row(ui, 84.0));
        ui.horizontal(|ui| {
            self.transport(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if mode_button(ui, "[full]", true, "back to the full window (ctrl+m)") {
                    let ctx = ui.ctx().clone();
                    self.set_mini(&ctx, false);
                }
                // Laid out right to left: whatever room is left, less "vol" and the brackets.
                let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&FontId::monospace(14.0), '='));
                let chars = ((ui.available_width() - 33.0) / cw - 2.0).clamp(4.0, 12.0) as usize;
                self.volume_bar(ui, chars);
                ui.label(RichText::new("vol").color(DIM));
            });
        });
    }

    fn mini_view(&mut self, ui: &mut egui::Ui) {
        let line = egui::Frame::NONE.fill(BG).inner_margin(egui::Margin::symmetric(10, 6));
        egui::Panel::bottom("mini").frame(line.stroke(Stroke::new(1.0, BORDER))).show(ui, |ui| self.mini_bar(ui));
        egui::CentralPanel::default_margins().frame(egui::Frame::NONE.fill(BG)).show(ui, |ui| {
            let area = ui.max_rect();
            self.paint_ascii(ui, area);
            self.paint_disc(ui, area);
        });
    }

    fn cycle_sort(&mut self) {
        self.order.mode = self.order.mode.cycled();
        self.scroll_sidebar = true;
        self.send(Cmd::SaveOrder(self.order.clone()));
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
                    if self.pending_play.as_deref() == Some(q.as_str()) {
                        self.pending_play = None;
                        self.play_top(&q, &r);
                    }
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
                Ev::Cover(uri, cells) => {
                    if self.now.as_ref().is_some_and(|t| t.uri == uri) {
                        let side = cover::CELLS as usize;
                        self.cover = cells.as_deref().map(|rgb| {
                            let image = egui::ColorImage::from_rgb([side, side], rgb);
                            ctx.load_texture("cover", image, egui::TextureOptions::NEAREST)
                        });
                        self.cover_rgb = cells;
                        self.cover_gen += 1;
                    }
                }
                Ev::HoverCover(uri, cells) => {
                    if let (Some(hover), Some(rgb)) = (self.hover.as_mut().filter(|h| h.uri == uri), cells) {
                        let rows = (HOVER_COLS as f32 * glyph_width(ctx, HOVER_FONT) / HOVER_LINE).round() as usize;
                        hover.galley = Some(ascii_galley(ctx, &rgb, HOVER_COLS, rows, HOVER_FONT, HOVER_LINE));
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
                Ev::Queue { tracks, idx, auto_from } => {
                    self.fonts.scan(ctx, track_texts(&tracks), false);
                    self.queue = tracks;
                    self.queue_idx = idx;
                    self.queue_auto_from = auto_from;
                }
                Ev::Modes { repeat, shuffle, autoplay } => {
                    self.repeat = repeat;
                    self.shuffle = shuffle;
                    self.autoplay = autoplay;
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
                if resp.hovered() || ar.hovered() {
                    self.hovered_row = Some(t.uri.clone());
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
            if id == "main" {
                self.play_main(i);
            } else {
                self.send(Cmd::Play(tracks.to_vec(), i));
            }
        }
        if let Some(t) = artist {
            self.open_artist(&t);
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Keys: Space play/pause, Ctrl+G command line, Esc clear/back, Ctrl+Left/Right prev/next,
        // Up/Down + Enter play.
        // Before anything reads the focus: egui drops focus on Esc as the frame starts.
        let clearing = !self.mini && (!self.command.is_empty() || self.show_help);
        if clearing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.clear_command();
            ctx.memory_mut(|m| m.surrender_focus(command_id()));
        }
        let typing = ctx.memory(|m| m.focused().is_some());
        let mut go_back = false;
        // `input_mut` holds egui's context lock: touching `ctx` inside it deadlocks.
        let (focus_search, focus_command) =
            ctx.input_mut(|i| (i.consume_key(Modifiers::COMMAND, Key::F), i.consume_key(Modifiers::COMMAND, Key::G)));
        let (move_up, move_down, cycle_sort, toggle_cover, toggle_mini) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::ALT, Key::ArrowUp),
                i.consume_key(Modifiers::ALT, Key::ArrowDown),
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::COMMAND, Key::I),
                i.consume_key(Modifiers::COMMAND, Key::M),
            )
        });
        if toggle_mini {
            self.set_mini(ctx, !self.mini);
        }
        if toggle_cover {
            self.toggle_cover();
        }
        if move_up {
            self.move_open_playlist(-1);
        }
        if move_down {
            self.move_open_playlist(1);
        }
        if cycle_sort {
            self.cycle_sort();
        }
        if focus_search {
            self.set_mini(ctx, false);
            let line = if command::live_query(&self.command).is_some() { self.command.clone() } else { "/srh ".to_string() };
            self.fill_command(ctx, line);
        }
        if focus_command {
            self.set_mini(ctx, false);
            ctx.memory_mut(|m| m.request_focus(command_id()));
        }
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::ArrowRight) {
                let _ = self.cmd.send(Cmd::Next);
            }
            if i.consume_key(Modifiers::COMMAND, Key::ArrowLeft) {
                let _ = self.cmd.send(Cmd::Prev);
            }
            if i.consume_key(Modifiers::COMMAND, Key::S) {
                let _ = self.cmd.send(Cmd::ToggleShuffle);
            }
            if i.consume_key(Modifiers::COMMAND, Key::R) {
                let _ = self.cmd.send(Cmd::CycleRepeat);
            }
            if !typing && i.consume_key(Modifiers::NONE, Key::Space) {
                let _ = self.cmd.send(Cmd::Toggle);
            }
            if !typing && (i.consume_key(Modifiers::NONE, Key::Escape) || i.consume_key(Modifiers::NONE, Key::Backspace)) {
                go_back = true;
            }
        });
        if go_back {
            if self.mini {
                self.set_mini(ctx, false);
            } else {
                self.back();
            }
        }
        if !typing && self.query.is_empty() && !self.need_login && !self.mini {
            let n = self.current_tracks().len();
            let (down, up, enter) = ctx.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter)));
            if n > 0 && down {
                self.selected = Some(self.selected.map_or(0, |s| (s + 1).min(n - 1)));
            }
            if n > 0 && up {
                self.selected = Some(self.selected.map_or(0, |s| s.saturating_sub(1)));
            }
            if enter && let Some(s) = self.selected {
                self.play_main(s);
            }
        }
    }

    fn volume_bar(&mut self, ui: &mut egui::Ui, chars: usize) {
        if let Some(v) = ascii_bar(ui, "volume", self.volume, chars) {
            self.volume = v;
            self.send(Cmd::Volume((v * 65535.0) as u16));
        }
    }

    fn transport(&self, ui: &mut egui::Ui) {
        if text_button(ui, "[|<]") {
            self.send(Cmd::Prev);
        }
        if text_button(ui, if self.playing { "[ || ]" } else { "[ >  ]" }) {
            self.send(Cmd::Toggle);
        }
        if text_button(ui, "[>|]") {
            self.send(Cmd::Next);
        }
    }

    /// Elapsed, seek bar, duration. `reserved` is the width left free after the bar starts.
    fn progress_row(&mut self, ui: &mut egui::Ui, reserved: f32) {
        let dur = self.now.as_ref().map_or(0, |t| t.ms);
        let shown = self.seek.map_or(self.position(), |f| (f * dur as f32) as u32);
        ui.label(RichText::new(fmt_ms(shown)).color(DIM));
        let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&FontId::monospace(14.0), '='));
        let chars = ((ui.available_width() - reserved) / cw).max(10.0) as usize;
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
    }

    fn player_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let mut open_artist = None;
        ui.horizontal(|ui| {
            self.transport(ui);
            ui.add_space(6.0);
            if mode_button(ui, "[shuf]", self.shuffle, "shuffle (ctrl+s)") {
                self.send(Cmd::ToggleShuffle);
            }
            let repeat = match self.repeat {
                Repeat::Off => "[rpt:off]",
                Repeat::All => "[rpt:all]",
                Repeat::One => "[rpt:one]",
            };
            if mode_button(ui, repeat, self.repeat != Repeat::Off, "repeat: off, all, one (ctrl+r)") {
                self.send(Cmd::CycleRepeat);
            }
            if mode_button(ui, "[auto]", self.autoplay, "autoplay: keep playing similar songs when the queue ends") {
                self.send(Cmd::ToggleAutoplay);
            }
            if mode_button(ui, "[art]", self.show_cover, "album cover (ctrl+i)") {
                self.toggle_cover();
            }
            if mode_button(ui, "[mini]", true, "mini mode: spinning record (ctrl+m)") {
                let ctx = ui.ctx().clone();
                self.set_mini(&ctx, true);
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
            self.progress_row(ui, 260.0);
            ui.add_space(16.0);
            ui.label(RichText::new("vol").color(DIM));
            self.volume_bar(ui, 10);
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

        self.handle_keys(&ctx);
        if self.need_login {
            self.set_mini(&ctx, false);
        }
        self.title_bar(ui);
        self.window_edges(&ctx);
        if !is_maximized(&ctx) {
            let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("window_border"));
            ctx.layer_painter(layer).rect_stroke(ctx.content_rect(), 0.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
        }
        if self.mini {
            self.mini_view(ui);
            if self.playing {
                ctx.request_repaint_after(Duration::from_millis(disc::STEP_MS as u64));
            }
            return;
        }
        let line = egui::Frame::NONE.fill(BG).inner_margin(egui::Margin::symmetric(10, 6));
        egui::Panel::top("top").frame(line.stroke(Stroke::new(1.0, BORDER))).show(ui, |ui| self.command_bar(ui));

        egui::Panel::bottom("player").frame(line.stroke(Stroke::new(1.0, BORDER))).show(ui, |ui| self.player_bar(ui));

        if !self.need_login {
            egui::Panel::left("sidebar")
                .frame(line)
                .resizable(true)
                .default_size(220.0)
                .show(ui, |ui| {
                    if self.show_cover && self.now.is_some() {
                        egui::Panel::bottom("cover")
                            .frame(egui::Frame::NONE)
                            .resizable(false)
                            .show(ui, |ui| self.cover_view(ui));
                    }
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.label(RichText::new("## library").color(DIM));
                        let mut open = None;
                        let in_list = self.query.is_empty() && self.view != ARTIST_VIEW;
                        let scroll = std::mem::take(&mut self.scroll_sidebar);
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
                            if on && scroll {
                                r.scroll_to_me(None);
                            }
                        };
                        entry(ui, LIKED, "liked songs");
                        entry(ui, QUEUE_VIEW, "queue");
                        ui.add_space(6.0);
                        let mut cycle = false;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("## playlists").color(DIM));
                            let label = match self.order.mode {
                                SortMode::Recent => "[recent]",
                                SortMode::Custom => "[custom]",
                                SortMode::Alpha => "[a-z]",
                            };
                            cycle = mode_button(
                                ui,
                                label,
                                true,
                                "sort: recent, custom, a-z (ctrl+o)\nalt+up / alt+down moves the open playlist",
                            );
                        });
                        for p in self.order.sorted(&self.lib.playlists) {
                            entry(ui, &p.id, &p.name);
                        }
                        if let Some(id) = open {
                            self.artist = None;
                            self.open(id);
                        }
                        if cycle {
                            self.cycle_sort();
                        }
                    });
                });
        }

        egui::CentralPanel::default_margins().frame(line).show(ui, |ui| {
            if self.need_login {
                self.login_screen(ui);
            } else if self.show_help {
                self.help_view(ui);
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
                    } else if self.view == QUEUE_VIEW {
                        ui.label(RichText::new("# queue").heading().color(ACCENT));
                        let auto = self.queue_auto_from.map_or(0, |a| self.queue.len() - a.max(self.queue_idx));
                        if auto > 0 {
                            ui.label(RichText::new(format!("{auto} from autoplay")).color(DIM));
                        }
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

        self.show_hover_art(&ctx);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(key: Key, modifiers: Modifiers) -> egui::RawInput {
        egui::RawInput {
            events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
            ..Default::default()
        }
    }

    fn playlist(id: &str) -> Playlist {
        Playlist { id: id.into(), name: id.into(), snapshot: String::new() }
    }

    #[test]
    fn alt_arrows_move_the_open_playlist_and_not_the_song_selection() {
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_ev_tx, ev_rx) = std::sync::mpsc::channel();
        let track = Track {
            uri: "spotify:track:1".into(),
            name: "x".into(),
            artists: String::new(),
            album: String::new(),
            ms: 1,
            artist_uris: Vec::new(),
        };
        let lib = Library {
            playlists: vec![playlist("a"), playlist("b")],
            contents: [("a".to_string(), (String::new(), vec![track.clone(), track]))].into(),
            ..Default::default()
        };
        let mut app = App::new(lib, &Config { last_view: Some("a".into()), ..Default::default() }, cmd_tx, ev_rx);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(pressed(Key::ArrowDown, Modifiers::ALT), |ui| app.handle_keys(ui.ctx()));
        output.textures_delta.clear();
        let order: Vec<&str> = app.order.sorted(&app.lib.playlists).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, ["b", "a"]);
        assert_eq!(app.order.mode, SortMode::Custom);
        assert_eq!(app.selected, None);
    }

    fn ctrl(key: Key) -> egui::RawInput {
        pressed(key, Modifiers { ctrl: true, command: true, ..Default::default() })
    }

    fn app() -> App {
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_ev_tx, ev_rx) = std::sync::mpsc::channel();
        App::new(Library::default(), &Config::default(), cmd_tx, ev_rx)
    }

    fn keys(app: &mut App, ctx: &egui::Context, input: egui::RawInput) {
        let mut output = ctx.run_ui(input, |ui| app.handle_keys(ui.ctx()));
        output.textures_delta.clear();
    }

    #[test]
    fn ctrl_m_toggles_mini_and_esc_or_ctrl_f_leave_it() {
        let ctx = egui::Context::default();
        let mut app = app();
        keys(&mut app, &ctx, ctrl(Key::M));
        assert!(app.mini);
        keys(&mut app, &ctx, ctrl(Key::M));
        assert!(!app.mini);
        keys(&mut app, &ctx, ctrl(Key::M));
        keys(&mut app, &ctx, pressed(Key::Escape, Modifiers::NONE));
        assert!(!app.mini);
        keys(&mut app, &ctx, ctrl(Key::M));
        keys(&mut app, &ctx, ctrl(Key::F));
        assert!(!app.mini);
    }

    fn hover_frame(app: &mut App, ctx: &egui::Context, uri: Option<&str>) {
        app.hovered_row = uri.map(String::from);
        let events = vec![egui::Event::PointerMoved(egui::pos2(100.0, 100.0))];
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.show_hover_art(ui.ctx()));
        output.textures_delta.clear();
    }

    #[test]
    fn a_song_is_only_fetched_after_resting_on_it_and_stale_covers_are_dropped() {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        let mut app = App::new(Library::default(), &Config::default(), cmd_tx, ev_rx);
        let ctx = egui::Context::default();
        hover_frame(&mut app, &ctx, Some("a"));
        hover_frame(&mut app, &ctx, Some("b"));
        assert!(cmd_rx.try_recv().is_err(), "passing over a song fetches nothing");

        app.hover.as_mut().unwrap().since -= HOVER_DELAY;
        hover_frame(&mut app, &ctx, Some("b"));
        assert!(matches!(cmd_rx.try_recv(), Ok(Cmd::HoverCover(u)) if u == "b"));
        hover_frame(&mut app, &ctx, Some("b"));
        assert!(cmd_rx.try_recv().is_err(), "asks once");

        let cells = vec![9; (cover::CELLS * cover::CELLS * 3) as usize];
        ev_tx.send(Ev::HoverCover("a".into(), Some(cells.clone()))).unwrap();
        app.drain_events(&ctx);
        assert!(app.hover.as_ref().unwrap().galley.is_none(), "reply for a song no longer pointed at");
        ev_tx.send(Ev::HoverCover("b".into(), Some(cells))).unwrap();
        app.drain_events(&ctx);
        assert!(app.hover.as_ref().unwrap().galley.is_some());

        hover_frame(&mut app, &ctx, None);
        assert!(app.hover.is_none(), "gone when the pointer leaves");
    }

    #[test]
    fn hover_art_follows_the_art_switch() {
        let ctx = egui::Context::default();
        let mut app = app();
        app.show_cover = false;
        hover_frame(&mut app, &ctx, Some("a"));
        assert!(app.hover.is_none());
    }

    fn text(s: &str) -> egui::RawInput {
        egui::RawInput { events: vec![egui::Event::Text(s.into())], ..Default::default() }
    }

    fn line(app: &mut App, ctx: &egui::Context, input: egui::RawInput) {
        let mut output = ctx.run_ui(input, |ui| {
            app.handle_keys(ui.ctx());
            app.command_bar(ui);
        });
        output.textures_delta.clear();
    }

    fn press(app: &mut App, ctx: &egui::Context, key: Key) {
        line(app, ctx, pressed(key, Modifiers::NONE));
    }

    /// Ctrl+G, then type `s`.
    fn type_line(app: &mut App, ctx: &egui::Context, s: &str) {
        line(app, ctx, ctrl(Key::G));
        line(app, ctx, text(s));
    }

    #[test]
    fn tab_completes_and_enter_runs() {
        let ctx = egui::Context::default();
        let mut app = app();
        type_line(&mut app, &ctx, "/mi");
        press(&mut app, &ctx, Key::Tab);
        assert_eq!(app.command, "/mini");
        press(&mut app, &ctx, Key::Enter);
        assert!(app.mini, "/mini ran");
        assert_eq!(app.command, "");
    }

    #[test]
    fn enter_on_part_of_a_word_runs_or_waits_for_the_argument() {
        let ctx = egui::Context::default();
        let mut app = app();
        type_line(&mut app, &ctx, "/hel");
        press(&mut app, &ctx, Key::Enter);
        assert!(app.show_help);
        press(&mut app, &ctx, Key::Escape);
        assert!(!app.show_help);

        type_line(&mut app, &ctx, "/sr");
        press(&mut app, &ctx, Key::Enter);
        assert_eq!(app.command, "/srh ");
    }

    #[test]
    fn arrows_choose_a_suggestion() {
        let ctx = egui::Context::default();
        let mut app = app();
        type_line(&mut app, &ctx, "/");
        press(&mut app, &ctx, Key::ArrowDown);
        press(&mut app, &ctx, Key::Tab);
        assert_eq!(app.command, "/srh ");
    }

    #[test]
    fn srh_searches_while_typing_and_esc_clears_it() {
        let ctx = egui::Context::default();
        let mut app = app();
        type_line(&mut app, &ctx, "/srh king");
        assert_eq!(app.query, "king");
        press(&mut app, &ctx, Key::Enter);
        assert_eq!(app.query, "king", "results stay after enter");
        press(&mut app, &ctx, Key::Escape);
        assert_eq!((app.command.as_str(), app.query.as_str()), ("", ""));
    }

    #[test]
    fn play_searches_then_plays_the_top_match() {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        let mut app = App::new(Library::default(), &Config::default(), cmd_tx, ev_rx);
        let ctx = egui::Context::default();
        type_line(&mut app, &ctx, "/play aizo");
        press(&mut app, &ctx, Key::Enter);
        assert!(matches!(cmd_rx.try_recv(), Ok(Cmd::Search(q)) if q == "aizo"));
        assert_eq!(app.command, "");

        let song = |uri: &str| Track {
            uri: uri.into(),
            name: uri.into(),
            artists: String::new(),
            album: String::new(),
            ms: 1,
            artist_uris: Vec::new(),
        };
        ev_tx.send(Ev::SearchResults("older".into(), vec![song("wrong")])).unwrap();
        app.drain_events(&ctx);
        assert!(cmd_rx.try_recv().is_err(), "another search's results don't play");
        ev_tx.send(Ev::SearchResults("aizo".into(), vec![song("top"), song("second")])).unwrap();
        app.drain_events(&ctx);
        assert!(matches!(cmd_rx.try_recv(), Ok(Cmd::Play(t, 0)) if t.len() == 1 && t[0].uri == "top"));
    }

    #[test]
    fn plain_text_is_not_a_command() {
        let ctx = egui::Context::default();
        let mut app = app();
        type_line(&mut app, &ctx, "hello");
        press(&mut app, &ctx, Key::Enter);
        assert!(app.status.contains("/help"), "{}", app.status);
        assert_eq!(app.command, "");
    }

    #[test]
    fn ctrl_f_starts_a_search() {
        let ctx = egui::Context::default();
        let mut app = app();
        line(&mut app, &ctx, ctrl(Key::F));
        line(&mut app, &ctx, text("aizo"));
        assert_eq!(app.command, "/srh aizo");
    }

    /// One frame of the window frame's code at the default size; returns what it asked Windows to do.
    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> Vec<egui::ViewportCommand> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(880.0, 580.0))),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            app.title_bar(ui);
            app.window_edges(ui.ctx());
        });
        output.textures_delta.clear();
        output.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    /// Hover, press and release at `pos`; returns everything asked of Windows.
    fn click(app: &mut App, ctx: &egui::Context, pos: egui::Pos2) -> Vec<egui::ViewportCommand> {
        let mut out = frame(app, ctx, vec![egui::Event::PointerMoved(pos)]);
        out.extend(frame(app, ctx, vec![button(pos, true)]));
        out.extend(frame(app, ctx, vec![button(pos, false)]));
        out
    }

    #[test]
    fn window_controls_do_what_they_say() {
        use egui::ViewportCommand as V;
        let ctx = egui::Context::default();
        let mut app = app();
        frame(&mut app, &ctx, Vec::new());
        let group = app.title_buttons;
        assert!(group.width() > 60.0 && group.right() > 870.0, "{group:?}");
        let y = group.center().y;
        assert!(click(&mut app, &ctx, egui::pos2(group.right() - 19.0, y)).contains(&V::Close));
        assert!(click(&mut app, &ctx, egui::pos2(group.center().x, y)).contains(&V::Maximized(true)));
        assert!(click(&mut app, &ctx, egui::pos2(group.left() + 14.0, y)).contains(&V::Minimized(true)));
    }

    #[test]
    fn dragging_the_title_moves_the_window_and_edges_resize_it() {
        use egui::{ViewportCommand as V, viewport::ResizeDirection as Dir};
        let ctx = egui::Context::default();
        let mut app = app();
        frame(&mut app, &ctx, Vec::new());
        let (from, to) = (egui::pos2(300.0, 11.0), egui::pos2(340.0, 30.0));
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(from)]);
        frame(&mut app, &ctx, vec![button(from, true)]);
        let moved = frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to)]);
        assert!(moved.contains(&V::StartDrag), "{moved:?}");
        frame(&mut app, &ctx, vec![button(to, false)]);

        for (pos, dir) in [
            (egui::pos2(878.0, 300.0), Dir::East),
            (egui::pos2(2.0, 300.0), Dir::West),
            (egui::pos2(400.0, 578.0), Dir::South),
            (egui::pos2(878.0, 578.0), Dir::SouthEast),
        ] {
            assert!(click(&mut app, &ctx, pos).contains(&V::BeginResize(dir)), "{dir:?}");
        }
    }

    #[test]
    fn the_corner_above_close_closes_instead_of_resizing() {
        use egui::ViewportCommand as V;
        let ctx = egui::Context::default();
        let mut app = app();
        frame(&mut app, &ctx, Vec::new());
        let pos = egui::pos2(app.title_buttons.right() - 19.0, 2.0);
        let out = click(&mut app, &ctx, pos);
        assert!(out.contains(&V::Close), "{out:?}");
        assert!(!out.iter().any(|c| matches!(c, V::BeginResize(_))), "{out:?}");
    }

    #[test]
    fn mini_needs_a_login_first() {
        let ctx = egui::Context::default();
        let mut app = app();
        app.need_login = true;
        keys(&mut app, &ctx, ctrl(Key::M));
        assert!(!app.mini);
    }

    /// Regression: Ctrl+F used to deadlock egui's context and freeze the app.
    #[test]
    fn ctrl_f_focuses_search_without_freezing() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
            let (_ev_tx, ev_rx) = std::sync::mpsc::channel();
            let mut app = App::new(Library::default(), &Config::default(), cmd_tx, ev_rx);
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                events: vec![egui::Event::Key {
                    key: Key::F,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers { ctrl: true, command: true, ..Default::default() },
                }],
                ..Default::default()
            };
            let mut focused = false;
            let mut output = ctx.run_ui(input, |ui| {
                app.handle_keys(ui.ctx());
                focused = ui.ctx().memory(|m| m.has_focus(command_id()));
            });
            output.textures_delta.clear();
            let _ = done_tx.send(focused);
        });
        let focused = done_rx.recv_timeout(Duration::from_secs(5)).expect("Ctrl+F froze the app");
        assert!(focused, "Ctrl+F should focus the search box");
    }
}
