//! Network, auth and playback. Runs on its own tokio runtime; talks to the UI
//! only through `Cmd` (in) and `Ev` (out).
//!
//! One Spotify login (the normal accounts.spotify.com page) gives a librespot
//! session. Library, playlists and track metadata come from Spotify's own
//! client API on that session (per-user limits, no shared developer-app quota).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use librespot_core::{
    SpotifyId, SpotifyUri, authentication::Credentials, cache::Cache, config::SessionConfig,
    session::Session,
};
use librespot_metadata::{Metadata, Playlist as SpPlaylist};
use librespot_oauth::OAuthClientBuilder;
use librespot_playback::{
    audio_backend,
    config::{AudioFormat, Bitrate, PlayerConfig},
    mixer::{self, Mixer, MixerConfig},
    player::{Player, PlayerEvent},
};
use librespot_protocol::{
    autoplay_context_request::AutoplayContextRequest,
    context_page::ContextPage,
    extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery},
    extension_kind::ExtensionKind,
    metadata::Track as PbTrack,
    playlist4_external::SelectedListContent,
};
use protobuf::{EnumOrUnknown, Message};
use tokio::sync::{Semaphore, mpsc, watch};

use crate::model::*;
use crate::order::PlaylistOrder;
use crate::queue::{Queue, Repeat};

/// Spotify's desktop client ID: its login page is the regular Spotify one.
const SPOTIFY_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const LOGIN_REDIRECT: &str = "http://127.0.0.1:5588/login";
const LOGIN_SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
];
const LOGIN_DONE_HTML: &str = "<!doctype html><meta charset=utf-8><title>GGspot</title>\
<body style='font-family:sans-serif;background:#121212;color:#eee;text-align:center;padding-top:20vh'>\
<h2>Signed in</h2><p>You can close this tab and go back to GGspot.</p></body>";

pub enum Cmd {
    SignIn,
    Open(String),
    Search(String),
    Artist(String),
    Play(Vec<Track>, usize),
    Toggle,
    Next,
    Prev,
    Jump(usize),
    CycleRepeat,
    ToggleShuffle,
    ToggleAutoplay,
    Seek(u32),
    Volume(u16),
    View(String),
    SaveOrder(PlaylistOrder),
}

pub enum Ev {
    NeedLogin,
    LoggedIn,
    Status(String),
    Playlists(Vec<Playlist>),
    Liked(Vec<Track>),
    Contents(String, String, Vec<Track>),
    Loading(String),
    SearchResults(String, Vec<Track>),
    Artist(String, Vec<Track>),
    NowPlaying(Track),
    State { playing: bool, pos_ms: u32 },
    Queue { tracks: Vec<Track>, idx: usize, auto_from: Option<usize> },
    Modes { repeat: Repeat, shuffle: bool, autoplay: bool },
}

#[derive(Clone)]
struct Out {
    tx: std::sync::mpsc::Sender<Ev>,
    ctx: eframe::egui::Context,
}

impl Out {
    fn send(&self, ev: Ev) {
        let _ = self.tx.send(ev);
        self.ctx.request_repaint();
    }
    fn status(&self, s: impl Into<String>) {
        self.send(Ev::Status(s.into()));
    }
}

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub fn spawn(
    cmds: mpsc::UnboundedReceiver<Cmd>,
    tx: std::sync::mpsc::Sender<Ev>,
    ctx: eframe::egui::Context,
    lib: Library,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(run(cmds, Out { tx, ctx }, lib));
    });
}

// ------------------------------------------------------------------ Session

fn open_cache() -> Res<Cache> {
    let dir = data_dir();
    Cache::new(
        Some(dir.join("credentials")),
        None,
        Some(dir.join("audio")),
        Some(1024 * 1024 * 1024),
    )
    .map_err(err)
}

