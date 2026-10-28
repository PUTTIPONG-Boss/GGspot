use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const LIKED: &str = "liked";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub ms: u32,
    /// Artist URIs in the same order as the names in `artists`.
    #[serde(default)]
    pub artist_uris: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub snapshot: String,
}

/// Everything the UI shows, persisted so the next launch renders instantly.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Library {
    pub playlists: Vec<Playlist>,
    pub liked: Vec<Track>,
    /// playlist id -> (snapshot it was fetched at, tracks)
    pub contents: HashMap<String, (String, Vec<Track>)>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub volume: Option<u16>,
    pub last_view: Option<String>,
}

pub fn data_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("ggspot");
    // Carry over data (login included) from when the app was called Litespot.
    let old = base.join("litespot");
    if !dir.exists() && old.exists() {
        let _ = std::fs::rename(&old, &dir);
    }
    dir
}

fn load_json<T: for<'a> Deserialize<'a> + Default>(name: &str) -> T {
    std::fs::read(data_dir().join(name))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_json<T: Serialize>(name: &str, value: &T) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let Ok(bytes) = serde_json::to_vec(value) else { return };
    // Write then rename so a crash never leaves a half-written file.
    let tmp = dir.join(format!("{name}.tmp"));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(name));
    }
}

impl Library {
    pub fn load() -> Self {
        if data_dir().join("library.json").exists() {
            load_json("library.json")
        } else {
            Self::import_spotifast().unwrap_or_default()
        }
    }

    /// First launch: reuse Spotifast's cached library (plain Web API JSON) so
    /// liked songs and the playlist list show up before any network call.
    fn import_spotifast() -> Option<Self> {
        let cache = dirs::data_local_dir()?.join("paolino").join("spotifast").join("cache");
        let first_json = |sub: &str| -> Option<serde_json::Value> {
            let entry = std::fs::read_dir(cache.join(sub)).ok()?.flatten().find(|e| {
                e.path().extension().is_some_and(|x| x == "json")
            })?;
            serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok()
        };
        let mut lib = Library::default();
        if let Some(v) = first_json("liked-songs") {
            for item in v["items"].as_array().into_iter().flatten() {
                let t = &item["track"];
                let Some(uri) = t["uri"].as_str().filter(|u| u.starts_with("spotify:track:")) else { continue };
                lib.liked.push(Track {
                    uri: uri.into(),
                    name: t["name"].as_str().unwrap_or("").into(),
                    artists: t["artists"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default(),
                    album: t["album"]["name"].as_str().unwrap_or("").into(),
                    ms: t["duration_ms"].as_u64().unwrap_or(0) as u32,
                    artist_uris: t["artists"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x["uri"].as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                });
            }
        }
        if let Some(v) = first_json("library") {
            for p in v["data"].as_array().into_iter().flatten() {
                if let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) {
                    // Empty snapshot: contents are fetched fresh in the background.
                    lib.playlists.push(Playlist { id: id.into(), name: name.into(), snapshot: String::new() });
                }
            }
        }
        (!lib.liked.is_empty() || !lib.playlists.is_empty()).then_some(lib)
    }
    pub fn save(&self) {
        save_json("library.json", self)
    }
    pub fn tracks(&self, view: &str) -> &[Track] {
        if view == LIKED {
            &self.liked
        } else {
            self.contents.get(view).map(|c| c.1.as_slice()).unwrap_or(&[])
        }
    }
}

impl Config {
    pub fn load() -> Self {
        load_json("config.json")
    }
    pub fn save(&self) {
        save_json("config.json", self)
    }
}

pub fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}
