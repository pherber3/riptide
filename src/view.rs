use serde::{Deserialize, Serialize};

use crate::tidal::{Card, Track};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Sort {
    #[default]
    Added,
    Title,
    Artist,
    Album,
    Year,
    Duration,
}

impl Sort {
    pub fn label(self, reverse: bool) -> &'static str {
        match (self, reverse) {
            (Sort::Added, false) => "Recently added",
            (Sort::Added, true) => "Oldest added",
            (Sort::Title, false) => "A–Z",
            (Sort::Title, true) => "Z–A",
            (Sort::Artist, false) => "Artist A–Z",
            (Sort::Artist, true) => "Artist Z–A",
            (Sort::Album, false) => "Album A–Z",
            (Sort::Album, true) => "Album Z–A",
            (Sort::Year, false) => "Newest",
            (Sort::Year, true) => "Oldest",
            (Sort::Duration, false) => "Shortest",
            (Sort::Duration, true) => "Longest",
        }
    }
}

/// A list page's filter and sort, and the row order they give. `key` names the remembered sort.
#[derive(Clone)]
pub struct View {
    pub key: &'static str,
    pub filter: String,
    pub sort: Sort,
    pub reverse: bool,
    rows: Vec<usize>,
    /// What `rows` was computed for: filter, sort, direction and list length.
    built: Option<(String, Sort, bool, usize)>,
}

impl View {
    pub fn new(key: &'static str, (sort, reverse): (Sort, bool)) -> Self {
        Self { key, filter: String::new(), sort, reverse, rows: Vec::new(), built: None }
    }

    /// The row order for `items`, recomputed only when the filter, sort or list changes.
    pub fn rows<T: Sortable>(&mut self, items: &[T]) -> &[usize] {
        let key = (self.filter.clone(), self.sort, self.reverse, items.len());
        if self.built.as_ref() != Some(&key) {
            let filter = self.filter.to_lowercase();
            self.rows = (0..items.len()).filter(|&i| filter.is_empty() || items[i].text().contains(&filter)).collect();
            if self.sort != Sort::Added {
                self.rows.sort_by_cached_key(|&i| items[i].key(self.sort));
            }
            if self.reverse {
                self.rows.reverse();
            }
            self.built = Some(key);
        }
        &self.rows
    }
}

pub trait Sortable {
    /// Lowercase text the filter matches.
    fn text(&self) -> String;
    fn key(&self, sort: Sort) -> (u32, String);
}

impl Sortable for Track {
    fn text(&self) -> String {
        format!("{} {} {}", self.title, self.artist, self.album).to_lowercase()
    }

    fn key(&self, sort: Sort) -> (u32, String) {
        match sort {
            Sort::Duration => (self.duration, String::new()),
            Sort::Artist => (0, self.artist.to_lowercase()),
            Sort::Album => (0, self.album.to_lowercase()),
            _ => (0, self.title.to_lowercase()),
        }
    }
}

impl Sortable for Card {
    fn text(&self) -> String {
        match self {
            Card::Album(a) => format!("{} {}", a.title, a.artist),
            Card::Artist(a) => a.name.clone(),
            Card::Playlist(p) => p.title.clone(),
            Card::Mix(m) => m.title.clone(),
            Card::Folder { name, .. } => name.clone(),
        }
        .to_lowercase()
    }

    /// Folders before everything else, then by the sort.
    fn key(&self, sort: Sort) -> (u32, String) {
        let folder = u32::from(!matches!(self, Card::Folder { .. }));
        match (self, sort) {
            (Card::Album(a), Sort::Year) => (u32::MAX - a.year().parse().unwrap_or(0), String::new()),
            (Card::Album(a), Sort::Artist) => (folder, a.artist.to_lowercase()),
            _ => (folder, self.text()),
        }
    }
}

/// Items in the given row order, or as they are.
pub fn ordered<'a, T>(items: &'a [T], order: Option<&'a [usize]>) -> impl Iterator<Item = &'a T> {
    (0..order.map_or(items.len(), <[usize]>::len)).map(move |p| &items[order.map_or(p, |o| o[p])])
}

pub fn in_order(tracks: &[Track], order: Option<&[usize]>) -> Vec<Track> {
    ordered(tracks, order).cloned().collect()
}
