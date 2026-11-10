//! The buckets shown in Flea: ~/.config/flea-b2/buckets.json, written by the manager, read by the plugin.
//!
//! It holds no secret: each entry names a key ID whose application key lives in the keyring.
use crate::error::{Error, Result};
use crate::paths::{root_prefix, segments, valid_name, MORE_MARKER};
use crate::util::{private, text, write_json_atomic};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::SystemTime;

/// Flea draws at most 64 rows in a plugin's rail section, and the last is “Manage buckets…”.
pub const MAX_BUCKETS: usize = 63;
const MAX_NAME_CHARS: usize = 80;
pub const EMPTY_HINT: &str =
    "No bucket yet. Choose “Manage buckets…” under Backblaze B2 in the sidebar to add one.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The name shown in Flea's sidebar and as the folder at the place's root.
    pub name: String,
    pub bucket: String,
    /// Optional folder inside the bucket that the entry shows, without slashes at its ends.
    pub folder: String,
    pub key_id: String,
}

impl Entry {
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "bucket": self.bucket, "folder": self.folder, "keyId": self.key_id})
    }

    pub fn from_json(value: &Value) -> Option<Entry> {
        let entry = Entry {
            name: text(value, "name").to_string(),
            bucket: text(value, "bucket").to_string(),
            folder: text(value, "folder").trim_matches('/').to_string(),
            key_id: text(value, "keyId").to_string(),
        };
        let usable = valid_name(&entry.name)
            && entry.name != MORE_MARKER
            && !entry.bucket.is_empty()
            && !entry.key_id.is_empty()
            && root_prefix(&entry.folder).is_ok();
        usable.then_some(entry)
    }

    /// The key prefix this entry shows: its folder, with a trailing slash, or "".
    pub fn root(&self) -> String {
        root_prefix(&self.folder).unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub buckets: Vec<Entry>,
}

pub fn path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    base.join("flea-b2").join("buckets.json")
}

/// When the file last changed, so the plugin knows to drop what it cached.
pub fn modified() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

impl Config {
    pub fn load() -> Result<Config> {
        let path = path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => {
                return Err(Error::msg(format!(
                    "{} cannot be read: {e}",
                    path.display()
                )))
            }
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| {
            Error::msg(format!(
                "{} is not valid JSON ({e}). Fix or delete it.",
                path.display()
            ))
        })?;
        let mut config = Config::default();
        for entry in value
            .get("buckets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            // An entry that is unusable or repeats a name is skipped, so Flea never gets a bad row.
            if let Some(entry) = Entry::from_json(entry) {
                if config.buckets.len() < MAX_BUCKETS && config.find(&entry.name).is_none() {
                    config.buckets.push(entry);
                }
            }
        }
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = path();
        if let Some(folder) = path.parent() {
            private(folder.into())?;
        }
        let buckets: Vec<Value> = self.buckets.iter().map(Entry::to_json).collect();
        write_json_atomic(&path, &json!({"version": 1, "buckets": buckets}))?;
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.buckets.iter().find(|e| e.name == name)
    }

    /// Why this name cannot be used for the entry at `except` (or a new one), if it cannot.
    pub fn check_name(&self, name: &str, except: Option<usize>) -> Result<()> {
        if name.trim() != name || name.is_empty() {
            return Err(Error::msg(
                "The name cannot be empty or start or end with a space.",
            ));
        }
        if !valid_name(name) || name == MORE_MARKER || name.starts_with('.') {
            return Err(Error::msg(
                "The name cannot hold / or \\, start with a dot, or use control characters.",
            ));
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(Error::msg(format!(
                "The name is longer than {MAX_NAME_CHARS} characters."
            )));
        }
        let taken = self
            .buckets
            .iter()
            .enumerate()
            .any(|(i, e)| e.name == name && Some(i) != except);
        if taken {
            return Err(Error::msg(format!(
                "Another entry is already called “{name}”."
            )));
        }
        Ok(())
    }

    /// The key IDs in use, each once, in entry order.
    pub fn key_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for entry in &self.buckets {
            if !ids.contains(&entry.key_id) {
                ids.push(entry.key_id.clone());
            }
        }
        ids
    }

    /// Splits a place path into its entry and the path inside it: "/Photos/2024" gives
    /// (Photos, "/2024"); the place's root "/" gives no entry.
    pub fn split<'a>(&'a self, place_path: &str) -> Result<(Option<&'a Entry>, String)> {
        let parts = segments(place_path)?;
        let Some(first) = parts.first() else {
            return Ok((None, "/".into()));
        };
        let entry = self.find(first).ok_or_else(|| {
            Error::msg(format!(
                "“{first}” is no longer in your B2 buckets. Choose “Manage buckets…” to see them."
            ))
        })?;
        Ok((Some(entry), format!("/{}", parts[1..].join("/"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            bucket: "b".into(),
            folder: String::new(),
            key_id: "k".into(),
        }
    }

    #[test]
    fn names() {
        let config = Config {
            buckets: vec![entry("Photos")],
        };
        assert!(config.check_name("Backups", None).is_ok());
        assert!(config.check_name("Photos", Some(0)).is_ok());
        for bad in ["Photos", "", " x", "a/b", ".hidden", "a\\b", "tab\t"] {
            assert!(config.check_name(bad, None).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn split_paths() {
        let config = Config {
            buckets: vec![entry("Photos")],
        };
        assert_eq!(config.split("/").unwrap().0, None);
        let (found, inner) = config.split("/Photos/2024/x.jpg").unwrap();
        assert_eq!(found.unwrap().name, "Photos");
        assert_eq!(inner, "/2024/x.jpg");
        assert_eq!(config.split("/Photos").unwrap().1, "/");
        assert!(config.split("/Gone/x").is_err());
    }

    #[test]
    fn unusable_entries_are_skipped() {
        let value = json!({"name": "a/b", "bucket": "b", "folder": "", "keyId": "k"});
        assert!(Entry::from_json(&value).is_none());
        let value = json!({"name": "ok", "bucket": "b", "folder": "/x/", "keyId": "k"});
        assert_eq!(Entry::from_json(&value).unwrap().folder, "x");
        assert_eq!(Entry::from_json(&value).unwrap().root(), "x/");
    }
}
