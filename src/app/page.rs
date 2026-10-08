use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::tidal::{self, Card, Item, Mix, ROOT, Shelf, Tidal, Track};
use crate::view::{Sort, View};

const ALBUM_SORTS: &[Sort] = &[Sort::Added, Sort::Title, Sort::Artist, Sort::Year];
const NAME_SORTS: &[Sort] = &[Sort::Added, Sort::Title];

/// Tidal content the app can open as a page or play.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum Source {
    Home,
    Search(String),
    Album(u64),
    Artist(u64),
    Playlist(String),
    Mix(Mix),
    /// A playlist folder: id ("root" is the top level) and name.
    Folder(String, String),
    Tracks,
    Albums,
    Artists,
    TrackRadio(u64),
    ArtistRadio(u64),
    /// A browse page by its path: Explore, a genre, a "View all" (see `Tidal::page`).
    Page(String),
}

impl Source {
    pub fn open(card: &Card) -> Self {
        match card {
            Card::Album(a) => Self::Album(a.id),
            Card::Artist(a) => Self::Artist(a.id),
            Card::Playlist(p) => Self::Playlist(p.id.clone()),
            Card::Mix(m) => Self::Mix(m.clone()),
            Card::Folder { id, name, .. } => Self::Folder(id.clone(), name.clone()),
        }
    }

    /// What a card's play button plays: its tracks, or an artist's radio.
    pub fn play(card: &Card) -> Option<Self> {
        match card {
            Card::Folder { .. } => None,
            Card::Artist(a) => Some(Self::ArtistRadio(a.id)),
            _ => Some(Self::open(card)),
        }
    }

    /// The remembered sort of a page that can be filtered and sorted.
    pub fn sort_key(&self) -> Option<&'static str> {
        match self {
            Self::Tracks => Some("tracks"),
            Self::Albums => Some("albums"),
            Self::Artists => Some("artists"),
            Self::Folder(..) => Some("playlists"),
            Self::Playlist(_) => Some("playlist"),
            _ => None,
        }
    }

    pub fn playlists() -> Self {
        Self::Folder(ROOT.into(), "Playlists".into())
    }

    pub fn explore() -> Self {
        Self::Page("pages/explore".into())
    }

    /// How long a page of this kind can be shown again as it was, without asking Tidal: only
    /// recommendations and editorial pages, which change slowly. Anything else is always fetched.
    pub fn keeps(&self) -> Option<Duration> {
        match self {
            Self::Home | Self::Page(_) => Some(Duration::from_secs(10 * 60)),
            _ => None,
        }
    }
}

/// A page's title, artwork (with whether it is round) and radio.
#[derive(Clone)]
pub struct Head {
    /// What the header's save button saves, or for the user's own playlist, what its menu changes.
    pub item: Option<Item>,
    pub title: String,
    /// Who made it (an album's artist), linked, ahead of the subtitle.
    pub by: Option<(String, Source)>,
    pub subtitle: String,
    pub art: Option<(Option<String>, bool)>,
    pub radio: Option<Source>,
}

impl Head {
    fn title(title: impl Into<String>) -> Option<Self> {
        Some(Self { item: None, title: title.into(), by: None, subtitle: String::new(), art: None, radio: None })
    }
}

#[derive(Clone)]
pub enum Body {
    Tracks { tracks: Vec<Track>, album_column: bool },
    Grid { cards: Vec<Card>, sorts: &'static [Sort] },
    Shelves(Vec<Shelf>),
}

impl Body {
    /// What the page's Play and Shuffle buttons play.
    pub fn tracks(&self) -> &[Track] {
        match self {
            Self::Tracks { tracks, .. } => tracks,
            Self::Shelves(shelves) => shelves.iter().find(|s| !s.tracks.is_empty()).map_or(&[], |s| &s.tracks),
            Self::Grid { .. } => &[],
        }
    }
}

#[derive(Clone)]
pub struct Page {
    pub source: Source,
    pub head: Option<Head>,
    pub body: Body,
    /// Filter and sort, on pages that have them.
    pub view: Option<View>,
}

impl Page {
    /// What playing from this page shows it was played from: the page and its title.
    pub fn origin(&self) -> Option<(Source, String)> {
        let head = self.head.as_ref().filter(|_| !matches!(self.source, Source::Search(_)))?;
        Some((self.source.clone(), head.title.clone()))
    }
}

/// The pages behind and ahead of the one showing, as in a browser.
#[derive(Default)]
pub struct History {
    back: Vec<Page>,
    forward: Vec<Page>,
}

impl History {
    const MAX: usize = 30;

