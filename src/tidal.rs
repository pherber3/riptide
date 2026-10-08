use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const V1: &str = "https://api.tidal.com/v1";
const V2: &str = "https://api.tidal.com/v2";
/// The id of the top level of the user's playlist folders.
pub const ROOT: &str = "root";
const TOKEN: &str = "https://auth.tidal.com/v1/oauth2/token";
const REDIRECT: &str = "https://tidal.com/android/login/auth";
const SCOPE: &str = "r_usr+w_usr+w_sub";
/// Tidal's Android app credentials ("id;secret"), as tidlers and python-tidalapi use; needed for hi-res.
const CLIENT: &str = "NkJEU1JkcEs5aHFFQlRnVTt4ZXVQbVk3bmJwWjlJSWJMQWNROTNzaGthMVZOaGVVQXFONkljc3pqVEc4PQ==";

/// One client for everything (API, audio, artwork), so connections are reused; timeouts turn a stalled
/// network into an error.
pub static HTTP: LazyLock<reqwest::Client> =
    LazyLock::new(|| reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).read_timeout(Duration::from_secs(20)).build().expect("HTTP client"));

/// A plain download (a stream segment, artwork), failing on an error status.
pub async fn fetch(url: impl reqwest::IntoUrl) -> reqwest::Result<bytes::Bytes> {
    HTTP.get(url).send().await?.error_for_status()?.bytes().await
}

/// Tidal's tiers: Low is AAC, High is 16-bit lossless FLAC, Max is hi-res FLAC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Quality {
    Low,
    High,
    Max,
}

impl Quality {
    pub const ALL: [Self; 3] = [Self::Max, Self::High, Self::Low];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|q| q.name().eq_ignore_ascii_case(name))
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::High => "High",
            Self::Max => "Max",
        }
    }

    /// What the tier streams.
    pub fn about(self) -> &'static str {
        match self {
            Self::Max => "Up to 24-bit, 192 kHz FLAC",
            Self::High => "16-bit, 44.1 kHz FLAC (CD quality)",
            Self::Low => "AAC, 320 kbps",
        }
    }

    fn api(self) -> &'static str {
        match self {
            Self::Low => "HIGH",
            Self::High => "LOSSLESS",
            Self::Max => "HI_RES",
        }
    }
}

/// Where a stream's bytes are: whole files, or DASH segments `start..start + count` (count 0 when unknown).
pub enum Parts {
    Urls(Vec<String>),
    Segments { init: String, template: String, start: u32, count: u32 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<u64>,
    pub album: String,
    pub album_id: Option<u64>,
    pub cover: Option<String>,
    pub duration: u32,
    /// When it was added to the playlist or collection, as YYYY-MM-DD.
    pub added: Option<String>,
    /// The volume scale that brings it to Tidal's reference loudness without clipping.
    #[serde(default)]
    pub gain: Option<f32>,
}

/// One release of an album. Tidal lists each edition of an album (Max, High, Dolby Atmos, explicit
/// or clean) as a release of its own.
#[derive(Clone, Debug)]
pub struct Album {
    pub id: u64,
    pub title: String,
    /// What sets the release apart beyond its edition, such as "Deluxe".
    pub version: String,
    pub artist: String,
    pub artist_id: Option<u64>,
    pub cover: Option<String>,
    /// The release date, as YYYY-MM-DD.
    pub released: String,
    pub edition: Edition,
}

impl Album {
    pub fn year(&self) -> &str {
        self.released.get(..4).unwrap_or_default()
    }

    /// Another edition of the same release: the same artist, title, version and date.
    pub fn same_release(&self, other: &Album) -> bool {
        (self.artist_id, &self.title, &self.version, &self.released) == (other.artist_id, &other.title, &other.version, &other.released)
    }
}

/// What tells an album's editions apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edition {
    /// The best tier it streams in.
    pub quality: Quality,
    /// A Dolby Atmos edition. Tidal streams it to desktop apps as the album's stereo master, so it
    /// plays like a Max edition, but a stereo edition is the one to show when there is one.
    pub atmos: bool,
    pub explicit: bool,
}

impl Edition {
    fn of(v: &Value) -> Self {
        let tags: Vec<_> = each(&v["mediaMetadata"]["tags"]).filter_map(Value::as_str).collect();
        let atmos = !tags.contains(&"LOSSLESS") && tags.contains(&"DOLBY_ATMOS");
        let quality = match () {
            _ if atmos || tags.contains(&"HIRES_LOSSLESS") => Quality::Max,
            _ if tags.contains(&"LOSSLESS") => Quality::High,
            _ => Quality::Low,
        };
        Self { quality, atmos, explicit: v["explicit"].as_bool().unwrap_or(false) }
    }

    /// The order editions are chosen in: stereo before Atmos, the best tier, then explicit before
    /// clean, as the artist released it.
    pub fn rank(self) -> (bool, std::cmp::Reverse<Quality>, bool) {
        (self.atmos, std::cmp::Reverse(self.quality), !self.explicit)
    }

