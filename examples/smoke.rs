//! Smoke test for the backend without the UI (uses your saved login):
//! `cargo run --release --example smoke`
//!
//! Plays real songs at volume 0 to check search, artist pages, autoplay,
//! repeat and shuffle. Your settings are put back afterwards.
#[path = "../src/backend.rs"]
#[allow(dead_code)]
mod backend;
#[path = "../src/cover.rs"]
#[allow(dead_code)]
mod cover;
#[path = "../src/model.rs"]
#[allow(dead_code)]
mod model;
#[path = "../src/order.rs"]
#[allow(dead_code)]
mod order;
#[path = "../src/queue.rs"]
#[allow(dead_code)]
mod queue;

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use backend::{Cmd, Ev};
use model::Track;
use tokio::sync::mpsc::UnboundedSender;

fn wait<T>(rx: &Receiver<Ev>, secs: u64, mut f: impl FnMut(Ev) -> Option<T>) -> Option<T> {
    let end = Instant::now() + Duration::from_secs(secs);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Ev::Status(s)) if !s.is_empty() => println!("  status: {s}"),
            Ok(ev) => {
                if let Some(v) = f(ev) {
                    return Some(v);
                }
            }
            Err(_) => break,
        }
    }
    None
}

fn wait_playing(rx: &Receiver<Ev>) -> bool {
    wait(rx, 30, |e| matches!(e, Ev::State { playing: true, .. }).then_some(())).is_some()
}

/// Jump near the end of `t` and let it finish by itself.
fn finish(cmd: &UnboundedSender<Cmd>, t: &Track) {
    cmd.send(Cmd::Seek(t.ms.saturating_sub(2500))).unwrap();
}

fn next_now_playing(rx: &Receiver<Ev>, secs: u64) -> Option<Track> {
    wait(rx, secs, |e| match e {
        Ev::NowPlaying(t) => Some(t),
        _ => None,
    })
}

struct Checks(u32);

impl Checks {
    fn check(&mut self, name: &str, ok: bool) {
        println!("{} {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            self.0 += 1;
        }
    }
}

