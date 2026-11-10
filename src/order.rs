//! How playlists are ordered in the sidebar. Kept apart from the library so a
//! sync with Spotify never disturbs it.

use std::cmp::Reverse;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model::Playlist;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortMode {
    #[default]
    Recent,
    Custom,
    Alpha,
}

impl SortMode {
    pub fn cycled(self) -> Self {
        match self {
            SortMode::Recent => SortMode::Custom,
            SortMode::Custom => SortMode::Alpha,
            SortMode::Alpha => SortMode::Recent,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistOrder {
    pub mode: SortMode,
    /// Playlist id -> when a song was last played from it (ms since the epoch).
    played: HashMap<String, u64>,
    custom: Vec<String>,
}

impl PlaylistOrder {
    pub fn sorted<'a>(&self, all: &'a [Playlist]) -> Vec<&'a Playlist> {
        let mut list: Vec<&Playlist> = all.iter().collect();
        match self.mode {
            // Stable: playlists never played keep Spotify's order, after the played ones.
            SortMode::Recent => list.sort_by_key(|p| Reverse(self.played.get(&p.id).copied().unwrap_or(0))),
            SortMode::Alpha => list.sort_by_cached_key(|p| p.name.to_lowercase()),
            SortMode::Custom => {
                let rank: HashMap<&str, usize> =
                    self.custom.iter().enumerate().map(|(i, id)| (id.as_str(), i + 1)).collect();
                // Rank 0: playlists added since the order was set go on top, where they get noticed.
                list.sort_by_key(|p| rank.get(p.id.as_str()).copied().unwrap_or(0));
            }
        }
        list
    }

    pub fn mark_played(&mut self, id: &str, now_ms: u64) {
        self.played.insert(id.to_string(), now_ms);
    }

    /// Move a playlist up (negative) or down in what is on screen. The list on
    /// screen becomes the custom order. False if it is already at that edge.
    pub fn move_by(&mut self, all: &[Playlist], id: &str, delta: isize) -> bool {
        let mut ids: Vec<String> = self.sorted(all).iter().map(|p| p.id.clone()).collect();
        let Some(from) = ids.iter().position(|x| x == id) else { return false };
        let Some(to) = from.checked_add_signed(delta).filter(|&to| to < ids.len()) else { return false };
        ids.swap(from, to);
        self.custom = ids;
        self.mode = SortMode::Custom;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(id: &str, name: &str) -> Playlist {
        Playlist { id: id.into(), name: name.into(), snapshot: String::new() }
    }
    fn all() -> Vec<Playlist> {
        vec![pl("a", "Zed"), pl("b", "alpha"), pl("c", "Mid")]
    }
    fn ids(o: &PlaylistOrder, all: &[Playlist]) -> Vec<String> {
        o.sorted(all).iter().map(|p| p.id.clone()).collect()
    }

    #[test]
    fn recent_defaults_to_spotify_order() {
        assert_eq!(ids(&PlaylistOrder::default(), &all()), ["a", "b", "c"]);
    }

    #[test]
    fn playing_moves_a_playlist_to_the_top() {
        let mut o = PlaylistOrder::default();
        o.mark_played("c", 100);
        assert_eq!(ids(&o, &all()), ["c", "a", "b"]);
        o.mark_played("b", 200);
        assert_eq!(ids(&o, &all()), ["b", "c", "a"]);
    }

    #[test]
    fn alpha_ignores_case() {
        let o = PlaylistOrder { mode: SortMode::Alpha, ..Default::default() };
        assert_eq!(ids(&o, &all()), ["b", "c", "a"]);
    }

    #[test]
    fn moving_starts_from_what_is_on_screen_and_switches_to_custom() {
        let mut o = PlaylistOrder::default();
        o.mark_played("c", 100);
        assert!(o.move_by(&all(), "a", 1));
        assert_eq!(o.mode, SortMode::Custom);
        assert_eq!(ids(&o, &all()), ["c", "b", "a"]);
    }

    #[test]
    fn moving_past_either_edge_changes_nothing() {
        let mut o = PlaylistOrder::default();
        assert!(!o.move_by(&all(), "a", -1));
        assert!(!o.move_by(&all(), "c", 1));
        assert!(!o.move_by(&all(), "nope", 1));
        assert_eq!(o.mode, SortMode::Recent);
    }

    #[test]
    fn custom_order_ignores_plays() {
        let mut o = PlaylistOrder::default();
        o.move_by(&all(), "a", 1);
        o.mark_played("c", 100);
        assert_eq!(ids(&o, &all()), ["b", "a", "c"]);
    }

    #[test]
    fn new_playlists_appear_on_top_of_a_custom_order() {
        let mut o = PlaylistOrder::default();
        o.move_by(&all(), "a", 1);
        let mut grown = vec![pl("new", "New")];
        grown.extend(all());
        assert_eq!(ids(&o, &grown), ["new", "b", "a", "c"]);
    }

    #[test]
    fn deleted_playlists_are_skipped() {
        let mut o = PlaylistOrder::default();
        o.move_by(&all(), "a", 1);
        assert_eq!(ids(&o, &[pl("c", "Mid"), pl("a", "Zed")]), ["a", "c"]);
    }
}