    /// Its name, in the tiers the player shows: "Max · Explicit", saying explicit or clean only
    /// when that tells the editions apart.
    pub fn name(self, explicit_varies: bool) -> String {
        let tier = self.quality.name();
        match (explicit_varies, self.explicit) {
            (false, _) => tier.into(),
            (true, true) => format!("{tier} · Explicit"),
            (true, false) => format!("{tier} · Clean"),
        }
    }
}

/// One card per release: of an album's editions, the card keeps the one to open (see `Edition::rank`).
fn one_per_release(cards: Vec<Card>) -> Vec<Card> {
    let mut out: Vec<Card> = Vec::with_capacity(cards.len());
    for card in cards {
        if let Card::Album(album) = &card
            && let Some(Card::Album(kept)) = out.iter_mut().find(|c| matches!(c, Card::Album(k) if k.same_release(album)))
        {
            if album.edition.rank() < kept.edition.rank() {
                *kept = album.clone();
            }
            continue;
        }
        out.push(card);
    }
    out
}

#[derive(Clone, Debug)]
pub struct Artist {
    pub id: u64,
    pub name: String,
    pub picture: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub cover: Option<String>,
    pub count: u64,
}

/// Lyrics: timed lines when Tidal has them synced, else just the text; both empty when there are none.
#[derive(Clone, Debug, Default)]
pub struct Lyrics {
    pub synced: Vec<(f64, String)>,
    pub text: String,
}

/// A Tidal mix (a personal radio station); its artwork is a full URL.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mix {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub image: Option<String>,
}

/// A playlist or folder among the user's playlist folders, by id.
pub enum Entry<'a> {
    Playlist(&'a str),
    Folder(&'a str),
}

impl Entry<'_> {
    fn trn(&self) -> String {
        match self {
            Self::Playlist(id) => format!("trn:playlist:{id}"),
            Self::Folder(id) => format!("trn:folder:{id}"),
        }
    }
}

/// Something the user can save to their collection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Item {
    Track(u64),
    Album(u64),
    Artist(u64),
    Playlist(String),
}

impl Item {
    /// Its share link, as Tidal's own Share gives: a page that offers to open it in the app or in
    /// the browser.
    pub fn link(&self) -> String {
        match self {
            Self::Track(id) => format!("https://tidal.com/track/{id}/u"),
            Self::Album(id) => format!("https://tidal.com/album/{id}/u"),
            Self::Artist(id) => format!("https://tidal.com/artist/{id}/u"),
            Self::Playlist(id) => format!("https://tidal.com/playlist/{id}/u"),
        }
    }
}

/// Anything shown as a card: in a grid, on a shelf, or in a playlist folder.
#[derive(Clone, Debug)]
pub enum Card {
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
    Mix(Mix),
    Folder { id: String, name: String, count: u64 },
}

/// One row of a browse page: cards, tracks, links to other pages, or a paragraph.
#[derive(Clone, Debug, Default)]
pub struct Shelf {
    pub title: String,
    pub cards: Vec<Card>,
    pub tracks: Vec<Track>,
    /// Other pages, as (title, path for `Tidal::page`): genres, moods and so on.
    pub links: Vec<(String, String)>,
    /// The page with all of it ("View all").
    pub more: Option<String>,
    /// A paragraph, such as an artist's bio.
    pub text: String,
}

impl Shelf {
    fn named(title: impl Into<String>) -> Self {
        Self { title: title.into(), ..Default::default() }
    }

    /// Adds an item of a feed or page by its type name (TRACK, ALBUM, ...).
    fn add(&mut self, kind: &str, v: &Value) {
        match kind {
            "TRACK" => self.tracks.extend(track(v)),
            "ALBUM" => self.cards.extend(album(v).map(Card::Album)),
            "ARTIST" => self.cards.extend(artist(v).map(Card::Artist)),
            "PLAYLIST" => self.cards.extend(playlist(v).map(Card::Playlist)),
            "MIX" => self.cards.extend(mix(v).map(Card::Mix)),
            _ => {}
        }
    }

    fn is_empty(&self) -> bool {
        self.cards.is_empty() && self.tracks.is_empty() && self.links.is_empty() && self.text.is_empty()
    }

    /// The shelf as it shows, with one card per release, if it has anything to show.
    fn done(mut self) -> Option<Self> {
        self.cards = one_per_release(self.cards);
        (!self.is_empty()).then_some(self)
    }

    pub fn tracks(title: &str, tracks: Vec<Track>) -> Self {
        Self { tracks, ..Self::named(title) }
    }

    pub fn cards(title: &str, cards: Vec<Card>) -> Self {
        Self { cards: one_per_release(cards), ..Self::named(title) }
    }
}

/// The URL of a Tidal image, if there is one. Albums come in 80/160/320/640/1280, artists in 160/320/480/750.
pub fn image(id: Option<&str>, size: u32) -> Option<String> {
    id.map(|id| format!("https://resources.tidal.com/images/{}/{size}x{size}.jpg", id.replace('-', "/")))
}