    /// Puts `page` in place of the one showing, which goes behind it unless the new page only
    /// refreshes it. Whatever was ahead is gone.
    pub fn open(&mut self, showing: &mut Option<Page>, page: Page, in_place: bool) {
        if let Some(old) = showing.replace(page).filter(|_| !in_place) {
            self.back.push(old);
            if self.back.len() > Self::MAX {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// Swaps the page showing for the one behind it (or ahead).
    pub fn step(&mut self, showing: &mut Option<Page>, back: bool) {
        let (from, to) = if back { (&mut self.back, &mut self.forward) } else { (&mut self.forward, &mut self.back) };
        if let Some(page) = from.pop() {
            to.extend(showing.replace(page));
        }
    }

    pub fn can_step(&self, back: bool) -> bool {
        !if back { &self.back } else { &self.forward }.is_empty()
    }
}

/// Slow-changing pages as they were last loaded, to show at once when opened again (see
/// `Source::keeps`).
#[derive(Default)]
pub struct Kept(Vec<(Page, Instant)>);

impl Kept {
    /// Keeps a freshly loaded page if its kind keeps, dropping any that have gone stale.
    pub fn keep(&mut self, page: &Page) {
        self.0.retain(|(kept, at)| kept.source != page.source && kept.source.keeps().is_some_and(|keep| at.elapsed() < keep));
        if page.source.keeps().is_some() {
            self.0.push((page.clone(), Instant::now()));
        }
    }

    /// The kept copy of a page, and whether it is still fresh enough to show without fetching.
    pub fn get(&self, source: &Source) -> Option<(Page, bool)> {
        let (page, at) = self.0.iter().find(|(kept, _)| kept.source == *source)?;
        Some((page.clone(), source.keeps().is_some_and(|keep| at.elapsed() < keep)))
    }
}

/// Loads any page. Playing a source loads its page too and plays the page's tracks.
pub async fn load(tidal: Tidal, source: Source) -> Result<Page> {
    let tracks = |tracks, album_column| Body::Tracks { tracks, album_column };
    let (head, body) = match &source {
        Source::Home => match tidal.home().await {
            Ok(shelves) if !shelves.is_empty() => (None, Body::Shelves(shelves)),
            _ => return Box::pin(load(tidal, Source::playlists())).await,
        },
        Source::Search(query) => (Head::title(format!("Results for “{query}”")), Body::Shelves(tidal.search(query).await?)),
        Source::Album(id) => {
            let (album, list) = tidal.album(*id).await?;
            let (by, subtitle) = match album.artist_id {
                Some(artist) => (Some((album.artist, Source::Artist(artist))), album.year),
                // Without an id there is no artist page to open: the name is plain text.
                None => (None, [album.artist, album.year].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")),
            };
            let art = Some((tidal::image(album.cover.as_deref(), 640), false));
            (Some(Head { item: Some(Item::Album(*id)), title: album.title, by, subtitle, art, radio: None }), tracks(list, false))
        }
        Source::Artist(id) => {
            let (artist, shelves) = tidal.artist(*id).await?;
            let art = Some((tidal::image(artist.picture.as_deref(), 480), true));
            let head =
                Head { item: Some(Item::Artist(*id)), title: artist.name, by: None, subtitle: String::new(), art, radio: Some(Source::ArtistRadio(*id)) };
            (Some(head), Body::Shelves(shelves))
        }
        Source::Playlist(id) => {
            let (playlist, list) = tidal.playlist(id).await?;
            let art = Some((tidal::image(playlist.cover.as_deref(), 640), false));
            (
                Some(Head { item: Some(Item::Playlist(id.clone())), title: playlist.title, by: None, subtitle: String::new(), art, radio: None }),
                tracks(list, true),
            )
        }
        Source::Mix(mix) => {
            let head =
                Head { item: None, title: mix.title.clone(), by: None, subtitle: mix.subtitle.clone(), art: Some((mix.image.clone(), false)), radio: None };
            (Some(head), tracks(tidal.mix_tracks(&mix.id).await?, true))
        }
        Source::Tracks => (Head::title("Tracks"), tracks(tidal.favorite_tracks().await?, true)),
        Source::Albums => {
            let cards = tidal.favorite_albums().await?.into_iter().map(Card::Album).collect();
            (Head::title("Albums"), Body::Grid { cards, sorts: ALBUM_SORTS })
        }
        Source::Artists => {
            let cards = tidal.favorite_artists().await?.into_iter().map(Card::Artist).collect();
            (Head::title("Artists"), Body::Grid { cards, sorts: NAME_SORTS })
        }
        Source::Folder(id, name) => (Head::title(name.clone()), Body::Grid { cards: tidal.folder(id).await?, sorts: NAME_SORTS }),
        Source::Page(path) => {
            let (title, mut shelves) = tidal.page(path).await?;
            // A page of one list ("View all") shows as a grid or a track list under that list's name.
            let title = if title.is_empty() { shelves.first().map(|s| s.title.clone()).unwrap_or_default() } else { title };
            let body = match shelves.as_mut_slice() {
                [only] if only.tracks.is_empty() && only.links.is_empty() && !only.cards.is_empty() => {
                    Body::Grid { cards: std::mem::take(&mut only.cards), sorts: &[] }
                }
                [only] if only.cards.is_empty() && only.links.is_empty() && !only.tracks.is_empty() => tracks(std::mem::take(&mut only.tracks), true),
                _ => Body::Shelves(shelves),
            };
            (Head::title(title), body)
        }
        Source::TrackRadio(id) => (Head::title("Radio"), tracks(tidal.track_radio(*id).await?, true)),
        Source::ArtistRadio(id) => (Head::title("Radio"), tracks(tidal.artist_radio(*id).await?, true)),
    };
    Ok(Page { source, head, body, view: None })
}
