use std::collections::HashSet;

use anyhow::Result;

use super::{Action, App, Body, Page, ROOT, Source};
use crate::dialogs::{PlaylistForm, Target};
use crate::tidal::{Card, Item, Playlist, Tidal, Track};

/// The user's collection as the app shows it: what is saved, the sidebar's folders, and the
/// playlists they made (the ones they can change).
#[derive(Default)]
pub struct Library {
    pub saved: HashSet<Item>,
    /// The top level of the playlist folders.
    pub folders: Vec<Card>,
    /// Most recently changed first, as "Add to playlist" lists them.
    pub playlists: Vec<Playlist>,
}

impl Library {
    pub async fn load(tidal: &Tidal) -> Result<Self> {
        let (saved, folders, playlists) = tokio::try_join!(tidal.saved(), tidal.folder(ROOT), tidal.my_playlists())?;
        Ok(Self { saved, folders, playlists })
    }

    /// Whether the user made this playlist, and so can change it.
    pub fn mine(&self, id: &str) -> bool {
        self.playlists.iter().any(|p| p.id == id)
    }
}

impl App {
    /// The open page's tracks, if it is this playlist.
    fn open_playlist(&mut self, id: &str) -> Option<&mut Vec<Track>> {
        match &mut self.page {
            Some(Page { source: Source::Playlist(open), body: Body::Tracks { tracks, .. }, .. }) if open == id => Some(tracks),
            _ => None,
        }
    }

    /// A change to the library: Tidal is told, then the app catches up. Edits to the open track
    /// list show at once; the rest arrive with the refresh after Tidal has them.
    pub(super) fn edit(&mut self, tidal: Tidal, action: Action) {
        match action {
            Action::Save(item, on) => {
                if on { self.library.saved.insert(item.clone()) } else { self.library.saved.remove(&item) };
                let track = matches!(item, Item::Track(_));
                let task = async move { tidal.set_saved(&item, on).await };
                // A track's heart changes nothing else; anything else shows in a list or the sidebar.
                if track { self.run(task, None) } else { self.changed(task, None) }
            }
            Action::AddToPlaylist(id, track) => {
                let notice = self.library.playlists.iter().find(|p| p.id == id).map(|p| format!("Added to {}", p.title));
                self.changed(async move { tidal.add_to_playlist(&id, track).await }, notice);
            }
            Action::RemoveFromPlaylist(id, index) => {
                if let Some(tracks) = self.open_playlist(&id).filter(|t| index < t.len()) {
                    tracks.remove(index);
                }
                self.run(async move { tidal.remove_from_playlist(&id, index).await }, None);
            }
            Action::MoveInPlaylist(id, from, to) => {
                if let Some(tracks) = self.open_playlist(&id).filter(|t| from.max(to) < t.len()) {
                    let track = tracks.remove(from);
                    tracks.insert(to, track);
                }
                self.run(async move { tidal.move_in_playlist(&id, from, to).await }, None);
            }
            Action::MovePlaylist(id, folder) => self.changed(async move { tidal.arrange("move", &format!("playlist:{id}"), Some(&folder)).await }, None),
            Action::DeletePlaylist(id) => {
                if self.open_playlist(&id).is_some() {
                    self.step(true);
                }
                self.changed(async move { tidal.arrange("remove", &format!("playlist:{id}"), None).await }, Some("Playlist deleted".into()));
            }
            Action::DeleteFolder(id) => self.changed(async move { tidal.delete_folder(&id).await }, Some("Folder deleted".into())),
            _ => {}
        }
    }

    /// Makes or renames the playlist or folder the dialog describes.
    pub(super) fn save_form(&mut self, form: PlaylistForm) {
        let Some(tidal) = self.tidal.clone() else { return };
        let PlaylistForm { target, title, description, public } = form;
        let title = title.trim().to_string();
        let notice = match target {
            Target::Create(_) | Target::CreateFolder => format!("Created {title}"),
            Target::Rename(_) | Target::RenameFolder(_) => format!("Renamed to {title}"),
        };
        let task = async move {
            match target {
                Target::Create(track) => {
                    let playlist = tidal.create_playlist(&title, description.trim(), public).await?;
                    if let Some(track) = track {
                        tidal.add_to_playlist(&playlist.id, track).await?;
                    }
                    Ok(())
                }
                Target::Rename(id) => tidal.rename_playlist(&id, &title).await,
                Target::CreateFolder => tidal.create_folder(&title).await,
                Target::RenameFolder(id) => tidal.rename_folder(&id, &title).await,
            }
        };
        self.changed(task, Some(notice));
    }
}