/// Where the sign-in is kept, in the app's data directory.
pub fn session_path(data: &Path) -> PathBuf {
    data.join("session.json")
}

fn client() -> (String, String) {
    let decoded = String::from_utf8(STANDARD.decode(CLIENT).expect("client")).expect("client");
    let (id, secret) = decoded.split_once(';').expect("client");
    (id.into(), secret.into())
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Serialize, Deserialize)]
struct Session {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    user_id: u64,
    country: String,
}

impl Session {
    /// From a token response; a refresh keeps the refresh token it was made with.
    fn from_token(v: &Value, refresh_token: Option<&str>) -> Result<Self> {
        Ok(Self {
            access_token: v["access_token"].as_str().context("no access token")?.into(),
            refresh_token: v["refresh_token"].as_str().or(refresh_token).context("no refresh token")?.into(),
            expires_at: now() + v["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60),
            user_id: v["user"]["userId"].as_u64().or(v["user_id"].as_u64()).context("no user")?,
            country: v["user"]["countryCode"].as_str().unwrap_or("US").into(),
        })
    }
}

async fn token(form: &[(&str, &str)]) -> Result<Value> {
    let (id, secret) = client();
    let form = [form, &[("client_id", id.as_str()), ("scope", SCOPE)]].concat();
    let resp = HTTP.post(TOKEN).basic_auth(&id, Some(&secret)).form(&form).send().await?;
    Ok(resp.error_for_status()?.json().await?)
}

/// A browser sign-in in progress (OAuth PKCE): open `url`, then pass the address it lands on to `finish`.
pub struct Login {
    verifier: String,
    unique_key: String,
    pub url: String,
}

impl Login {
    pub fn start() -> Self {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).expect("random bytes");
        let verifier = URL_SAFE_NO_PAD.encode(random);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let unique_key = format!("{:016x}", u64::from_le_bytes(random[..8].try_into().expect("8 bytes")));
        let query = [
            ("response_type", "code"),
            ("redirect_uri", REDIRECT),
            ("client_id", &client().0),
            ("lang", "EN"),
            ("appMode", "android"),
            ("client_unique_key", &unique_key),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("restrict_signup", "true"),
        ];
        let url = reqwest::Url::parse_with_params("https://login.tidal.com/authorize", &query).expect("login URL").into();
        Self { verifier, unique_key, url }
    }

    pub async fn finish(self, landed: &str, path: &Path) -> Result<Tidal> {
        let landed = reqwest::Url::parse(landed.trim()).context("that isn't a web address")?;
        let code = landed.query_pairs().find(|(k, _)| k == "code").context("no sign-in code in that address")?.1;
        let form = [
            ("code", &*code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", REDIRECT),
            ("code_verifier", &self.verifier),
            ("client_unique_key", &self.unique_key),
        ];
        let session = Session::from_token(&token(&form).await?, None)?;
        crate::json::save(path, &session)?;
        Ok(Tidal::new(session, path))
    }
}

/// A signed-in Tidal account. Cheap to clone and use from many tasks at once: only the token is
/// behind a lock, and only while it is read or refreshed.
#[derive(Clone)]
pub struct Tidal {
    session: Arc<tokio::sync::Mutex<Session>>,
    path: Arc<PathBuf>,
}

impl Tidal {
    fn new(session: Session, path: &Path) -> Self {
        Self { session: Arc::new(tokio::sync::Mutex::new(session)), path: Arc::new(path.into()) }
    }

    pub async fn load(path: &Path) -> Result<Self> {
        let session = crate::json::load(path).context("not signed in")?;
        let tidal = Self::new(session, path);
        tidal.auth().await?;
        Ok(tidal)
    }

    /// The access token and account country, refreshed (and saved) first when about to expire.
    async fn auth(&self) -> Result<(String, String, u64)> {
        let mut s = self.session.lock().await;
        if now() >= s.expires_at {
            let refreshed = token(&[("grant_type", "refresh_token"), ("refresh_token", &s.refresh_token)]).await?;
            *s = Session::from_token(&refreshed, Some(&s.refresh_token))?;
            crate::json::save(&self.path, &*s)?;
        }
        Ok((s.access_token.clone(), s.country.clone(), s.user_id))
    }

    async fn request(&self, method: Method, url: &str, query: &[(&str, &str)]) -> Result<reqwest::RequestBuilder> {
        let (token, country, _) = self.auth().await?;
        let mut req = HTTP.request(method, url).bearer_auth(token).query(&[("countryCode", country.as_str())]).query(query);
        if url.starts_with(V2) {
            req = req.header("x-tidal-client-version", "2026.1.5").query(&[("locale", "en_US"), ("deviceType", "BROWSER")]);
        }
        Ok(req)
    }

    async fn send(&self, method: Method, url: &str, query: &[(&str, &str)], form: &[(&str, &str)]) -> Result<reqwest::Response> {
        submit(self.request(method, url, query).await?, form).await
    }

