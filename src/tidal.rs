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
const TOKEN: &str = "https://auth.tidal.com/v1/oauth2/token";
const REDIRECT: &str = "https://tidal.com/android/login/auth";
const SCOPE: &str = "r_usr+w_usr+w_sub";
/// Tidal's Android app credentials ("id;secret"), as tidlers and python-tidalapi use; needed for hi-res.
const CLIENT: &str = "NkJEU1JkcEs5aHFFQlRnVTt4ZXVQbVk3bmJwWjlJSWJMQWNROTNzaGthMVZOaGVVQXFONkljc3pqVEc4PQ==";

/// One client for everything (API, audio, artwork), so connections are reused; timeouts turn a stalled
/// network into an error.
pub static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).read_timeout(Duration::from_secs(20));
    builder.build().expect("HTTP client")
});

/// Tidal's tiers: Low is AAC, High is 16-bit lossless FLAC, Max is hi-res FLAC.
#[derive(Clone, Copy, Debug, PartialEq)]
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

#[derive(Clone, Debug)]
pub struct Album {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub cover: Option<String>,
    pub year: String,
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
#[derive(Clone, Debug, PartialEq)]
pub struct Mix {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub image: Option<String>,
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

/// One row of the home page.
#[derive(Clone, Debug)]
pub struct Shelf {
    pub title: String,
    pub cards: Vec<Card>,
    pub tracks: Vec<Track>,
}

/// An image URL from a Tidal image id. Albums come in 80/160/320/640/1280, artists in 160/320/480/750.
pub fn image(id: &str, size: u32) -> String {
    format!("https://resources.tidal.com/images/{}/{size}x{size}.jpg", id.replace('-', "/"))
}

pub fn session_path(dir: &Path) -> PathBuf {
    dir.join("data").join("session.json")
}

fn client() -> (String, String) {
    let decoded = String::from_utf8(STANDARD.decode(CLIENT).expect("client")).expect("client");
    let (id, secret) = decoded.split_once(';').expect("client");
    (id.into(), secret.into())
}

fn now() -> u64 {
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
        Tidal::new(session, path)
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
    /// A new sign-in, saved for next time.
    fn new(session: Session, path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(path, serde_json::to_string(&session)?)?;
        Ok(Self { session: Arc::new(tokio::sync::Mutex::new(session)), path: Arc::new(path.into()) })
    }

    pub async fn load(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path).context("not signed in")?;
        let session = serde_json::from_str(&json).context("please sign in again")?;
        let tidal = Self { session: Arc::new(tokio::sync::Mutex::new(session)), path: Arc::new(path.into()) };
        tidal.auth().await?;
        Ok(tidal)
    }

