use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tidlers::TidalClient;
use tidlers::auth::TidalAuth;
use tidlers::client::models::playback::AudioQuality;
use tidlers::client::models::track::playback::ParsedTrackManifest;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Quality {
    High,
    Lossless,
    Max,
}

impl Quality {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "high" => Some(Self::High),
            "lossless" => Some(Self::Lossless),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    fn tidlers(self) -> AudioQuality {
        match self {
            Self::High => AudioQuality::High,
            Self::Lossless => AudioQuality::Lossless,
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
    pub quality: String,
    pub codec: String,
}

pub struct Tidal {
    client: TidalClient,
}

impl Tidal {
    pub async fn login(session: &Path) -> Result<Self> {
        let mut client = TidalClient::new(&TidalAuth::with_pkce());
        let url = client.initiate_pkce_login()?;
        println!("Sign in in your browser, then paste the address of the page you land on:\n{url}");
        let _ = open::that(&url);
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        client.finish_pkce_login(line.trim()).await?;
        save(session, &client)?;
        Ok(Self { client })
    }

    pub async fn load(session: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(session).context("not signed in; run `tidalfast login`")?;
        let mut client = TidalClient::from_json(&json)?;
        if client.refresh_access_token(false).await? {
            save(session, &client)?;
        }
        Ok(Self { client })
    }

    pub async fn stream(&mut self, track_id: &str, quality: Quality) -> Result<Stream> {
        self.client.set_audio_quality(quality.tidlers());
        let info = self.client.get_track_postpaywall_playback_info(track_id, None).await?;
        let codec = info.get_codecs().unwrap_or_default();
        let parts = match info.manifest_parsed {
            Some(ParsedTrackManifest::Json(m)) => Parts::Urls(m.urls),
            Some(ParsedTrackManifest::Dash(m)) => Parts::Segments {
                init: m.get_init_url().context("DASH manifest has no init segment")?.clone(),
                template: m.get_media_template().context("DASH manifest has no media template")?.clone(),
                start: m.start_number.unwrap_or(1),
            },
            None => bail!("Tidal returned no stream manifest"),
        };
        Ok(Stream { parts, quality: info.audio_quality, codec })
    }
}

fn save(session: &Path, client: &TidalClient) -> Result<()> {
    std::fs::create_dir_all(session.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(session, client.get_json())?;
    Ok(())
}

pub fn session_path(dir: &Path) -> PathBuf {
    dir.join("data").join("session.json")
}
