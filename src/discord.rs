//! "Listening to Riptide" on the user's Discord profile, through the local connection the Discord
//! app offers to other programs. Nothing is sent anywhere else, and without Discord nothing happens.

use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use serde_json::{Value, json};

use crate::tidal::{self, Item, Track};

/// Riptide's application on Discord, which names the status.
const CLIENT_ID: &str = "1557212658904596490";

/// The status as last sent, so it is sent again only when it changes.
#[derive(Default)]
pub struct Discord {
    /// The connection's thread, started the first time there is something to show.
    tx: Option<Sender<Value>>,
    /// The track shown and when it started (Unix seconds).
    shown: Option<(u64, i64)>,
}

impl Discord {
    /// Shows `track` as having started at `started` (Unix seconds), or clears the status.
    pub fn show(&mut self, now: Option<(&Track, i64)>) {
        let key = now.map(|(t, started)| (t.id, started));
        // A second or two of drift is the clock, not a seek.
        let same = match (self.shown, key) {
            (Some((a, s)), Some((b, t))) => a == b && (s - t).abs() <= 2,
            (shown, key) => shown.is_none() && key.is_none(),
        };
        if same {
            return;
        }
        self.shown = key;
        let activity = now.map_or(Value::Null, |(t, started)| {
            let mut activity = json!({
                "type": 2,
                "details": t.title,
                "state": t.artist,
                "timestamps": { "start": started * 1000, "end": (started + i64::from(t.duration)) * 1000 },
                "buttons": [{ "label": "Open in Tidal", "url": Item::Track(t.id).link() }],
            });
            if let Some(cover) = &t.cover {
                activity["assets"] = json!({ "large_image": tidal::image(cover, 640), "large_text": t.album });
            }
            activity
        });
        if self.tx.is_some() || !activity.is_null() {
            let _ = self.tx.get_or_insert_with(spawn).send(activity);
        }
    }
}

/// The thread that talks to Discord: it connects when there is something to show, and tries again
/// now and then while Discord isn't running.
fn spawn() -> Sender<Value> {
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        let (mut pipe, mut latest, mut nonce) = (None, Value::Null, 0u64);
        loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                // Quick skips settle into one update: Discord takes only a few every 20 seconds.
                Ok(activity) => {
                    std::thread::sleep(Duration::from_secs(1));
                    latest = rx.try_iter().last().unwrap_or(activity);
                }
                Err(RecvTimeoutError::Timeout) if pipe.is_none() && !latest.is_null() => {}
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
            nonce += 1;
            let set = json!({ "cmd": "SET_ACTIVITY", "args": { "pid": std::process::id(), "activity": latest }, "nonce": nonce.to_string() });
            // A connection Discord closed is opened again once.
            for _ in 0..2 {
                if pipe.is_none() {
                    pipe = connect();
                }
                let Some(open) = &mut pipe else { break };
                if send(open, 1, &set).is_ok() {
                    break;
                }
                pipe = None;
            }
        }
    });
    tx
}

fn connect() -> Option<File> {
    (0..10).find_map(|n| {
        let mut pipe = File::options().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{n}")).ok()?;
        send(&mut pipe, 0, &json!({ "v": 1, "client_id": CLIENT_ID })).ok()?;
        Some(pipe)
    })
}

/// One message: its kind and length, then the JSON. Discord answers each; the answer is read so
/// the pipe never fills.
fn send(pipe: &mut File, op: u32, body: &Value) -> io::Result<()> {
    let body = body.to_string();
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend(op.to_le_bytes());
    frame.extend((body.len() as u32).to_le_bytes());
    frame.extend(body.as_bytes());
    pipe.write_all(&frame)?;
    let mut head = [0; 8];
    pipe.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    io::copy(&mut (&mut *pipe).take(len.into()), &mut io::sink())?;
    Ok(())
}