    /// The access token and account country, refreshed (and saved) first when about to expire.
    async fn auth(&self) -> Result<(String, String, u64)> {
        let mut s = self.session.lock().await;
        if now() >= s.expires_at {
            let refreshed = token(&[("grant_type", "refresh_token"), ("refresh_token", &s.refresh_token)]).await?;
            *s = Session::from_token(&refreshed, Some(&s.refresh_token))?;
            std::fs::write(&*self.path, serde_json::to_string(&*s)?)?;
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
        let mut req = self.request(method, url, query).await?;
        if !form.is_empty() {
            req = req.form(form);
        }
        Ok(req.send().await?.error_for_status()?)
    }

    async fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<Value> {
        Ok(self.send(Method::GET, url, query, &[]).await?.json().await?)
    }

    /// Every item of a paged list (v1 or v2), up to `max`: the first page, then the rest a few at a time.
    async fn items<T>(&self, url: &str, query: &[(&str, &str)], max: usize, parse: impl Fn(&Value) -> Option<T>) -> Result<Vec<T>> {
        const PAGE: usize = 50;
        let page = |offset: usize| async move {
            let offset = offset.to_string();
            let mut paged = vec![("limit", "50"), ("offset", offset.as_str())];
            paged.extend_from_slice(query);
            self.get(url, &paged).await
        };
        let first = page(0).await?;
        let total = first["totalNumberOfItems"].as_u64().unwrap_or(0) as usize;
        let rest: Vec<Value> = stream::iter((PAGE..total.min(max)).step_by(PAGE)).map(page).buffered(6).try_collect().await?;
        let items = std::iter::once(first).chain(rest).flat_map(|mut page| match page["items"].take() {
            Value::Array(items) => items,
            _ => Vec::new(),
        });
        Ok(items.filter_map(|item| parse(&item)).take(max).collect())
    }

    pub async fn stream(&self, track_id: u64, quality: Quality) -> Result<Parts> {
        let query = [("audioquality", quality.api()), ("playbackmode", "STREAM"), ("assetpresentation", "FULL")];
        let info = self.get(&format!("{V1}/tracks/{track_id}/playbackinfopostpaywall"), &query).await?;
        let manifest = STANDARD.decode(info["manifest"].as_str().context("no stream manifest")?)?;
        let manifest = String::from_utf8(manifest)?;
        if let Ok(json) = serde_json::from_str::<Value>(&manifest) {
            return Ok(Parts::Urls(json["urls"].as_array().context("no stream URLs")?.iter().map(text).collect()));
        }
        dash(&manifest).context("unreadable stream manifest")
    }

    /// Search results as shelves: tracks, then artists, albums and playlists.
    pub async fn search(&self, query: &str) -> Result<Vec<Shelf>> {
        let types = "ARTISTS,ALBUMS,TRACKS,PLAYLISTS";
        let v = self.get(&format!("{V1}/search"), &[("query", query), ("types", types), ("limit", "20")]).await?;
        let shelf = |title: &str, cards: Vec<Card>, tracks| Shelf { title: title.into(), cards, tracks };
        let cards = |key: &str, parse: fn(&Value) -> Option<Card>| list(&v[key]["items"], parse);
        let shelves = [
            shelf("Tracks", Vec::new(), list(&v["tracks"]["items"], track)),
            shelf("Artists", cards("artists", |v| artist(v).map(Card::Artist)), Vec::new()),
            shelf("Albums", cards("albums", |v| album(v).map(Card::Album)), Vec::new()),
            shelf("Playlists", cards("playlists", |v| playlist(v).map(Card::Playlist)), Vec::new()),
        ];
        Ok(shelves.into_iter().filter(|s| !s.cards.is_empty() || !s.tracks.is_empty()).collect())
    }

    pub async fn album(&self, id: u64) -> Result<(Album, Vec<Track>)> {
        let url = format!("{V1}/albums/{id}");
        let items = format!("{url}/items");
        let (info, tracks) = tokio::try_join!(self.get(&url, &[]), self.items(&items, &[], 1000, track))?;
        Ok((album(&info).context("bad album")?, tracks))
    }

    pub async fn artist(&self, id: u64) -> Result<(Artist, Vec<Track>, Vec<Album>)> {
        let url = format!("{V1}/artists/{id}");
        let (top_url, albums_url) = (format!("{url}/toptracks"), format!("{url}/albums"));
        let (info, top, albums) = tokio::try_join!(
            self.get(&url, &[]),
            self.get(&top_url, &[("limit", "10")]),
            self.items(&albums_url, &[], 200, album)
        )?;
        Ok((artist(&info).context("bad artist")?, list(&top["items"], track), albums))
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
        let shelves = feed["items"].as_array().into_iter().flatten().filter_map(|module| {
            let mut shelf = Shelf { title: text(&module["title"]), cards: Vec::new(), tracks: Vec::new() };
            for item in module["items"].as_array()? {
                let data = &item["data"];
                match item["type"].as_str()? {
                    "TRACK" => shelf.tracks.extend(track(data)),
                    "ALBUM" => shelf.cards.extend(album(data).map(Card::Album)),
                    "ARTIST" => shelf.cards.extend(artist(data).map(Card::Artist)),
                    "PLAYLIST" => shelf.cards.extend(playlist(data).map(Card::Playlist)),
                    "MIX" => shelf.cards.extend(mix(data).map(Card::Mix)),
                    _ => {}
                }
            }
            (!shelf.cards.is_empty() || !shelf.tracks.is_empty()).then_some(shelf)
        });
        Ok(shelves.collect())
    }

    pub async fn mix_tracks(&self, id: &str) -> Result<Vec<Track>> {
        self.items(&format!("{V1}/mixes/{id}/items"), &[], 1000, track).await
    }

    /// Tracks like this one ("tracks") or this artist's ("artists"), for radio.
    pub async fn radio(&self, kind: &str, id: u64) -> Result<Vec<Track>> {
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
            "FOLDER" => Some(Card::Folder {
                id: text(&item["data"]["id"]),
                name: text(&item["name"]),
                count: item["data"]["totalNumberOfItems"].as_u64().unwrap_or(0),
            }),
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
            playlists.extend(page["items"].as_array().into_iter().flatten().filter(mine).filter_map(|item| playlist(&item["data"])));
            match page["cursor"].take() {
                Value::String(next) if !next.is_empty() => cursor = next,
                _ => return Ok(playlists),
            }
        }
    }

