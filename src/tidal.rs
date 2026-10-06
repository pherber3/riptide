use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::Value;
use tidlers::TidalClient;
use tidlers::auth::TidalAuth;
use tidlers::client::models::playback::AudioQuality;
use tidlers::client::models::track::playback::ParsedTrackManifest;

const API: &str = "https://api.tidal.com/v1";

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

    fn tidlers(self) -> AudioQuality {
        match self {
            Self::Low => AudioQuality::High,
            Self::High => AudioQuality::Lossless,
            Self::Max => AudioQuality::HiRes,
        }
    }
}

pub enum Parts {
    Urls(Vec<String>),
    Segments { init: String, template: String, start: u32 },
}

pub struct Stream {
    pub parts: Parts,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<u64>,
    pub album: String,
    pub album_id: Option<u64>,
    pub cover: Option<String>,
    pub duration: u32,
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

#[derive(Clone, Debug, Default)]
pub struct Results {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
    pub playlists: Vec<Playlist>,
}

/// An image URL from a Tidal image id. Albums come in 80/160/320/640/1280, artists in 160/320/480/750.
pub fn image(id: &str, size: u32) -> String {
    format!("https://resources.tidal.com/images/{}/{size}x{size}.jpg", id.replace('-', "/"))
}

pub struct Tidal {
    client: TidalClient,
    session: PathBuf,
    http: reqwest::Client,
}

impl Tidal {
    /// Starts a PKCE sign-in; the user opens the URL and passes the address they land on to `finish_login`.
    pub fn start_login(session: &Path) -> Result<(Self, String)> {
        let mut client = TidalClient::new(&TidalAuth::with_pkce());
        let url = client.initiate_pkce_login()?;
        Ok((Self { client, session: session.into(), http: reqwest::Client::new() }, url))
    }

    pub async fn finish_login(&mut self, redirect: &str) -> Result<()> {
        self.client.finish_pkce_login(redirect.trim()).await?;
        self.save()
    }

    pub async fn load(session: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(session).context("not signed in")?;
        let client = TidalClient::from_json(&json)?;
        let mut tidal = Self { client, session: session.into(), http: reqwest::Client::new() };
        tidal.refresh().await?;
        Ok(tidal)
    }

