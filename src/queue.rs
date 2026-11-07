//! Play order: repeat, shuffle and the autoplay tail. No I/O, so it is unit tested.

use serde::{Deserialize, Serialize};

use crate::model::Track;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn cycled(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

#[derive(Default)]
pub struct Queue {
    /// Play order.
    tracks: Vec<Track>,
    /// Same tracks in their source order, restored when shuffle is turned off.
    ordered: Vec<Track>,
    idx: usize,
    /// Index in `tracks` where autoplay-added songs begin (only ever a tail).
    auto_from: Option<usize>,
    pub repeat: Repeat,
    pub shuffle: bool,
}

impl Queue {
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }
    pub fn idx(&self) -> usize {
        self.idx
    }
    pub fn auto_from(&self) -> Option<usize> {
        self.auto_from
    }
    pub fn current(&self) -> Option<&Track> {
        self.tracks.get(self.idx)
    }

    /// New queue from a list, starting at `start`. Shuffled, `start` still plays first.
    pub fn set(&mut self, tracks: Vec<Track>, start: usize) {
        self.ordered = tracks.clone();
        self.tracks = tracks;
        self.idx = start.min(self.tracks.len().saturating_sub(1));
        self.auto_from = None;
        if self.shuffle && !self.tracks.is_empty() {
            self.tracks.swap(0, self.idx);
            self.idx = 0;
            fastrand::shuffle(&mut self.tracks[1..]);
        }
    }

    pub fn jump(&mut self, i: usize) -> bool {
        let ok = i < self.tracks.len();
        if ok {
            self.idx = i;
        }
        ok
    }

    pub fn restart(&mut self) {
        self.idx = 0;
    }

    /// The track ended by itself. False means there is nothing further to play.
    pub fn advance_on_end(&mut self) -> bool {
        (self.repeat == Repeat::One && !self.tracks.is_empty()) || self.skip_forward()
    }

    /// Next button (also leaves a repeat-one track). False at the end of the queue.
    pub fn skip_forward(&mut self) -> bool {
        match self.peek_next() {
            Some(i) => {
                self.idx = i;
                true
            }
            None => false,
        }
    }

    /// Previous button. False means there is nothing earlier: restart this track instead.
    pub fn skip_back(&mut self) -> bool {
        if self.idx > 0 {
            self.idx -= 1;
            true
        } else if self.repeat == Repeat::All && self.tracks.len() > 1 {
            self.idx = self.tracks.len() - 1;
            true
        } else {
            false
        }
    }

    fn peek_next(&self) -> Option<usize> {
        if self.idx + 1 < self.tracks.len() {
            Some(self.idx + 1)
        } else if self.repeat == Repeat::All && !self.tracks.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    /// Track to preload for a gapless change, if the next one is a different load.
    pub fn preload_target(&self) -> Option<&Track> {
        if self.repeat == Repeat::One {
            return None;
        }
        self.peek_next().filter(|&i| i != self.idx).map(|i| &self.tracks[i])
    }

    /// Autoplay tops up early, so a slow response never causes a gap.
    pub fn wants_autoplay(&self) -> bool {
        self.repeat == Repeat::Off && !self.tracks.is_empty() && self.idx + 2 >= self.tracks.len()
    }

    /// Seed for autoplay, and the songs it should not repeat.
    pub fn autoplay_seed(&self) -> Option<(String, Vec<String>)> {
        let last = self.tracks.last()?;
        let from = self.tracks.len().saturating_sub(50);
        Some((last.uri.clone(), self.tracks[from..].iter().map(|t| t.uri.clone()).collect()))
    }

    /// Append autoplay songs, skipping any already queued. Returns how many were added.
    pub fn extend_autoplay(&mut self, new: Vec<Track>) -> usize {
        let mut seen: std::collections::HashSet<String> = self.tracks.iter().map(|t| t.uri.clone()).collect();
        let fresh: Vec<Track> = new.into_iter().filter(|t| seen.insert(t.uri.clone())).collect();
        if !fresh.is_empty() {
            self.auto_from.get_or_insert(self.tracks.len());
            self.tracks.extend(fresh.iter().cloned());
            self.ordered.extend(fresh.iter().cloned());
        }
        fresh.len()
    }

    /// Like Spotify: the current song keeps playing, only what comes after it changes.
    pub fn set_shuffle(&mut self, on: bool) {
        if self.shuffle == on {
            return;
        }
        self.shuffle = on;
        if self.tracks.is_empty() {
            return;
        }
        if on {
            let end = self.auto_from.unwrap_or(self.tracks.len()).max(self.idx + 1);
            fastrand::shuffle(&mut self.tracks[self.idx + 1..end]);
        } else {
            let uri = self.tracks[self.idx].uri.clone();
            self.tracks = self.ordered.clone();
            self.idx = self.tracks.iter().position(|t| t.uri == uri).unwrap_or(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u32) -> Track {
        Track {
            uri: format!("spotify:track:{n}"),
            name: n.to_string(),
            artists: String::new(),
            album: String::new(),
            ms: 1000,
            artist_uris: Vec::new(),
        }
    }
    fn list(n: u32) -> Vec<Track> {
        (0..n).map(t).collect()
    }
    fn cur(q: &Queue) -> String {
        q.current().unwrap().name.clone()
    }

    #[test]
    fn stops_at_end_when_repeat_off() {
        let mut q = Queue::default();
        q.set(list(2), 0);
        assert!(q.advance_on_end());
        assert_eq!(cur(&q), "1");
        assert!(!q.advance_on_end());
        assert_eq!(cur(&q), "1");
    }

    #[test]
    fn repeat_all_wraps_both_ways() {
        let mut q = Queue { repeat: Repeat::All, ..Default::default() };
        q.set(list(3), 2);
        assert!(q.advance_on_end());
        assert_eq!(cur(&q), "0");
        assert!(q.skip_back());
        assert_eq!(cur(&q), "2");
    }

    #[test]
    fn repeat_all_replays_a_single_song() {
        let mut q = Queue { repeat: Repeat::All, ..Default::default() };
        q.set(list(1), 0);
        assert!(q.advance_on_end());
        assert_eq!(cur(&q), "0");
        assert!(q.preload_target().is_none());
    }

    #[test]
    fn repeat_one_loops_but_next_button_moves_on() {
        let mut q = Queue { repeat: Repeat::One, ..Default::default() };
        q.set(list(3), 0);
        assert!(q.advance_on_end());
        assert_eq!(cur(&q), "0");
        assert!(q.preload_target().is_none());
        assert!(q.skip_forward());
        assert_eq!(cur(&q), "1");
    }

    #[test]
    fn empty_queue_never_advances() {
        for repeat in [Repeat::Off, Repeat::All, Repeat::One] {
            let mut q = Queue { repeat, ..Default::default() };
            assert!(!q.advance_on_end());
            assert!(!q.skip_forward());
            assert!(!q.skip_back());
            assert!(!q.wants_autoplay());
        }
    }

    #[test]
    fn shuffled_set_plays_chosen_song_first_and_keeps_all_songs() {
        let mut q = Queue { shuffle: true, ..Default::default() };
        q.set(list(50), 7);
        assert_eq!(cur(&q), "7");
        assert_eq!(q.idx(), 0);
        let mut names: Vec<u32> = q.tracks().iter().map(|t| t.name.parse().unwrap()).collect();
        assert_ne!(names, (0..50).collect::<Vec<_>>());
        names.sort();
        assert_eq!(names, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn shuffle_toggle_keeps_current_song_and_restores_order() {
        let mut q = Queue::default();
        q.set(list(50), 10);
        q.set_shuffle(true);
        assert_eq!(cur(&q), "10");
        assert_eq!(q.idx(), 10);
        assert_eq!(q.tracks()[..=10], list(11)[..], "already played songs do not move");
        q.skip_forward();
        let now = cur(&q);
        q.set_shuffle(false);
        assert_eq!(cur(&q), now);
        assert_eq!(q.tracks(), &list(50)[..]);
    }

    #[test]
    fn autoplay_wanted_near_end_only_when_repeat_off() {
        let mut q = Queue::default();
        q.set(list(5), 0);
        assert!(!q.wants_autoplay());
        q.jump(3);
        assert!(q.wants_autoplay());
        q.repeat = Repeat::All;
        assert!(!q.wants_autoplay());
        q.repeat = Repeat::One;
        assert!(!q.wants_autoplay());
    }

    #[test]
    fn autoplay_appends_new_songs_and_continues() {
        let mut q = Queue::default();
        q.set(list(1), 0);
        assert!(q.wants_autoplay());
        let (seed, recent) = q.autoplay_seed().unwrap();
        assert_eq!(seed, "spotify:track:0");
        assert_eq!(recent, ["spotify:track:0"]);
        assert_eq!(q.extend_autoplay(vec![t(0), t(1), t(2), t(1)]), 2, "duplicates dropped");
        assert_eq!(q.auto_from(), Some(1));
        assert!(q.advance_on_end());
        assert_eq!(cur(&q), "1");
        assert_eq!(q.extend_autoplay(vec![t(1)]), 0);
    }

    #[test]
    fn autoplay_tail_survives_shuffle_toggle() {
        let mut q = Queue::default();
        q.set(list(3), 2);
        q.extend_autoplay(vec![t(10), t(11)]);
        q.set_shuffle(true);
        assert_eq!(q.tracks()[3..], [t(10), t(11)], "autoplay songs stay as the tail");
        assert_eq!(q.auto_from(), Some(3));
        q.set_shuffle(false);
        assert_eq!(q.tracks().len(), 5);
        assert_eq!(cur(&q), "2");
        assert_eq!(q.auto_from(), Some(3));
    }
}
