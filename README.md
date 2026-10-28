# GGspot

A tiny, terminal-style Spotify player for Windows. It's for listening and nothing else: search, playlists, liked songs, artist pages. No home feed, no cover art, no browser engine.

Built with Rust, [egui](https://www.egui.rs) and [librespot](https://github.com/librespot-org/librespot). **Playback needs Spotify Premium.**

## Why it's fast

- **Instant start.** The library is cached on disk, so it shows up on the first frame (about 100 ms). It refreshes in the background.
- **No shared API quota.** Library, playlists, search and artist pages go through the same per-user client API the Spotify app uses. Nothing goes through a rate-limited shared developer app.
- **Only what changed.** A playlist is re-downloaded only when its revision changes, and every playlist is pre-fetched in the background (4 at a time).
- **Batched metadata.** Track info is fetched 200 tracks per request.
- **Search as you type.** Your own library is filtered instantly. Spotify is queried 250 ms after you stop typing, and stale requests are cancelled.

## Use

1. Run `ggspot.exe` and press **[ login with spotify ]**. This opens the normal Spotify login page in your browser (email, Google or Apple). You only do this once.
2. Double-click a song to play it. Click an artist name to see their top tracks and discography.

| Key | Action |
|---|---|
| `Space` | play / pause |
| `Ctrl+F` | search |
| `Esc` / `Backspace` | clear search / back from artist page |
| `Ctrl+Left` / `Ctrl+Right` | previous / next |
| `Up` / `Down`, `Enter` | select, play |

`ggspot.exe --offline` browses the cached library without touching the network.

Data lives in `%LOCALAPPDATA%\ggspot`: the library cache, your saved login and a 1 GB audio cache. Delete the `credentials` folder there to log out.

## Build

```
cargo build --release
```

`cargo run --release --example smoke` exercises login, search and artist pages without the UI.

GGspot is an independent project and is not affiliated with Spotify.
