//! Smoke test for the backend without the UI (uses your saved login):
//! `cargo run --release --example smoke`
#[path = "../src/backend.rs"]
#[allow(dead_code)]
mod backend;
#[path = "../src/model.rs"]
#[allow(dead_code)]
mod model;

use std::time::{Duration, Instant};

use backend::{Cmd, Ev};

fn wait<T>(rx: &std::sync::mpsc::Receiver<Ev>, secs: u64, mut f: impl FnMut(Ev) -> Option<T>) -> Option<T> {
    let end = Instant::now() + Duration::from_secs(secs);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Ev::Status(s)) if !s.is_empty() => println!("  status: {s}"),
            Ok(ev) => if let Some(v) = f(ev) { return Some(v) },
            Err(_) => break,
        }
    }
    None
}

fn main() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    backend::spawn(cmd_rx, ev_tx, eframe::egui::Context::default(), model::Library::load());
    wait(&ev_rx, 20, |e| matches!(e, Ev::LoggedIn).then_some(())).expect("login");
    println!("logged in");
    for q in ["king gnu", "อฟ", "newjeans"] {
        let t = Instant::now();
        cmd_tx.send(Cmd::Search(q.into())).unwrap();
        let r = wait(&ev_rx, 20, |e| match e { Ev::SearchResults(_, r) => Some(r), _ => None }).expect("search");
        println!("search {q:?}: {} tracks in {} ms", r.len(), t.elapsed().as_millis());
        for x in r.iter().take(3) { println!("  {} - {} ({} artist uris)", x.name, x.artists, x.artist_uris.len()); }
        if q == "king gnu" {
            let a = r[0].artist_uris[0].clone();
            let t = Instant::now();
            cmd_tx.send(Cmd::Artist(a.clone())).unwrap();
            let r = wait(&ev_rx, 30, |e| match e { Ev::Artist(_, r) => Some(r), _ => None }).expect("artist");
            println!("artist {a}: {} tracks in {} ms", r.len(), t.elapsed().as_millis());
            for x in r.iter().take(5) { println!("  {} - {}", x.name, x.album); }
        }
    }
}