fn run(cmd: &UnboundedSender<Cmd>, rx: &Receiver<Ev>, c: &mut Checks) -> Option<()> {
    wait(rx, 20, |e| matches!(e, Ev::LoggedIn).then_some(()))?;

    // Search and artist page.
    let t = Instant::now();
    cmd.send(Cmd::Search("king gnu".into())).unwrap();
    let found = wait(rx, 20, |e| match e {
        Ev::SearchResults(_, r) => Some(r),
        _ => None,
    })?;
    println!("search: {} tracks in {} ms", found.len(), t.elapsed().as_millis());
    c.check("search returns songs", !found.is_empty());
    let seed = found.first()?.clone();
    cmd.send(Cmd::Artist(seed.artist_uris.first()?.clone())).unwrap();
    let page = wait(rx, 30, |e| match e {
        Ev::Artist(_, r) => Some(r),
        _ => None,
    })?;
    c.check("artist page returns songs", !page.is_empty());

    // Cover for a song that is only being pointed at.
    let hovered = found.last()?.uri.clone();
    cmd.send(Cmd::HoverCover(hovered.clone())).unwrap();
    let art = wait(rx, 20, |e| match e {
        Ev::HoverCover(uri, cells) if uri == hovered => Some(cells),
        _ => None,
    })?;
    c.check("hover cover loads", art.is_some());

    // Autoplay: a one-song queue grows, and the next song starts by itself.
    let t = Instant::now();
    cmd.send(Cmd::Play(vec![seed.clone()], 0)).unwrap();
    // Either can arrive first.
    let (mut len, mut playing, mut art) = (None, false, None);
    wait(rx, 30, |e| {
        match e {
            Ev::Queue { tracks, .. } if tracks.len() > 1 => {
                println!("autoplay: queue is {} songs after {} ms", tracks.len(), t.elapsed().as_millis());
                len = Some(tracks.len());
            }
            Ev::State { playing: true, .. } => {
                println!("song started after {} ms", t.elapsed().as_millis());
                playing = true;
            }
            Ev::Cover(_, cells) => {
                println!("cover: {:?} bytes after {} ms", cells.as_ref().map(Vec::len), t.elapsed().as_millis());
                art = Some(cells);
            }
            _ => {}
        }
        (len.is_some() && playing && art.is_some()).then_some(())
    });
    c.check("cover art loads", art.flatten().is_some());
    c.check("autoplay extends a one-song queue", len.is_some());
    c.check("song starts playing", playing);
    finish(cmd, &seed);
    let second = next_now_playing(rx, 30);
    if let Some(t) = &second {
        println!("autoplay continued with: {} - {}", t.name, t.artists);
    }
    c.check("autoplay continues after the last song", second.as_ref().is_some_and(|t| t.uri != seed.uri));
    let second = second?;
    wait_playing(rx);

    // Repeat one: the same song starts again.
    cmd.send(Cmd::CycleRepeat).unwrap();
    cmd.send(Cmd::CycleRepeat).unwrap();
    finish(cmd, &second);
    let again = next_now_playing(rx, 30);
    c.check("repeat one replays the song", again.as_ref().is_some_and(|t| t.uri == second.uri));
    wait_playing(rx);

    // Next button leaves a repeat-one song.
    cmd.send(Cmd::Next).unwrap();
    let third = next_now_playing(rx, 15);
    c.check("next moves on from repeat one", third.as_ref().is_some_and(|t| t.uri != second.uri));
    wait_playing(rx);

    // Shuffle keeps the current song.
    cmd.send(Cmd::ToggleShuffle).unwrap();
    let kept = wait(rx, 5, |e| match e {
        Ev::Queue { tracks, idx, .. } => Some(tracks[idx].uri.clone()),
        _ => None,
    });
    c.check("shuffle keeps the current song", kept == third.map(|t| t.uri));
    cmd.send(Cmd::ToggleShuffle).unwrap();

    // Repeat all on one song: loops, and autoplay stays out of it.
    cmd.send(Cmd::CycleRepeat).unwrap(); // one -> off
    cmd.send(Cmd::CycleRepeat).unwrap(); // off -> all
    wait(rx, 5, |e| matches!(e, Ev::Modes { repeat: queue::Repeat::All, .. }).then_some(()));
    cmd.send(Cmd::Play(vec![seed.clone()], 0)).unwrap();
    let mut longest = 0;
    wait(rx, 30, |e| match e {
        Ev::Queue { tracks, .. } => {
            longest = longest.max(tracks.len());
            None
        }
        Ev::State { playing: true, .. } => Some(()),
        _ => None,
    });
    finish(cmd, &seed);
    let looped = next_now_playing(rx, 30);
    c.check("repeat all loops a single song", looped.is_some_and(|t| t.uri == seed.uri));
    c.check("no autoplay while repeating", longest == 1);
    wait_playing(rx);

    // Autoplay off, repeat off: stops at the end, as it used to.
    cmd.send(Cmd::CycleRepeat).unwrap(); // all -> one
    cmd.send(Cmd::CycleRepeat).unwrap(); // one -> off
    cmd.send(Cmd::ToggleAutoplay).unwrap();
    wait(rx, 5, |e| matches!(e, Ev::Modes { autoplay: false, .. }).then_some(()));
    cmd.send(Cmd::Play(vec![seed.clone()], 0)).unwrap();
    wait_playing(rx);
    finish(cmd, &seed);
    let stopped = wait(rx, 30, |e| match e {
        Ev::NowPlaying(_) => Some(false),
        Ev::State { playing: false, pos_ms: 0 } => Some(true),
        _ => None,
    });
    c.check("stops at the end with autoplay off", stopped == Some(true));
    Some(())
}

fn main() {
    let config = model::data_dir().join("config.json");
    let saved = std::fs::read(&config).ok();
    // Silent, and from known modes.
    model::Config { volume: Some(0), ..Default::default() }.save();

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    backend::spawn(cmd_rx, ev_tx, eframe::egui::Context::default(), model::Library::load());

    let mut checks = Checks(0);
    let finished = run(&cmd_tx, &ev_rx, &mut checks).is_some();

    match saved {
        Some(bytes) => std::fs::write(&config, bytes).unwrap(),
        None => std::fs::remove_file(&config).unwrap(),
    }
    if !finished {
        println!("FAIL stopped early: an expected event never arrived");
        checks.0 += 1;
    }
    println!("{} failed", checks.0);
    std::process::exit((checks.0 > 0) as i32);
}
