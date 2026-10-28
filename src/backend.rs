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
    context_page::ContextPage,
    extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery},
    extension_kind::ExtensionKind,
    metadata::Track as PbTrack,
    playlist4_external::SelectedListContent,
};
use protobuf::{EnumOrUnknown, Message};
use tokio::sync::{Semaphore, mpsc, watch};

use crate::model::*;

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
    Seek(u32),
    Volume(u16),
    View(String),
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

    let mut player: Option<(Arc<Player>, Arc<dyn Mixer>)> = None;
    let mut events: Option<mpsc::UnboundedReceiver<PlayerEvent>> = None;
    let mut queue: Vec<Track> = Vec::new();
    let mut idx = 0usize;
    let mut playing = false;
    let mut pos = (0u32, Instant::now());
    let mut pending: Option<(Vec<Track>, usize)> = None;
    let mut search_task: Option<tokio::task::JoinHandle<()>> = None;

    let load = |p: &Player, t: &Track| {
        if let Ok(uri) = SpotifyUri::from_uri(&t.uri) {
            p.load(uri, true, 0);
        }
    };

    loop {
        tokio::select! {
            Some(pv) = player_rx.recv() => {
                events = Some(pv.0.get_player_event_channel());
                player = Some(pv);
                if let Some((q, i)) = pending.take() {
                    queue = q; idx = i;
                    load(&player.as_ref().unwrap().0, &queue[idx]);
                    out.send(Ev::NowPlaying(queue[idx].clone()));
                }
            }
            Some(ev) = async { match events.as_mut() { Some(e) => e.recv().await, None => std::future::pending().await } } => {
                let p = &player.as_ref().unwrap().0;
                match ev {
                    PlayerEvent::Playing { position_ms, .. } => {
                        playing = true; pos = (position_ms, Instant::now());
                        out.send(Ev::State { playing, pos_ms: position_ms });
                    }
                    PlayerEvent::Paused { position_ms, .. } => {
                        playing = false; pos = (position_ms, Instant::now());
                        out.send(Ev::State { playing, pos_ms: position_ms });
                    }
                    PlayerEvent::TimeToPreloadNextTrack { .. } => {
                        if let Some(t) = queue.get(idx + 1)
                            && let Ok(uri) = SpotifyUri::from_uri(&t.uri)
                        {
                            p.preload(uri);
                        }
                    }
                    PlayerEvent::EndOfTrack { .. } | PlayerEvent::Unavailable { .. } => {
                        if idx + 1 < queue.len() {
                            idx += 1;
                            load(p, &queue[idx]);
                            out.send(Ev::NowPlaying(queue[idx].clone()));
                        } else {
                            playing = false;
                            out.send(Ev::State { playing, pos_ms: 0 });
                        }
                    }
                    _ => {}
                }
            }
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
                Cmd::Play(q, i) => {
                    if q.is_empty() { continue; }
                    // Session dropped (sleep, network change): reconnect, then play.
                    let dead = shared.session.borrow().as_ref().is_some_and(|s| s.is_invalid());
                    if dead {
                        player = None;
                        events = None;
                        start_session(false, false);
                    }
                    match &player {
                        Some((p, _)) => {
                            queue = q; idx = i;
                            load(p, &queue[idx]);
                            out.send(Ev::NowPlaying(queue[idx].clone()));
                        }
                        None => { out.status("connecting... will play when ready"); pending = Some((q, i)); }
                    }
                }
                Cmd::Toggle => if let Some((p, _)) = &player { if playing { p.pause() } else { p.play() } },
                Cmd::Next => if let Some((p, _)) = &player
                    && idx + 1 < queue.len()
                {
                    idx += 1; load(p, &queue[idx]); out.send(Ev::NowPlaying(queue[idx].clone()));
                },
                Cmd::Prev => if let Some((p, _)) = &player {
                    let now = if playing { pos.0 + pos.1.elapsed().as_millis() as u32 } else { pos.0 };
                    if now > 3000 || idx == 0 { p.seek(0); }
                    else { idx -= 1; load(p, &queue[idx]); out.send(Ev::NowPlaying(queue[idx].clone())); }
                },
                Cmd::Seek(ms) => if let Some((p, _)) = &player { p.seek(ms) },
                Cmd::Volume(v) => {
                    if let Some((_, m)) = &player { m.set_volume(v) }
                    volume.store(v, Ordering::Relaxed);
                    cfg.volume = Some(v); cfg.save();
                }
                Cmd::View(v) => { cfg.last_view = Some(v); cfg.save(); }
            },
            else => break,
        }
    }
}