/// Connect with saved credentials, or (when `interactive`) open Spotify's
/// login page in the browser. `Ok(None)` means a login is needed.
async fn connect_session(interactive: bool, out: &Out) -> Res<Option<Session>> {
    let cache = open_cache()?;
    let creds = match cache.credentials() {
        Some(c) => c,
        None if !interactive => return Ok(None),
        None => {
            out.status("waiting for browser login...");
            let oauth = OAuthClientBuilder::new(SPOTIFY_CLIENT_ID, LOGIN_REDIRECT, LOGIN_SCOPES.to_vec())
                .open_in_browser()
                .with_custom_message(LOGIN_DONE_HTML)
                .build()
                .map_err(err)?;
            let t = oauth.get_access_token_async().await.map_err(err)?;
            Credentials::with_access_token(t.access_token)
        }
    };
    out.status("connecting...");
    let session = Session::new(SessionConfig::default(), Some(cache));
    session.connect(creds, true).await.map_err(err)?;
    Ok(Some(session))
}

/// Track metadata in one batched request per 200 tracks instead of one per track.
async fn fetch_tracks(session: &Session, uris: &[String]) -> Res<HashMap<String, Track>> {
    let mut out = HashMap::new();
    for chunk in uris.chunks(200) {
        let req = BatchedEntityRequest {
            entity_request: chunk
                .iter()
                .map(|u| EntityRequest {
                    entity_uri: u.clone(),
                    query: vec![ExtensionQuery {
                        extension_kind: EnumOrUnknown::new(ExtensionKind::TRACK_V4),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let res = session.spclient().get_extended_metadata(req).await.map_err(err)?;
        for arr in res.extended_metadata {
            for d in arr.extension_data {
                let Some(any) = d.extension_data.as_ref() else { continue };
                let Ok(t) = PbTrack::parse_from_bytes(&any.value) else { continue };
                out.insert(
                    d.entity_uri.clone(),
                    Track {
                        uri: d.entity_uri.clone(),
                        name: t.name().to_string(),
                        artists: t.artist.iter().map(|a| a.name()).collect::<Vec<_>>().join(", "),
                        album: t.album.name().to_string(),
                        ms: t.duration().max(0) as u32,
                        artist_uris: t
                            .artist
                            .iter()
                            .filter_map(|a| SpotifyId::from_raw(a.gid()).ok())
                            .filter_map(|id| SpotifyUri::Artist { id }.to_uri().ok())
                            .collect(),
                    },
                );
            }
        }
    }
    Ok(out)
}

/// Resolve URIs to tracks, fetching metadata only for ones not seen before.
async fn resolve(session: &Session, uris: &[String], known: &HashMap<String, Track>) -> Res<Vec<Track>> {
    // Tracks cached before artist links existed are fetched again once.
    let have = |u: &String| known.get(u).filter(|t| !t.artist_uris.is_empty());
    let missing: Vec<String> = uris.iter().filter(|u| have(u).is_none()).cloned().collect();
    let fresh = fetch_tracks(session, &missing).await?;
    Ok(uris
        .iter()
        .filter_map(|u| have(u).or_else(|| fresh.get(u)).cloned())
        .collect())
}

async fn fetch_playlist(session: &Session, id: &str, known: &HashMap<String, Track>) -> Res<Vec<Track>> {
    let uri = SpotifyUri::Playlist { user: None, id: SpotifyId::from_base62(id).map_err(err)? };
    let pl = SpPlaylist::get(session, &uri).await.map_err(err)?;
    let uris: Vec<String> = pl
        .tracks()
        .filter(|u| matches!(u, SpotifyUri::Track { .. }))
        .filter_map(|u| u.to_uri().ok())
        .collect();
    resolve(session, &uris, known).await
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The sidebar list ("rootlist"), names and revisions included, in the user's order.
async fn fetch_rootlist(session: &Session) -> Res<Vec<Playlist>> {
    const PAGE: usize = 200;
    let mut out = Vec::new();
    let mut unnamed = Vec::new();
    let mut from = 0;
    loop {
        let bytes = session.spclient().get_rootlist(from, Some(PAGE)).await.map_err(err)?;
        let list = SelectedListContent::parse_from_bytes(&bytes).map_err(err)?;
        let c = list.contents.get_or_default();
        let aligned = c.meta_items.len() == c.items.len();
        for (i, item) in c.items.iter().enumerate() {
            let Some(id) = item.uri().strip_prefix("spotify:playlist:") else { continue };
            let meta = aligned.then(|| &c.meta_items[i]);
            let name = meta.map(|m| m.attributes.name().to_string()).unwrap_or_default();
            if name.is_empty() {
                unnamed.push(out.len());
            }
            out.push(Playlist {
                id: id.to_string(),
                name,
                snapshot: meta.map(|m| hex(m.revision())).unwrap_or_default(),
            });
        }
        if c.items.len() < PAGE {
            break;
        }
        from += c.items.len();
    }
    // Rare: names missing from the rootlist; ask for those playlists directly.
    let tasks: Vec<_> = unnamed
        .iter()
        .map(|&i| {
            let (s, id) = (session.clone(), out[i].id.clone());
            tokio::spawn(async move {
                let uri = SpotifyUri::Playlist { user: None, id: SpotifyId::from_base62(&id).ok()? };
                SpPlaylist::get(&s, &uri).await.ok().map(|p| (p.name().to_string(), hex(&p.revision)))
            })
        })
        .collect();
    for (i, t) in unnamed.into_iter().zip(tasks) {
        if let Ok(Some((name, rev))) = t.await {
            out[i].name = name;
            out[i].snapshot = rev;
        }
    }
    Ok(out)
}

/// Liked Songs via the same context the Spotify app plays it from.
async fn fetch_liked(session: &Session, known: &HashMap<String, Track>) -> Res<Vec<Track>> {
    let sp = session.spclient();
    let ctx = sp
        .get_context(&format!("spotify:user:{}:collection", session.username()))
        .await
        .map_err(err)?;
    let page_from = |bytes: &[u8]| -> Res<ContextPage> {
        protobuf_json_mapping::parse_from_str::<ContextPage>(std::str::from_utf8(bytes).map_err(err)?).map_err(err)
    };
    let mut uris = Vec::new();
    let mut seen = HashSet::new();
    let mut pages: VecDeque<ContextPage> = ctx.pages.into();
    let mut budget = 500;
    while let Some(p) = pages.pop_front() {
        budget -= 1;
        if budget == 0 {
            break;
        }
        if p.tracks.is_empty() && !p.page_url().is_empty() {
            pages.push_front(page_from(&sp.get_next_page(p.page_url()).await.map_err(err)?)?);
            continue;
        }
        for t in &p.tracks {
            if t.uri().starts_with("spotify:track:") && seen.insert(t.uri().to_string()) {
                uris.push(t.uri().to_string());
            }
        }
        if !p.next_page_url().is_empty() {
            pages.push_back(page_from(&sp.get_next_page(p.next_page_url()).await.map_err(err)?)?);
        }
    }
    resolve(session, &uris, known).await
}

/// Track URIs of a context, loading lazy pages concurrently, up to `max` tracks.
async fn context_tracks(session: &Session, uri: &str, max: usize) -> Res<Vec<String>> {
    let sp = session.spclient();
    let ctx = sp.get_context(uri).await.map_err(err)?;
    let lazy: Vec<_> = ctx
        .pages
        .iter()
        .take(30)
        .map(|p| {
            let url = (p.tracks.is_empty() && !p.page_url().is_empty()).then(|| p.page_url().to_string());
            let s = session.clone();
            tokio::spawn(async move {
                let bytes = s.spclient().get_next_page(&url?).await.ok()?;
                protobuf_json_mapping::parse_from_str::<ContextPage>(std::str::from_utf8(&bytes).ok()?).ok()
            })
        })
        .collect();
    let mut uris = Vec::new();
    let mut seen = HashSet::new();
    for (page, task) in ctx.pages.iter().zip(lazy) {
        let loaded = task.await.ok().flatten();
        for t in loaded.as_ref().unwrap_or(page).tracks.iter() {
            if uris.len() < max && t.uri().starts_with("spotify:track:") && seen.insert(t.uri().to_string()) {
                uris.push(t.uri().to_string());
            }
        }
    }
    Ok(uris)
}

/// Catalogue search: the same "search context" the Spotify app plays results from.
async fn search(session: &Session, q: &str, known: &HashMap<String, Track>) -> Res<Vec<Track>> {
    let enc: String = q
        .trim()
        .bytes()
        .map(|b| match b {
            b' ' => "+".to_string(),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let uris = context_tracks(session, &format!("spotify:search:{enc}"), 50).await?;
    resolve(session, &uris, known).await
}

/// Artist page: top tracks first, then the discography.
async fn artist(session: &Session, uri: &str, known: &HashMap<String, Track>) -> Res<Vec<Track>> {
    let uris = context_tracks(session, uri, 300).await?;
    resolve(session, &uris, known).await
}

/// Songs Spotify would keep playing after `seed`, avoiding `recent`.
async fn fetch_autoplay(
    session: &Session,
    seed: String,
    recent: Vec<String>,
    known: &HashMap<String, Track>,
) -> Res<Vec<Track>> {
    let req = AutoplayContextRequest {
        context_uri: Some(seed),
        recent_track_uri: recent,
        ..Default::default()
    };
    let ctx = session.spclient().get_autoplay_context(&req).await.map_err(err)?;
    let uris: Vec<String> = ctx
        .pages
        .iter()
        .flat_map(|p| p.tracks.iter())
        .map(|t| t.uri().to_string())
        .filter(|u| u.starts_with("spotify:track:"))
        .collect();
    resolve(session, &uris, known).await
}

// ------------------------------------------------------------- Main loop

struct Shared {
    lib: Mutex<Library>,
    out: Out,
    session: watch::Receiver<Option<Session>>,
    fetch_slots: Semaphore,
}

impl Shared {
    fn known_tracks(&self) -> HashMap<String, Track> {
        let lib = self.lib.lock().unwrap();
        lib.liked
            .iter()
            .chain(lib.contents.values().flat_map(|c| c.1.iter()))
            .map(|t| (t.uri.clone(), t.clone()))
            .collect()
    }

    fn save(&self) {
        let lib = self.lib.lock().unwrap().clone();
        tokio::task::spawn_blocking(move || lib.save());
    }

    async fn session(&self) -> Session {
        let mut rx = self.session.clone();
        loop {
            if let Some(s) = rx.borrow_and_update().clone() {
                return s;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    /// Refresh one playlist if its cached copy is older than `snapshot`.
    async fn refresh_playlist(self: Arc<Self>, id: String, snapshot: String, foreground: bool) {
        let fresh = {
            let lib = self.lib.lock().unwrap();
            lib.contents.get(&id).is_some_and(|c| c.0 == snapshot && !snapshot.is_empty())
        };
        if fresh {
            return;
        }
        let _slot = if foreground { None } else { Some(self.fetch_slots.acquire().await.unwrap()) };
        if foreground {
            self.out.send(Ev::Loading(id.clone()));
        }
        let session = self.session().await;
        let known = self.known_tracks();
        match fetch_playlist(&session, &id, &known).await {
            Ok(tracks) => {
                self.lib.lock().unwrap().contents.insert(id.clone(), (snapshot.clone(), tracks.clone()));
                self.out.send(Ev::Contents(id, snapshot, tracks));
                self.save();
            }
            Err(e) if foreground => self.out.status(format!("playlist failed: {e}")),
            Err(_) => {}
        }
    }
}

/// Background refresh after login: list + liked first, then every changed playlist.
async fn sync_library(shared: Arc<Shared>) {
    let session = shared.session().await;
    let known = shared.known_tracks();
    let (pls, liked) = tokio::join!(fetch_rootlist(&session), fetch_liked(&session, &known));
    match liked {
        Ok(liked) => {
            let changed = {
                let mut lib = shared.lib.lock().unwrap();
                let changed = lib.liked != liked;
                lib.liked = liked.clone();
                changed
            };
            if changed {
                shared.out.send(Ev::Liked(liked));
                shared.save();
            }
        }
        Err(e) => shared.out.status(format!("liked songs failed: {e}")),
    }
    match pls {
        Ok(pls) => {
            {
                let mut lib = shared.lib.lock().unwrap();
                lib.playlists = pls.clone();
                let ids: HashSet<_> = pls.iter().map(|p| p.id.clone()).collect();
                lib.contents.retain(|k, _| ids.contains(k));
            }
            shared.out.send(Ev::Playlists(pls.clone()));
            shared.save();
            // Pre-fetch every changed playlist so opening one later is instant.
            for p in pls {
                tokio::spawn(shared.clone().refresh_playlist(p.id, p.snapshot, false));
            }
        }
        Err(e) => shared.out.status(format!("playlist failed: {e}")),
    }
}

/// What is playing, what comes next, and the autoplay top-up.
struct Playback {
    shared: Arc<Shared>,
    out: Out,
    queue: Queue,
    player: Option<(Arc<Player>, Arc<dyn Mixer>)>,
    autoplay: bool,
    auto_tx: mpsc::UnboundedSender<(u64, Res<Vec<Track>>)>,
    /// Bumped when the queue is replaced, so late autoplay answers are ignored.
    auto_gen: u64,
    auto_busy: bool,
    auto_exhausted: bool,
    /// The queue ran out; play the next song as soon as autoplay answers.
    waiting_for_auto: bool,
    ended: bool,
    playing: bool,
    pos: (u32, Instant),
    /// Unavailable songs in a row; stops an endless skip loop.
    failures: u32,
    /// Asked to play before the player exists.
    pending: bool,
    reconnect: bool,
}

impl Playback {
    fn publish_queue(&self) {
        self.out.send(Ev::Queue {
            tracks: self.queue.tracks().to_vec(),
            idx: self.queue.idx(),
            auto_from: self.queue.auto_from(),
        });
    }

    fn publish_modes(&self) {
        self.out.send(Ev::Modes {
            repeat: self.queue.repeat,
            shuffle: self.queue.shuffle,
            autoplay: self.autoplay,
        });
    }

    fn start_current(&mut self) {
        // Session dropped (sleep, network change): reconnect, then play.
        if self.shared.session.borrow().as_ref().is_some_and(|s| s.is_invalid()) {
            self.player = None;
            self.reconnect = true;
        }
        self.ended = false;
        self.waiting_for_auto = false;
        let Some(t) = self.queue.current().cloned() else { return };
        match (&self.player, SpotifyUri::from_uri(&t.uri)) {
            (Some((p, _)), Ok(uri)) => p.load(uri, true, 0),
            _ => {
                self.pending = true;
                self.out.status("connecting... will play when ready");
            }
        }
        self.pos = (0, Instant::now());
        self.out.send(Ev::NowPlaying(t));
        self.publish_queue();
        self.auto_exhausted = false;
        self.maybe_autoplay();
    }

    fn stop_at_end(&mut self) {
        self.playing = false;
        self.ended = true;
        self.waiting_for_auto = false;
        self.pos = (0, Instant::now());
        self.out.send(Ev::State { playing: false, pos_ms: 0 });
    }

    fn maybe_autoplay(&mut self) {
        if !self.autoplay || self.auto_busy || self.auto_exhausted || !self.queue.wants_autoplay() {
            return;
        }
        let Some((seed, recent)) = self.queue.autoplay_seed() else { return };
        self.auto_busy = true;
        let (shared, tx, generation) = (self.shared.clone(), self.auto_tx.clone(), self.auto_gen);
        tokio::spawn(async move {
            let session = shared.session().await;
            let res = fetch_autoplay(&session, seed, recent, &shared.known_tracks()).await;
            let _ = tx.send((generation, res));
        });
    }

    fn on_autoplay(&mut self, generation: u64, res: Res<Vec<Track>>) {
        if generation != self.auto_gen {
            return;
        }
        self.auto_busy = false;
        match res {
            Ok(tracks) => {
                let added = self.queue.extend_autoplay(tracks);
                self.auto_exhausted = added == 0;
                self.publish_queue();
            }
            Err(e) => {
                // No retry until the next song starts.
                self.auto_exhausted = true;
                self.out.status(format!("autoplay failed: {e}"));
            }
        }
        if self.waiting_for_auto {
            self.waiting_for_auto = false;
            if self.queue.skip_forward() {
                self.start_current();
            } else {
                self.stop_at_end();
            }
        } else {
            self.maybe_autoplay();
        }
    }

    /// Move past the current song; autoplay fills in when the queue has run out.
    fn advance(&mut self, moved: bool, at_end_of_track: bool) {
        if moved {
            self.start_current();
            return;
        }
        self.maybe_autoplay();
        if self.auto_busy {
            self.waiting_for_auto = true;
        } else if at_end_of_track {
            self.stop_at_end();
        }
    }

    fn on_player_event(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Playing { position_ms, .. } => {
                self.failures = 0;
                self.playing = true;
                self.pos = (position_ms, Instant::now());
                self.out.send(Ev::State { playing: true, pos_ms: position_ms });
            }
            PlayerEvent::Paused { position_ms, .. } => {
                self.playing = false;
                self.pos = (position_ms, Instant::now());
                self.out.send(Ev::State { playing: false, pos_ms: position_ms });
            }
            PlayerEvent::TimeToPreloadNextTrack { .. } => {
                if let (Some((p, _)), Some(t)) = (&self.player, self.queue.preload_target())
                    && let Ok(uri) = SpotifyUri::from_uri(&t.uri)
                {
                    p.preload(uri);
                }
            }
            PlayerEvent::EndOfTrack { .. } => {
                let moved = self.queue.advance_on_end();
                self.advance(moved, true);
            }
            PlayerEvent::Unavailable { .. } => {
                self.failures += 1;
                if self.failures >= 5 {
                    self.out.status("too many unavailable songs in a row");
                    self.stop_at_end();
                } else {
                    let moved = self.queue.skip_forward();
                    self.advance(moved, true);
                }
            }
            _ => {}
        }
    }

    fn set_queue(&mut self, tracks: Vec<Track>, start: usize) {
        self.auto_gen += 1;
        self.auto_busy = false;
        self.auto_exhausted = false;
        self.queue.set(tracks, start);
        self.start_current();
    }

    fn next(&mut self) {
        let moved = self.queue.skip_forward();
        self.advance(moved, false);
    }

    fn prev(&mut self) {
        let now = if self.playing { self.pos.0 + self.pos.1.elapsed().as_millis() as u32 } else { self.pos.0 };
        if now <= 3000 && self.queue.skip_back() {
            self.start_current();
        } else if let Some((p, _)) = &self.player {
            p.seek(0);
        }
    }

    fn toggle(&mut self) {
        if self.ended {
            self.queue.restart();
            self.start_current();
        } else if let Some((p, _)) = &self.player {
            if self.playing { p.pause() } else { p.play() }
        }
    }
}

async fn run(mut cmds: mpsc::UnboundedReceiver<Cmd>, out: Out, lib: Library) {
    let mut cfg = Config::load();
    let (session_tx, session_rx) = watch::channel(None);
    let shared = Arc::new(Shared {
        lib: Mutex::new(lib),
        out: out.clone(),
        session: session_rx,
        fetch_slots: Semaphore::new(4),
    });

    let (player_tx, mut player_rx) = mpsc::unbounded_channel::<(Arc<Player>, Arc<dyn Mixer>)>();
    let volume = Arc::new(AtomicU16::new(cfg.volume.unwrap_or(40000)));
    let start_session = {
        let (shared, volume) = (shared.clone(), volume.clone());
        move |interactive: bool, first: bool| {
            let (shared, volume) = (shared.clone(), volume.clone());
            let session_tx = session_tx.clone();
            let player_tx = player_tx.clone();
            tokio::spawn(async move {
                let out = &shared.out;
                match connect_session(interactive, out).await {
                    Ok(Some(session)) => {
                        let mixer = (mixer::find(None).unwrap())(MixerConfig::default()).unwrap();
                        mixer.set_volume(volume.load(Ordering::Relaxed));
                        let config = PlayerConfig {
                            bitrate: Bitrate::Bitrate320,
                            gapless: true,
                            position_update_interval: None,
                            ..Default::default()
                        };
                        let backend = audio_backend::find(None).unwrap();
                        let player = Player::new(config, session.clone(), mixer.get_soft_volume(), move || {
                            backend(None, AudioFormat::default())
                        });
                        let _ = session_tx.send(Some(session));
                        let _ = player_tx.send((player, mixer));
                        out.send(Ev::LoggedIn);
                        out.status("");
                        if first {
                            sync_library(shared.clone()).await;
                        }
                    }
                    Ok(None) => out.send(Ev::NeedLogin),
                    Err(e) => {
                        out.status(format!("login failed: {e}"));
                        if interactive || open_cache().ok().and_then(|c| c.credentials()).is_none() {
                            out.send(Ev::NeedLogin);
                        }
                    }
                }
            });
        }
    };

    // `--offline`: browse the cached library without touching the network.
    if std::env::args().any(|a| a == "--offline") {
        out.status("offline");
    } else {
        start_session(false, true);
    }

    let (auto_tx, mut auto_rx) = mpsc::unbounded_channel();
    let mut queue = Queue::default();
    queue.repeat = cfg.repeat;
    queue.shuffle = cfg.shuffle;
    let mut pb = Playback {
        shared: shared.clone(),
        out: out.clone(),
        queue,
        player: None,
        autoplay: cfg.autoplay.unwrap_or(true),
        auto_tx,
        auto_gen: 0,
        auto_busy: false,
        auto_exhausted: false,
        waiting_for_auto: false,
        ended: false,
        playing: false,
        pos: (0, Instant::now()),
        failures: 0,
        pending: false,
        reconnect: false,
    };
    pb.publish_modes();
    let mut events: Option<mpsc::UnboundedReceiver<PlayerEvent>> = None;
    let mut search_task: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        tokio::select! {
            Some(pv) = player_rx.recv() => {
                events = Some(pv.0.get_player_event_channel());
                pb.player = Some(pv);
                pb.reconnect = false;
                if std::mem::take(&mut pb.pending) {
                    pb.out.status("");
                    pb.start_current();
                }
            }
            Some(ev) = async { match events.as_mut() { Some(e) => e.recv().await, None => std::future::pending().await } } => {
                pb.on_player_event(ev);
            }
            Some((generation, res)) = auto_rx.recv() => pb.on_autoplay(generation, res),
            Some(cmd) = cmds.recv() => match cmd {
                Cmd::SignIn => start_session(true, true),
                Cmd::Open(id) => {
                    if id != LIKED {
                        let snap = shared.lib.lock().unwrap().playlists.iter()
                            .find(|p| p.id == id).map(|p| p.snapshot.clone()).unwrap_or_default();
                        tokio::spawn(shared.clone().refresh_playlist(id, snap, true));
                    }
                }
                Cmd::Search(q) => {
                    if let Some(t) = search_task.take() { t.abort(); }
                    let shared = shared.clone();
                    search_task = Some(tokio::spawn(async move {
                        let session = shared.session().await;
                        match search(&session, &q, &shared.known_tracks()).await {
                            Ok(r) => shared.out.send(Ev::SearchResults(q, r)),
                            Err(e) => shared.out.status(format!("search failed: {e}")),
                        }
                    }));
                }
                Cmd::Artist(uri) => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let session = shared.session().await;
                        match artist(&session, &uri, &shared.known_tracks()).await {
                            Ok(r) => shared.out.send(Ev::Artist(uri, r)),
                            Err(e) => shared.out.status(format!("artist failed: {e}")),
                        }
                    });
                }
                Cmd::Play(q, i) => if !q.is_empty() { pb.set_queue(q, i) },
                Cmd::Jump(i) => if pb.queue.jump(i) { pb.start_current() },
                Cmd::Toggle => pb.toggle(),
                Cmd::Next => pb.next(),
                Cmd::Prev => pb.prev(),
                Cmd::Seek(ms) => if let Some((p, _)) = &pb.player { p.seek(ms) },
                Cmd::CycleRepeat => {
                    pb.queue.repeat = pb.queue.repeat.cycled();
                    cfg.repeat = pb.queue.repeat;
                    cfg.save();
                    pb.publish_modes();
                    pb.maybe_autoplay();
                }
                Cmd::ToggleShuffle => {
                    pb.queue.set_shuffle(!pb.queue.shuffle);
                    cfg.shuffle = pb.queue.shuffle;
                    cfg.save();
                    pb.publish_modes();
                    pb.publish_queue();
                }
                Cmd::ToggleAutoplay => {
                    pb.autoplay = !pb.autoplay;
                    pb.auto_exhausted = false;
                    cfg.autoplay = Some(pb.autoplay);
                    cfg.save();
                    pb.publish_modes();
                    pb.maybe_autoplay();
                }
                Cmd::Volume(v) => {
                    if let Some((_, m)) = &pb.player { m.set_volume(v) }
                    volume.store(v, Ordering::Relaxed);
                    cfg.volume = Some(v); cfg.save();
                }
                Cmd::View(v) => { cfg.last_view = Some(v); cfg.save(); }
                Cmd::SaveOrder(o) => { cfg.order = o; cfg.save(); }
            },
            else => break,
        }
        if pb.player.is_none() {
            events = None;
        }
        if std::mem::take(&mut pb.reconnect) {
            start_session(false, false);
        }
    }
}
