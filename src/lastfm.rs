use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use md5::{Digest, Md5};
use serde_json::Value;

use crate::tidal::{HTTP, Track};

const API: &str = "https://ws.audioscrobbler.com/2.0/";

/// Last.fm scrobbling with the user's own API account, kept in `data/lastfm.txt` as `key=`,
/// `secret=`, and once connected `session=` and `user=` lines.
#[derive(Clone)]
pub struct LastFm {
    path: PathBuf,
    key: String,
    secret: String,
    pub session: Option<(String, String)>,
}

impl LastFm {
    pub fn path(data: &Path) -> PathBuf {
        data.join("lastfm.txt")
    }

    pub fn load(path: PathBuf) -> Option<Self> {
        let text = std::fs::read_to_string(&path).ok()?;
        let field = |name: &str| text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix('=')).map(|v| v.trim().to_string());
        let session = field("session").zip(field("user"));
        Some(Self { key: field("key")?, secret: field("secret")?, session, path })
    }

    fn save(&self) -> Result<()> {
        let mut text = format!("key={}\nsecret={}\n", self.key, self.secret);
        if let Some((session, user)) = &self.session {
            text += &format!("session={session}\nuser={user}\n");
        }
        Ok(std::fs::write(&self.path, text)?)
    }

    /// A signed API call: every parameter but `format`, sorted, then the secret, hashed.
    async fn call(&self, method: &str, mut params: Vec<(&str, String)>, post: bool) -> Result<Value> {
        params.extend([("method", method.into()), ("api_key", self.key.clone())]);
        if let Some((session, _)) = &self.session {
            params.push(("sk", session.clone()));
        }
        params.sort();
        let signature: String = params.iter().map(|(k, v)| format!("{k}{v}")).collect::<String>() + &self.secret;
        params.push(("api_sig", format!("{:x}", Md5::digest(signature))));
        params.push(("format", "json".into()));
        let req = if post { HTTP.post(API).form(&params) } else { HTTP.get(API).query(&params) };
        let v: Value = req.send().await?.json().await?;
        if let Some(code) = v["error"].as_u64() {
            bail!("Last.fm: {} ({code})", v["message"].as_str().unwrap_or("error"));
        }
        Ok(v)
    }

    /// Connects through the browser: the user approves there, and this waits up to three minutes
    /// for it, then saves the session.
    pub async fn connect(mut self) -> Result<Self> {
        self.session = None;
        let token = self.call("auth.getToken", Vec::new(), false).await?["token"].as_str().context("no Last.fm token")?.to_string();
        open::that(format!("https://www.last.fm/api/auth/?api_key={}&token={token}", self.key))?;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if let Ok(v) = self.call("auth.getSession", vec![("token", token.clone())], false).await {
                let s = &v["session"];
                self.session = Some((s["key"].as_str().context("no session")?.into(), s["name"].as_str().unwrap_or_default().into()));
                self.save()?;
                return Ok(self);
            }
        }
        bail!("Last.fm wasn't approved in time")
    }

    fn track(t: &Track) -> Vec<(&'static str, String)> {
        vec![("artist", t.artist.clone()), ("track", t.title.clone()), ("album", t.album.clone()), ("duration", t.duration.to_string())]
    }

    pub async fn now_playing(&self, t: &Track) -> Result<()> {
        self.call("track.updateNowPlaying", Self::track(t), true).await.map(drop)
    }

    pub async fn scrobble(&self, t: &Track, started: u64) -> Result<()> {
        let mut params = Self::track(t);
        params.push(("timestamp", started.to_string()));
        self.call("track.scrobble", params, true).await.map(drop)
    }
}

/// Last.fm's rule: a track over 30 seconds counts once half of it, or four minutes, has played.
pub fn counts(t: &Track, played: f64) -> bool {
    t.duration > 30 && played >= (f64::from(t.duration) / 2.0).min(240.0)
}