    async fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<Value> {
        Ok(self.send(Method::GET, url, query, &[]).await?.json().await?)
    }

    /// Every item of a paged list (v1 or v2), up to `max`: the first page, then the rest a few at a
    /// time, each parsed as it arrives so a long list never sits in memory as raw JSON.
    async fn items<T>(&self, url: &str, query: &[(&str, &str)], max: usize, parse: impl Fn(&Value) -> Option<T>) -> Result<Vec<T>> {
        // v1 lists take pages of up to 100, v2's of 50.
        let size: usize = if url.starts_with(V2) { 50 } else { 100 };
        let (parse, limit) = (&parse, &size.to_string());
        let page = |offset: usize| async move {
            let offset = offset.to_string();
            let mut paged = vec![("limit", limit.as_str()), ("offset", offset.as_str())];
            paged.extend_from_slice(query);
            let v = self.get(url, &paged).await?;
            let items = list(&v["items"], parse);
            anyhow::Ok((v["totalNumberOfItems"].as_u64().unwrap_or(0) as usize, items))
        };
        let (total, mut items) = page(0).await?;
        let rest: Vec<_> = stream::iter((size..total.min(max)).step_by(size)).map(page).buffered(6).try_collect().await?;
        items.extend(rest.into_iter().flat_map(|(_, items)| items));
        items.truncate(max);
        Ok(items)
    }

    pub async fn stream(&self, track_id: u64, quality: Quality) -> Result<Parts> {
        let query = [("audioquality", quality.api()), ("playbackmode", "STREAM"), ("assetpresentation", "FULL")];
        let info = self.get(&format!("{V1}/tracks/{track_id}/playbackinfopostpaywall"), &query).await?;
        let manifest = String::from_utf8(STANDARD.decode(info["manifest"].as_str().context("no stream manifest")?)?)?;
        if let Ok(json) = serde_json::from_str::<Value>(&manifest) {
            return Ok(Parts::Urls(json["urls"].as_array().context("no stream URLs")?.iter().map(text).collect()));
        }
        dash(&manifest).context("unreadable stream manifest")
    }

    /// Search results as shelves: tracks, then artists, albums and playlists.
    pub async fn search(&self, query: &str) -> Result<Vec<Shelf>> {
        let types = "ARTISTS,ALBUMS,TRACKS,PLAYLISTS";
        let v = self.get(&format!("{V1}/search"), &[("query", query), ("types", types), ("limit", "20")]).await?;
        let cards = |key: &str, parse: fn(&Value) -> Option<Card>| list(&v[key]["items"], parse);
        // Tidal's best match: an artist, album or playlist gets a shelf of its own; a track goes first.
        let hit = &v["topHit"]["value"];
        let top = match v["topHit"]["type"].as_str() {
            Some("ARTISTS") => artist(hit).map(Card::Artist),
            Some("ALBUMS") => album(hit).map(Card::Album),
            Some("PLAYLISTS") => playlist(hit).map(Card::Playlist),
            _ => None,
        };
        let mut tracks = list(&v["tracks"]["items"], track);
        if let Some(at) = tracks.iter().position(|t| Some(t.id) == hit["id"].as_u64()).filter(|_| v["topHit"]["type"] == "TRACKS") {
            tracks[..=at].rotate_right(1);
        }
        let shelves = [
            Shelf::cards("Top result", top.into_iter().collect()),
            Shelf::tracks("Tracks", tracks),
            Shelf::cards("Artists", cards("artists", |v| artist(v).map(Card::Artist))),
            Shelf::cards("Albums", cards("albums", |v| album(v).map(Card::Album))),
            Shelf::cards("Playlists", cards("playlists", |v| playlist(v).map(Card::Playlist))),
        ];
        Ok(shelves.into_iter().filter(|s| !s.is_empty()).collect())
    }

    /// An album, its tracks, and its editions (itself among them), best first. Tidal doesn't link
    /// editions, so they are found among the artist's releases, while the tracks load.
    pub async fn album(&self, id: u64) -> Result<(Album, Vec<Album>, Vec<Track>)> {
        let url = format!("{V1}/albums/{id}");
        let about = async {
            let info = self.get(&url, &[]).await?;
            let this = album(&info).context("bad album")?;
            let filter: &[(&str, &str)] = match info["type"].as_str() {
                Some("EP" | "SINGLE") => &[("filter", "EPSANDSINGLES")],
                _ => &[],
            };
            let releases = match this.artist_id {
                Some(artist) => self.items(&format!("{V1}/artists/{artist}/albums"), filter, 200, album).await.unwrap_or_default(),
                None => Vec::new(),
            };
            let mut editions: Vec<Album> = releases.into_iter().filter(|r| r.same_release(&this)).collect();
            editions.sort_by_key(|e| e.edition.rank());
            anyhow::Ok((this, editions))
        };
        let items = format!("{url}/items");
        let ((album, editions), tracks) = tokio::try_join!(about, self.items(&items, &[], 1000, track))?;
        Ok((album, editions, tracks))
    }

