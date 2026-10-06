use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::tidal::Track;

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

/// What plays, in order, and where playback is in it.
#[derive(Default, Serialize, Deserialize)]
pub struct Queue {
    pub tracks: Vec<Track>,
    pub index: Option<usize>,
    pub repeat: Repeat,
    /// The order before shuffling, while shuffle is on.
    unshuffled: Option<Vec<Track>>,
}

impl Queue {
    /// The queue and the position in its current track, as the app was left.
    pub fn load(path: &Path) -> Option<(Self, f64)> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    pub fn save(&self, path: &Path, position: f64) {
        if let Ok(json) = serde_json::to_vec(&(self, position)) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn current(&self) -> Option<&Track> {
        self.tracks.get(self.index?)
    }

    pub fn shuffled(&self) -> bool {
        self.unshuffled.is_some()
    }

    /// Replaces the queue and returns where to start: at `index`, or, with shuffle on, that track
    /// first and the rest shuffled.
    pub fn replace(&mut self, mut tracks: Vec<Track>, index: usize, shuffle: bool) -> usize {
        let shuffle = shuffle || self.shuffled();
        self.unshuffled = shuffle.then(|| tracks.clone());
        if shuffle {
            let first = tracks.remove(index);
            shuffle_slice(&mut tracks);
            tracks.insert(0, first);
        }
        self.tracks = tracks;
        if shuffle { 0 } else { index }
    }

    pub fn set_shuffle(&mut self, on: bool) {
        let current = self.current().map(|t| t.id);
        if on {
            self.unshuffled = Some(self.tracks.clone());
            let from = self.index.map_or(0, |i| i + 1);
            shuffle_slice(&mut self.tracks[from..]);
        } else if let Some(order) = self.unshuffled.take() {
            self.tracks = order;
            self.index = current.and_then(|id| self.tracks.iter().position(|t| t.id == id));
        }
    }

    /// Adds `track` right after the current one (`next`) or at the end. Returns false when nothing
    /// was playing, in which case it is now the whole queue, waiting to be played.
    pub fn enqueue(&mut self, track: Track, next: bool) -> bool {
        let Some(i) = self.index else {
            (self.tracks, self.unshuffled) = (vec![track], None);
            return false;
        };
        if let Some(order) = &mut self.unshuffled {
            order.push(track.clone());
        }
        let at = if next { i + 1 } else { self.tracks.len() };
        self.tracks.insert(at, track);
        true
    }

    pub fn move_track(&mut self, from: usize, to: usize) {
        let track = self.tracks.remove(from);
        self.tracks.insert(to, track);
        self.index = self.index.map(|i| match i {
            i if i == from => to,
            i if from < i && i <= to => i - 1,
            i if to <= i && i < from => i + 1,
            i => i,
        });
    }

    pub fn remove(&mut self, i: usize) {
        self.tracks.remove(i);
        self.index = self.index.map(|c| if i < c { c - 1 } else { c });
    }

    /// What follows the current track: the next one, or the first again with repeat-all.
    pub fn after(&self) -> Option<usize> {
        let i = self.index?;
        match i + 1 < self.tracks.len() {
            true => Some(i + 1),
            false => (self.repeat == Repeat::All).then_some(0),
        }
    }

    /// Appends the tracks not already queued (autoplay), returning the first new one's index.
    pub fn extend_new(&mut self, tracks: Vec<Track>) -> Option<usize> {
        let known: HashSet<u64> = self.tracks.iter().map(|t| t.id).collect();
        let first = self.tracks.len();
        self.tracks.extend(tracks.into_iter().filter(|t| !known.contains(&t.id)));
        (self.tracks.len() > first).then_some(first)
    }

    pub fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }
}

/// A random index below `n`.
pub fn random(n: usize) -> usize {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as usize);
    nanos % n.max(1)
}

fn shuffle_slice<T>(items: &mut [T]) {
    let mut seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        items.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}