    /// Makes a private playlist at the top level.
    pub async fn create_playlist(&self, name: &str) -> Result<Playlist> {
        let query = [("name", name), ("description", ""), ("folderId", "root"), ("isPublic", "false")];
        let url = format!("{V2}/my-collection/playlists/folders/create-playlist");
        let v: Value = self.send(Method::PUT, &url, &query, &[]).await?.json().await?;
        playlist(&v["data"]).context("Tidal didn't return the new playlist")
    }

    /// Appends a track to one of the user's playlists. Tidal asks for the playlist's current version.
    pub async fn add_to_playlist(&self, playlist: &str, track: u64) -> Result<()> {
        let url = format!("{V1}/playlists/{playlist}");
        let current = self.send(Method::GET, &url, &[], &[]).await?;
        let version = current.headers().get("etag").context("no playlist version")?.clone();
        current.bytes().await?; // read to the end so the connection is reused for the POST
        let form = [("trackIds", track.to_string()), ("onDupes", "SKIP".into()), ("onArtifactNotFound", "SKIP".into())];
        let req = self.request(Method::POST, &format!("{url}/items"), &[]).await?;
        req.header("If-None-Match", version).form(&form).send().await?.error_for_status()?;
        Ok(())
    }

    pub async fn favorite_ids(&self) -> Result<HashSet<u64>> {
        let v = self.get(&format!("{}/favorites/ids", self.user().await?), &[]).await?;
        Ok(v["TRACK"].as_array().into_iter().flatten().filter_map(|id| id.as_str()?.parse().ok()).collect())
    }

    pub async fn set_favorite(&self, id: u64, on: bool) -> Result<()> {
        let url = format!("{}/favorites/tracks", self.user().await?);
        if on {
            self.send(Method::POST, &url, &[], &[("trackIds", &id.to_string())]).await?;
        } else {
            self.send(Method::DELETE, &format!("{url}/{id}"), &[], &[]).await?;
        }
        Ok(())
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
        .map(|s| 1 + attr(&format!(" {}", &s[..s.find('>').unwrap_or(s.len())]), "r").and_then(|r| r.parse::<u32>().ok()).unwrap_or(0))
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

fn list<T>(v: &Value, parse: fn(&Value) -> Option<T>) -> Vec<T> {
    v.as_array().map_or_else(Vec::new, |a| a.iter().filter_map(parse).collect())
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
    let artist = v["artist"]["name"].as_str().or(v["artists"][0]["name"].as_str()).unwrap_or_default();
    Some(Album {
        id: v["id"].as_u64()?,
        title: text(&v["title"]),
        artist: artist.into(),
        cover: image_id(&v["cover"]),
        year: v["releaseDate"].as_str().unwrap_or_default().chars().take(4).collect(),
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
