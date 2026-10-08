//! The small JSON files in the data folder.

use std::io;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The file's contents, if it exists and reads as a `T`.
pub fn load<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Writes `value` as indented JSON, so the files are easy to read and edit by hand.
pub fn save(path: &Path, value: &impl Serialize) -> io::Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value)?)
}