    /// An artist and their page: top tracks, releases by kind, similar artists and bio.
    pub async fn artist(&self, id: u64) -> Result<(Artist, Vec<Shelf>)> {
        let url = format!("{V1}/artists/{id}");
        let (top_url, albums_url, similar_url, bio_url) = (format!("{url}/toptracks"), format!("{url}/albums"), format!("{url}/similar"), format!("{url}/bio"));
        let releases = |filter| self.items(&albums_url, filter, 200, |v| album(v).map(Card::Album));
        let (info, top, albums, singles, compilations, similar, bio) = tokio::join!(
            self.get(&url, &[]),
            self.get(&top_url, &[("limit", "10")]),
            releases(&[]),
            releases(&[("filter", "EPSANDSINGLES")]),
            releases(&[("filter", "COMPILATIONS")]),
            self.get(&similar_url, &[("limit", "20")]),
            self.get(&bio_url, &[]),
        );
        let shelves = [
            Shelf::tracks("Top tracks", top.map(|v| list(&v["items"], track)).unwrap_or_default()),
            Shelf::cards("Albums", albums?),
            Shelf::cards("EPs & Singles", singles.unwrap_or_default()),
            Shelf::cards("Compilations", compilations.unwrap_or_default()),
            Shelf::cards("Fans also like", similar.map(|v| list(&v["items"], |v| artist(v).map(Card::Artist))).unwrap_or_default()),
            Shelf { text: bio.map(|v| plain(&text(&v["text"]))).unwrap_or_default(), ..Shelf::named("About") },
        ];
        Ok((artist(&info?).context("bad artist")?, shelves.into_iter().filter(|s| !s.is_empty()).collect()))
    }

    pub async fn playlist(&self, id: &str) -> Result<(Playlist, Vec<Track>)> {
        let url = format!("{V1}/playlists/{id}");
        let items = format!("{url}/items");
        let (info, tracks) = tokio::try_join!(self.get(&url, &[]), self.items(&items, &[], 10_000, track))?;
        Ok((playlist(&info).context("bad playlist")?, tracks))
    }

    /// Tidal's personal home feed: recently played, your top playlists, mixes and the rest.
    pub async fn home(&self) -> Result<Vec<Shelf>> {
        let feed = self.get(&format!("{V2}/home/feed/static"), &[("platform", "WEB"), ("limit", "20")]).await?;
        let shelves = each(&feed["items"]).filter_map(|module| {
            let mut shelf = Shelf { more: module["viewAll"].as_str().map(Into::into), ..Shelf::named(text(&module["title"])) };
            for item in module["items"].as_array()? {
                shelf.add(item["type"].as_str()?, &item["data"]);
            }
            shelf.done()
        });
        Ok(shelves.collect())
    }

    /// A browse page and its title: a home row's "view all" (`home/...`), or one of Tidal's
    /// editorial pages (`pages/...`), such as Explore, a genre or a mood.
    pub async fn page(&self, path: &str) -> Result<(String, Vec<Shelf>)> {
        if path.starts_with("home/") {
            let v = self.get(&format!("{V2}/{path}"), &[("platform", "WEB"), ("limit", "50")]).await?;
            let mut shelf = Shelf::default();
            for item in each(&v["items"]) {
                shelf.add(item["type"].as_str().unwrap_or_default(), &item["data"]);
            }
            return Ok((text(&v["title"]), vec![shelf]));
        }
        let v = self.get(&format!("{V1}/{path}"), &[("deviceType", "BROWSER"), ("locale", "en_US")]).await?;
        let modules = each(&v["rows"]).flat_map(|row| each(&row["modules"]));
        let shelves = modules.filter_map(|m| {
            let kind = m["type"].as_str()?;
            let mut shelf = Shelf { more: m["showMore"]["apiPath"].as_str().map(Into::into), ..Shelf::named(text(&m["title"])) };
            for item in m["pagedList"]["items"].as_array().or(m["items"].as_array())? {
                match kind {
                    "PAGE_LINKS_CLOUD" | "PAGE_LINKS" => shelf.links.extend(item["apiPath"].as_str().map(|p| (text(&item["title"]), p.into()))),
                    "MIXED_TYPES_LIST" => shelf.add(item["type"].as_str().unwrap_or_default(), &item["item"]),
                    _ => shelf.add(kind.trim_end_matches("_LIST"), item),
                }
            }
            shelf.done()
        });
        Ok((text(&v["title"]), shelves.collect()))
    }

    /// Who made a track, as (role, names).
    pub async fn credits(&self, id: u64) -> Result<Vec<(String, String)>> {
        let v = self.get(&format!("{V1}/tracks/{id}/credits"), &[]).await?;
        let role = |c: &Value| {
            let names: Vec<String> = c["contributors"].as_array()?.iter().map(|p| text(&p["name"])).collect();
            Some((text(&c["type"]), names.join(", ")))
        };
        Ok(list(&v, role))
    }

