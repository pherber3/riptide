use anyhow::Result;

use super::ROOT;
use crate::tidal::{Card, Item, Mix, Shelf, Tidal, Track};
use crate::view::{Sort, View};
use crate::widgets::art;

const ALBUM_SORTS: &[Sort] = &[Sort::Added, Sort::Title, Sort::Artist, Sort::Year];
const NAME_SORTS: &[Sort] = &[Sort::Added, Sort::Title];

/// Tidal content the app can open as a page or play.
#[derive(Clone, PartialEq)]
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
    Settings,
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
}

/// A page's title, artwork (with whether it is round) and radio.
pub struct Head {
    pub kind: &'static str,
    /// What the header's save button saves, or for the user's own playlist, what its menu changes.
    pub item: Option<Item>,
    pub title: String,
    pub subtitle: String,
    pub art: Option<(Option<String>, bool)>,
    pub radio: Option<Source>,
}

impl Head {
    fn title(title: impl Into<String>) -> Option<Self> {
        Some(Self { kind: "", item: None, title: title.into(), subtitle: String::new(), art: None, radio: None })
    }
}

pub enum Body {
    Tracks { tracks: Vec<Track>, album_column: bool },
    Grid { cards: Vec<Card>, sorts: &'static [Sort] },
    Shelves(Vec<Shelf>),
    /// Drawn by the app from its own state rather than loaded.
    Settings,
}

impl Body {
    /// What the page's Play and Shuffle buttons play.
    pub fn tracks(&self) -> &[Track] {
        match self {
            Self::Tracks { tracks, .. } => tracks,
            Self::Shelves(shelves) => shelves.iter().find(|s| !s.tracks.is_empty()).map_or(&[], |s| &s.tracks),
            Self::Grid { .. } | Self::Settings => &[],
        }
    }

    pub fn into_tracks(self) -> Vec<Track> {
        match self {
            Self::Tracks { tracks, .. } => tracks,
            Self::Shelves(shelves) => shelves.into_iter().map(|s| s.tracks).find(|t| !t.is_empty()).unwrap_or_default(),
            Self::Grid { .. } | Self::Settings => Vec::new(),
        }
    }
}

pub struct Page {
    pub source: Source,
    pub head: Option<Head>,
    pub body: Body,
    /// Filter and sort, on pages that have them.
    pub view: Option<View>,
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
            let subtitle = [album.artist.as_str(), &album.year].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
            let art = Some((art(album.cover.as_deref(), 640), false));
            (Some(Head { kind: "ALBUM", item: Some(Item::Album(*id)), title: album.title, subtitle, art, radio: None }), tracks(list, false))
        }
        Source::Artist(id) => {
            let (artist, shelves) = tidal.artist(*id).await?;
            let art = Some((art(artist.picture.as_deref(), 480), true));
            let head = Head { kind: "ARTIST", item: Some(Item::Artist(*id)), title: artist.name, subtitle: String::new(), art, radio: Some(Source::ArtistRadio(*id)) };
            (Some(head), Body::Shelves(shelves))
        }
        Source::Playlist(id) => {
            let (playlist, list) = tidal.playlist(id).await?;
            let art = Some((art(playlist.cover.as_deref(), 640), false));
            let subtitle = format!("{} tracks", playlist.count);
            (Some(Head { kind: "PLAYLIST", item: Some(Item::Playlist(id.clone())), title: playlist.title, subtitle, art, radio: None }), tracks(list, true))
        }
        Source::Mix(mix) => {
            let head = Head { kind: "MIX", item: None, title: mix.title.clone(), subtitle: mix.subtitle.clone(), art: Some((mix.image.clone(), false)), radio: None };
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
        Source::Settings => (Head::title("Settings"), Body::Settings),
        Source::Page(path) => {
            let (title, mut shelves) = tidal.page(path).await?;
            // A page of one list ("View all") shows as a grid or a track list under that list's name.
            let title = if title.is_empty() { shelves.first().map(|s| s.title.clone()).unwrap_or_default() } else { title };
            let body = match shelves.as_mut_slice() {
                [only] if only.tracks.is_empty() && only.links.is_empty() && !only.cards.is_empty() => Body::Grid { cards: std::mem::take(&mut only.cards), sorts: &[] },
                [only] if only.cards.is_empty() && only.links.is_empty() && !only.tracks.is_empty() => tracks(std::mem::take(&mut only.tracks), true),
                _ => Body::Shelves(shelves),
            };
            (Head::title(title), body)
        }
        Source::TrackRadio(id) => (Head::title("Radio"), tracks(tidal.radio("tracks", *id).await?, true)),
        Source::ArtistRadio(id) => (Head::title("Radio"), tracks(tidal.radio("artists", *id).await?, true)),
    };
    Ok(Page { source, head, body, view: None })
}