    async fn refresh(&mut self) -> Result<()> {
        if self.client.refresh_access_token(false).await? {
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        std::fs::create_dir_all(self.session.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(&self.session, self.client.get_json())?;
        Ok(())
    }

    pub async fn stream(&mut self, track_id: u64, quality: Quality) -> Result<Stream> {
        self.refresh().await?;
        self.client.set_audio_quality(quality.tidlers());
        let info = self.client.get_track_postpaywall_playback_info(track_id.to_string(), None).await?;
        let parts = match info.manifest_parsed {
            Some(ParsedTrackManifest::Json(m)) => Parts::Urls(m.urls),
            // tidlers leaves XML escapes in the DASH attributes.
            Some(ParsedTrackManifest::Dash(m)) => Parts::Segments {
                init: m.get_init_url().context("DASH manifest has no init segment")?.replace("&amp;", "&"),
                template: m.get_media_template().context("DASH manifest has no media template")?.replace("&amp;", "&"),
                start: m.start_number.unwrap_or(1),
            },
            None => bail!("Tidal returned no stream manifest"),
        };
        Ok(Stream { parts })
    }

    async fn request(&mut self, method: Method, path: &str, query: &[(&str, &str)], form: &[(&str, &str)]) -> Result<reqwest::Response> {
        self.refresh().await?;
        let token = self.client.session.auth.access_token.clone().context("not signed in")?;
        let country = self.client.user_info.as_ref().map_or("US".into(), |u| u.country_code.clone());
        let mut req = self
            .http
            .request(method, format!("{API}/{path}"))
            .bearer_auth(token)
            .query(&[("countryCode", country.as_str())])
            .query(query);
        if !form.is_empty() {
            req = req.form(form);
        }
        Ok(req.send().await?.error_for_status()?)
    }

    async fn get(&mut self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        Ok(self.request(Method::GET, path, query, &[]).await?.json().await?)
    }

    /// Every item of a paged list, up to `max`.
    async fn items(&mut self, path: &str, query: &[(&str, &str)], max: usize) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        loop {
            let offset = all.len().to_string();
            let mut page_query = vec![("limit", "50"), ("offset", offset.as_str())];
            page_query.extend_from_slice(query);
            let page = self.get(path, &page_query).await?;
            let items = page["items"].as_array().cloned().unwrap_or_default();
            let total = page["totalNumberOfItems"].as_u64().unwrap_or(0) as usize;
            let empty = items.is_empty();
            all.extend(items);
            if empty || all.len() >= total.min(max) {
                return Ok(all);
            }
        }
    }

    pub async fn search(&mut self, query: &str) -> Result<Results> {
        let types = "ARTISTS,ALBUMS,TRACKS,PLAYLISTS";
        let v = self.get("search", &[("query", query), ("types", types), ("limit", "20")]).await?;
        Ok(Results {
            artists: list(&v["artists"]["items"], artist),
            albums: list(&v["albums"]["items"], album),
            tracks: list(&v["tracks"]["items"], track),
            playlists: list(&v["playlists"]["items"], playlist),
        })
    }

    pub async fn album(&mut self, id: u64) -> Result<(Album, Vec<Track>)> {
        let info = self.get(&format!("albums/{id}"), &[]).await?;
        let items = self.items(&format!("albums/{id}/items"), &[], 1000).await?;
        Ok((album(&info).context("bad album")?, items.iter().filter_map(track).collect()))
    }

    pub async fn artist(&mut self, id: u64) -> Result<(Artist, Vec<Track>, Vec<Album>)> {
        let info = self.get(&format!("artists/{id}"), &[]).await?;
        let top = self.get(&format!("artists/{id}/toptracks"), &[("limit", "10")]).await?;
        let albums = self.items(&format!("artists/{id}/albums"), &[], 200).await?;
        let albums = albums.iter().filter_map(album).collect();
        Ok((artist(&info).context("bad artist")?, list(&top["items"], track), albums))
    }

    pub async fn playlist(&mut self, id: &str) -> Result<(Playlist, Vec<Track>)> {
        let info = self.get(&format!("playlists/{id}"), &[]).await?;
        let items = self.items(&format!("playlists/{id}/items"), &[], 10_000).await?;
        Ok((playlist(&info).context("bad playlist")?, items.iter().filter_map(track).collect()))
    }

    fn user(&self) -> Result<u64> {
        let info = self.client.user_info.as_ref().map(|u| u.user_id);
        info.or(self.client.session.auth.user_id).context("not signed in")
    }

    /// A favorites list, newest first.
    async fn favorites(&mut self, kind: &str) -> Result<Vec<Value>> {
        let path = format!("users/{}/favorites/{kind}", self.user()?);
        let items = self.items(&path, &[("order", "DATE"), ("orderDirection", "DESC")], 10_000).await?;
        Ok(items.into_iter().map(|mut v| v["item"].take()).collect())
    }

    pub async fn favorite_tracks(&mut self) -> Result<Vec<Track>> {
        Ok(self.favorites("tracks").await?.iter().filter_map(track).collect())
    }

    pub async fn favorite_albums(&mut self) -> Result<Vec<Album>> {
        Ok(self.favorites("albums").await?.iter().filter_map(album).collect())
    }

    pub async fn favorite_artists(&mut self) -> Result<Vec<Artist>> {
        Ok(self.favorites("artists").await?.iter().filter_map(artist).collect())
    }

    /// Your own playlists and the ones you follow, newest first.
    pub async fn playlists(&mut self) -> Result<Vec<Playlist>> {
        let path = format!("users/{}/playlistsAndFavoritePlaylists", self.user()?);
        let items = self.items(&path, &[("order", "DATE"), ("orderDirection", "DESC")], 1000).await?;
        Ok(items.iter().filter_map(|v| playlist(&v["playlist"])).collect())
    }

    pub async fn favorite_ids(&mut self) -> Result<HashSet<u64>> {
        let v = self.get(&format!("users/{}/favorites/ids", self.user()?), &[]).await?;
        Ok(v["TRACK"].as_array().into_iter().flatten().filter_map(|id| id.as_str()?.parse().ok()).collect())
    }

    pub async fn set_favorite(&mut self, id: u64, on: bool) -> Result<()> {
        let path = format!("users/{}/favorites/tracks", self.user()?);
        if on {
            self.request(Method::POST, &path, &[], &[("trackIds", &id.to_string())]).await?;
        } else {
            self.request(Method::DELETE, &format!("{path}/{id}"), &[], &[]).await?;
        }
        Ok(())
    }
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

/// A track, or a playlist/album item wrapping one (`{"item": {...}, "type": "track"}`); videos are skipped.
fn track(v: &Value) -> Option<Track> {
    if v["item"].is_object() {
        return if v["type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("track")) { track(&v["item"]) } else { None };
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

fn playlist(v: &Value) -> Option<Playlist> {
    Some(Playlist {
        id: v["uuid"].as_str()?.into(),
        title: text(&v["title"]),
        cover: image_id(&v["squareImage"]).or_else(|| image_id(&v["image"])),
        count: v["numberOfTracks"].as_u64().unwrap_or(0),
    })
}

pub fn session_path(dir: &Path) -> PathBuf {
    dir.join("data").join("session.json")
}