    pub async fn mix_tracks(&self, id: &str) -> Result<Vec<Track>> {
        self.items(&format!("{V1}/mixes/{id}/items"), &[], 1000, track).await
    }

    /// Tracks like this one, for radio.
    pub async fn track_radio(&self, id: u64) -> Result<Vec<Track>> {
        self.radio("tracks", id).await
    }

    /// Tracks like this artist's, for radio.
    pub async fn artist_radio(&self, id: u64) -> Result<Vec<Track>> {
        self.radio("artists", id).await
    }

    async fn radio(&self, kind: &str, id: u64) -> Result<Vec<Track>> {
        Ok(list(&self.get(&format!("{V1}/{kind}/{id}/radio"), &[("limit", "100")]).await?["items"], track))
    }

    pub async fn lyrics(&self, id: u64) -> Result<Lyrics> {
        let v = match self.get(&format!("{V1}/tracks/{id}/lyrics"), &[]).await {
            Ok(v) => v,
            Err(e) if e.downcast_ref::<reqwest::Error>().and_then(reqwest::Error::status) == Some(reqwest::StatusCode::NOT_FOUND) => {
                return Ok(Lyrics::default());
            }
            Err(e) => return Err(e),
        };
        Ok(Lyrics { synced: v["subtitles"].as_str().map_or_else(Vec::new, lrc), text: text(&v["lyrics"]) })
    }

    async fn user(&self) -> Result<String> {
        Ok(format!("{V1}/users/{}", self.auth().await?.2))
    }

    /// A favorites list, newest first; entries are `{"created": ..., "item": {...}}`.
    async fn favorites<T>(&self, kind: &str, parse: impl Fn(&Value) -> Option<T>) -> Result<Vec<T>> {
        let url = format!("{}/favorites/{kind}", self.user().await?);
        self.items(&url, &[("order", "DATE"), ("orderDirection", "DESC")], 10_000, parse).await
    }

    pub async fn favorite_tracks(&self) -> Result<Vec<Track>> {
        self.favorites("tracks", track).await
    }

    pub async fn favorite_albums(&self) -> Result<Vec<Album>> {
        self.favorites("albums", |v| album(&v["item"])).await
    }

    pub async fn favorite_artists(&self) -> Result<Vec<Artist>> {
        self.favorites("artists", |v| artist(&v["item"])).await
    }

    /// A playlist folder's folders and playlists, newest first; "root" is the top level.
    pub async fn folder(&self, id: &str) -> Result<Vec<Card>> {
        let query = [("folderId", id), ("includeOnly", ""), ("order", "DATE"), ("orderDirection", "DESC")];
        let entry = |item: &Value| match item["itemType"].as_str()? {
            "FOLDER" => {
                Some(Card::Folder { id: text(&item["data"]["id"]), name: text(&item["name"]), count: item["data"]["totalNumberOfItems"].as_u64().unwrap_or(0) })
            }
            "PLAYLIST" => playlist(&item["data"]).map(Card::Playlist),
            _ => None,
        };
        self.items(&format!("{V2}/my-collection/playlists/folders"), &query, 1000, entry).await
    }

    /// The playlists the user made (the ones they can add to), most recently changed first.
    pub async fn my_playlists(&self) -> Result<Vec<Playlist>> {
        let user = self.auth().await?.2;
        let url = format!("{V2}/my-collection/playlists/folders/flattened");
        let (mut playlists, mut cursor) = (Vec::new(), String::new());
        loop {
            let query = [("includeOnly", "PLAYLIST"), ("limit", "50"), ("order", "DATE_UPDATED"), ("orderDirection", "DESC"), ("cursor", cursor.as_str())];
            let mut page = self.get(&url, &query).await?;
            let mine = |item: &&Value| item["data"]["creator"]["id"].as_u64() == Some(user);
            playlists.extend(each(&page["items"]).filter(mine).filter_map(|item| playlist(&item["data"])));
            match page["cursor"].take() {
                Value::String(next) if !next.is_empty() => cursor = next,
                _ => return Ok(playlists),
            }
        }
    }

    /// Makes a playlist at the top level.
    pub async fn create_playlist(&self, name: &str, description: &str, public: bool) -> Result<Playlist> {
        let public = public.to_string();
        let query = [("name", name), ("description", description), ("folderId", ROOT), ("isPublic", public.as_str())];
        let v: Value = self.folders("create-playlist", &query).await?.json().await?;
        playlist(&v["data"]).context("Tidal didn't return the new playlist")
    }

    /// Appends a track to one of the user's playlists. Tidal asks for the playlist's current version.
    pub async fn add_to_playlist(&self, playlist: &str, track: u64) -> Result<()> {
        let form = [("trackIds", track.to_string()), ("onDupes", "SKIP".into()), ("onArtifactNotFound", "SKIP".into())];
        self.edit(playlist, Method::POST, "/items", &[], &form).await
    }

    /// Removes the track at `index` (in the playlist's own order).
    pub async fn remove_from_playlist(&self, playlist: &str, index: usize) -> Result<()> {
        let query = [("order", "INDEX"), ("orderDirection", "ASC")];
        self.edit(playlist, Method::DELETE, &format!("/items/{index}"), &query, &[]).await
    }

    pub async fn rename_playlist(&self, playlist: &str, title: &str) -> Result<()> {
        self.edit(playlist, Method::POST, "", &[], &[("title", title.into())]).await
    }

    /// Changes one of the user's playlists. Tidal asks for the version being changed.
    async fn edit(&self, playlist: &str, method: Method, path: &str, query: &[(&str, &str)], form: &[(&str, String)]) -> Result<()> {
        let url = format!("{V1}/playlists/{playlist}");
        let current = self.send(Method::GET, &url, &[], &[]).await?;
        let version = current.headers().get("etag").context("no playlist version")?.clone();
        current.bytes().await?; // read to the end so the connection is reused
        let req = self.request(method, &format!("{url}{path}"), query).await?.header("If-None-Match", version);
        submit(req, form).await.map(drop)
    }

    /// Moves a playlist or folder into a folder (`ROOT` is the top level).
    pub async fn move_entry(&self, entry: Entry<'_>, folder: &str) -> Result<()> {
        self.folders("move", &[("trns", &entry.trn()), ("folderId", folder)]).await.map(drop)
    }

    /// Takes a playlist or folder out of the user's folders; a playlist the user made is deleted.
    pub async fn remove_entry(&self, entry: Entry<'_>) -> Result<()> {
        self.folders("remove", &[("trns", &entry.trn())]).await.map(drop)
    }

    /// One of the playlist-folder calls (create, rename, move, remove), all PUTs on v2.
    async fn folders(&self, action: &str, query: &[(&str, &str)]) -> Result<reqwest::Response> {
        self.send(Method::PUT, &format!("{V2}/my-collection/playlists/folders/{action}"), query, &[]).await
    }

    /// Moves the track at `from` so it ends up at `to` (positions in the playlist's own order).
    pub async fn move_in_playlist(&self, playlist: &str, from: usize, to: usize) -> Result<()> {
        self.edit(playlist, Method::POST, &format!("/items/{from}"), &[], &[("toIndex", to.to_string())]).await
    }

    pub async fn create_folder(&self, name: &str) -> Result<()> {
        self.folders("create-folder", &[("name", name), ("folderId", ROOT)]).await.map(drop)
    }

    pub async fn rename_folder(&self, id: &str, name: &str) -> Result<()> {
        self.folders("rename", &[("trn", &Entry::Folder(id).trn()), ("name", name)]).await.map(drop)
    }

    /// Deletes a folder, first moving what is in it to the top level so no playlist goes with it.
    pub async fn delete_folder(&self, id: &str) -> Result<()> {
        let cards = self.folder(id).await?;
        let moves = cards.iter().filter_map(|card| match card {
            Card::Playlist(p) => Some(self.move_entry(Entry::Playlist(&p.id), ROOT)),
            Card::Folder { id, .. } => Some(self.move_entry(Entry::Folder(id), ROOT)),
            _ => None,
        });
        futures_util::future::try_join_all(moves).await?;
        self.remove_entry(Entry::Folder(id)).await
    }

    /// Everything saved to the collection: tracks, albums, artists and playlists.
    pub async fn saved(&self) -> Result<HashSet<Item>> {
        let v = self.get(&format!("{}/favorites/ids", self.user().await?), &[]).await?;
        let ids = |kind: &str| each(&v[kind]).filter_map(Value::as_str);
        let number = |id: &str| id.parse().ok();
        let mut saved: HashSet<Item> = ids("TRACK").filter_map(number).map(Item::Track).collect();
        saved.extend(ids("ALBUM").filter_map(number).map(Item::Album));
        saved.extend(ids("ARTIST").filter_map(number).map(Item::Artist));
        saved.extend(ids("PLAYLIST").map(|id| Item::Playlist(id.into())));
        Ok(saved)
    }

    /// Saves something to the collection, or takes it out.
    pub async fn set_saved(&self, item: &Item, on: bool) -> Result<()> {
        let (kind, field, id) = match item {
            Item::Track(id) => ("tracks", "trackIds", id.to_string()),
            Item::Album(id) => ("albums", "albumIds", id.to_string()),
            Item::Artist(id) => ("artists", "artistIds", id.to_string()),
            Item::Playlist(id) => ("playlists", "uuids", id.clone()),
        };
        let url = format!("{}/favorites/{kind}", self.user().await?);
        match on {
            true => self.send(Method::POST, &url, &[], &[(field, &id)]).await,
            false => self.send(Method::DELETE, &format!("{url}/{id}"), &[], &[]).await,
        }
        .map(drop)
    }
}

/// A DASH manifest's segment template, and its segment count from the timeline.
fn dash(xml: &str) -> Option<Parts> {
    let attr = |tag: &str, name: &str| {
        let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        Some(tag[at..].split('"').next()?.replace("&amp;", "&"))
    };
    let count = xml
        .split("<S ")
        .skip(1)
        .map(|s| 1 + attr(&format!(" {}", s.split('>').next().unwrap_or_default()), "r").and_then(|r| r.parse::<u32>().ok()).unwrap_or(0))
        .sum();
    Some(Parts::Segments {
        init: attr(xml, "initialization")?,
        template: attr(xml, "media")?,
        start: attr(xml, "startNumber").and_then(|n| n.parse().ok()).unwrap_or(1),
        count,
    })
}

/// `[mm:ss.xx] words` lines.
fn lrc(subtitles: &str) -> Vec<(f64, String)> {
    let line = |l: &str| {
        let (stamp, words) = l.trim().strip_prefix('[')?.split_once(']')?;
        let (minutes, seconds) = stamp.split_once(':')?;
        Some((minutes.parse::<f64>().ok()? * 60.0 + seconds.parse::<f64>().ok()?, words.trim().to_string()))
    };
    subtitles.lines().filter_map(line).collect()
}

/// Sends a request, with a form body when there is one, and fails on an error status.
async fn submit(req: reqwest::RequestBuilder, form: &[(&str, impl serde::Serialize)]) -> Result<reqwest::Response> {
    let req = if form.is_empty() { req } else { req.form(form) };
    Ok(req.send().await?.error_for_status()?)
}

/// The elements of a JSON array; none for anything else.
fn each(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}

fn list<T>(v: &Value, parse: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    each(v).filter_map(parse).collect()
}

/// Tidal's text without its link markup (`[wimpLink artistId="1"]Name[/wimpLink]`).
fn plain(marked: &str) -> String {
    let mut out = String::with_capacity(marked.len());
    let mut rest = marked;
    while let Some(start) = [rest.find("[wimpLink"), rest.find("[/wimpLink]")].into_iter().flatten().min() {
        out.push_str(&rest[..start]);
        rest = rest[start..].split_once(']').map_or("", |(_, after)| after);
    }
    out.push_str(rest);
    out.replace("<br/>", "\n")
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn image_id(v: &Value) -> Option<String> {
    v.as_str().map(Into::into)
}

fn date(v: &Value) -> Option<String> {
    v.as_str().map(|d| d.chars().take(10).collect())
}

/// A track, or an item wrapping one (`{"item": {...}, "type": "track"}` or a favorite's `{"created": ..., "item": {...}}`);
/// videos are skipped.
fn track(v: &Value) -> Option<Track> {
    if v["item"].is_object() {
        if v["type"].as_str().is_some_and(|t| !t.eq_ignore_ascii_case("track")) {
            return None;
        }
        let mut t = track(&v["item"])?;
        t.added = date(&v["dateAdded"]).or_else(|| date(&v["created"])).or(t.added);
        return Some(t);
    }
    let mut title = text(&v["title"]);
    if let Some(version) = v["version"].as_str().filter(|s| !s.is_empty()) {
        title = format!("{title} ({version})");
    }
    let names: Vec<String> = v["artists"].as_array()?.iter().map(|a| text(&a["name"])).collect();
    Some(Track {
        id: v["id"].as_u64()?,
        title,
        artist: names.join(", "),
        artist_id: v["artists"][0]["id"].as_u64(),
        album: text(&v["album"]["title"]),
        album_id: v["album"]["id"].as_u64(),
        cover: image_id(&v["album"]["cover"]),
        duration: v["duration"].as_u64().unwrap_or(0) as u32,
        added: date(&v["dateAdded"]),
        gain: v["replayGain"].as_f64().map(|db| 10f64.powf(db / 20.0).min(1.0 / v["peak"].as_f64().unwrap_or(1.0).max(0.01)) as f32),
    })
}

fn album(v: &Value) -> Option<Album> {
    let artist = if v["artist"].is_object() { &v["artist"] } else { &v["artists"][0] };
    Some(Album {
        id: v["id"].as_u64()?,
        title: text(&v["title"]),
        version: text(&v["version"]),
        artist: text(&artist["name"]),
        artist_id: artist["id"].as_u64(),
        cover: image_id(&v["cover"]),
        released: text(&v["releaseDate"]),
        edition: Edition::of(v),
    })
}

fn artist(v: &Value) -> Option<Artist> {
    Some(Artist { id: v["id"].as_u64()?, name: text(&v["name"]), picture: image_id(&v["picture"]) })
}

fn mix(v: &Value) -> Option<Mix> {
    Some(Mix {
        id: v["id"].as_str()?.into(),
        title: text(&v["titleTextInfo"]["text"]),
        subtitle: text(&v["subtitleTextInfo"]["text"]),
        image: v["mixImages"][0]["url"].as_str().map(Into::into),
    })
}

fn playlist(v: &Value) -> Option<Playlist> {
    Some(Playlist {
        id: v["uuid"].as_str()?.into(),
        title: text(&v["title"]),
        cover: image_id(&v["squareImage"]).or_else(|| image_id(&v["image"])),
        count: v["numberOfTracks"].as_u64().unwrap_or(0),
    })
}
