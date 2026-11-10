//! Changing the bucket list: everything the bucket manager does to ~/.config/flea-b2/buckets.json
//! and the keyring, with its rules and its sentences. Each call can block on Backblaze or the
//! keyring; the manager runs them off its UI thread.
//!
//! A change is made on a copy of the list and only replaces it once the file is written, so a
//! refusal from the keyring or the disk leaves nothing half-saved.
use crate::b2::Client;
use crate::config::{Config, Entry, MAX_BUCKETS};
use crate::creds;
use crate::error::{Error, Result};
use crate::paths::root_prefix;
use std::time::Duration;

/// Long enough to answer the keyring's own unlock prompt.
const KEYRING_WAIT: Duration = Duration::from_secs(120);
pub const APP_KEYS_URL: &str = "https://secure.backblaze.com/app_keys.htm";

/// A key the user chose for an entry: a stored one, or a new one that is stored on save.
#[derive(Clone)]
pub struct KeyChoice {
    pub key_id: String,
    pub new_secret: Option<String>,
}

/// A key Backblaze accepted, signed in.
pub struct CheckedKey {
    pub choice: KeyChoice,
    client: Client,
}

impl CheckedKey {
    /// Reads a stored key from the keyring and signs in with it.
    pub fn stored(key_id: &str) -> Result<CheckedKey> {
        let secret = creds::lookup(key_id, KEYRING_WAIT)?;
        Ok(CheckedKey {
            client: connect(key_id, &secret)?,
            choice: KeyChoice {
                key_id: key_id.to_string(),
                new_secret: None,
            },
        })
    }

    /// Signs in with a key typed in; nothing is stored until the entry is saved.
    pub fn new_key(key_id: &str, secret: &str) -> Result<CheckedKey> {
        let (key_id, secret) = (key_id.trim(), secret.trim());
        if key_id.is_empty() || secret.is_empty() {
            return Err(Error::msg(
                "Both the key ID and the application key are needed.",
            ));
        }
        Ok(CheckedKey {
            client: connect(key_id, secret)?,
            choice: KeyChoice {
                key_id: key_id.to_string(),
                new_secret: Some(secret.to_string()),
            },
        })
    }

    /// The buckets this key can open.
    pub fn bucket_names(&mut self) -> Result<Vec<String>> {
        let names = self.client.bucket_names()?;
        if names.is_empty() {
            return Err(Error::msg("This key cannot open any bucket."));
        }
        Ok(names)
    }
}

fn connect(key_id: &str, secret: &str) -> Result<Client> {
    let mut client = Client::new(key_id.to_string(), secret.to_string());
    client.authorize()?;
    Ok(client)
}

/// Refuses a new entry once Flea's rail section is full.
pub fn check_room(config: &Config) -> Result<()> {
    if config.buckets.len() >= MAX_BUCKETS {
        return Err(Error::msg(format!(
            "Flea shows at most {MAX_BUCKETS} buckets."
        )));
    }
    Ok(())
}

/// The entries that use a key, by name.
pub fn users_of<'a>(config: &'a Config, key_id: &str) -> Vec<&'a str> {
    config
        .buckets
        .iter()
        .filter(|e| e.key_id == key_id)
        .map(|e| e.name.as_str())
        .collect()
}

/// The name a new entry for `bucket` starts with: the bucket's own, numbered when it is taken.
pub fn suggested_name(config: &Config, bucket: &str) -> String {
    let mut name = bucket.to_string();
    let mut number = 2;
    while config.check_name(&name, None).is_err() && number < 100 {
        name = format!("{bucket} {number}");
        number += 1;
    }
    name
}

/// The name and folder as they would be saved, or why they cannot be.
pub fn check_details(
    config: &Config,
    name: &str,
    folder: &str,
    editing: Option<usize>,
) -> Result<(String, String)> {
    let name = name.trim().to_string();
    let folder = folder.trim().trim_matches('/').to_string();
    config.check_name(&name, editing)?;
    root_prefix(&folder)?;
    Ok((name, folder))
}

/// Adds an entry, or replaces the one at `editing`, storing a new key first.
pub fn save_entry(
    config: &mut Config,
    editing: Option<usize>,
    entry: Entry,
    key: &KeyChoice,
) -> Result<String> {
    let mut next = config.clone();
    let name = entry.name.clone();
    match editing {
        Some(index) => next.buckets[index] = entry,
        None => {
            check_room(&next)?;
            next.buckets.push(entry);
        }
    }
    if let Some(secret) = &key.new_secret {
        creds::store(&key.key_id, secret)?;
    }
    next.save()?;
    *config = next;
    let verb = if editing.is_some() { "Saved" } else { "Added" };
    Ok(format!(
        "{verb} “{name}”. Flea shows it in its sidebar within a few seconds."
    ))
}

/// Moves the entry at `index` onto another key, once that key can open its bucket.
pub fn change_key(config: &mut Config, index: usize, key: &mut CheckedKey) -> Result<String> {
    let entry = config.buckets[index].clone();
    key.client.bucket_id(&entry.bucket)?;
    if let Some(secret) = &key.choice.new_secret {
        creds::store(&key.choice.key_id, secret)?;
    }
    let mut next = config.clone();
    next.buckets[index].key_id = key.choice.key_id.clone();
    next.save()?;
    *config = next;
    let forgot = forget_unused(config, &entry.key_id);
    Ok(format!(
        "“{}” now uses key {}.{forgot}",
        entry.name, key.choice.key_id
    ))
}

/// Removes the entry at `index` from Flea; nothing is deleted in B2.
pub fn remove(config: &mut Config, index: usize) -> Result<String> {
    let mut next = config.clone();
    let entry = next.buckets.remove(index);
    next.save()?;
    *config = next;
    let forgot = forget_unused(config, &entry.key_id);
    Ok(format!(
        "Removed “{}” from Flea. Its files in B2 are untouched.{forgot}",
        entry.name
    ))
}

/// Swaps the entry at `index` with the one above or below it, which Flea's sidebar follows.
/// Returns where the entry went, or `None` at either end.
pub fn move_entry(config: &mut Config, index: usize, up: bool) -> Result<Option<usize>> {
    let other = if up {
        index.checked_sub(1)
    } else {
        Some(index + 1).filter(|i| *i < config.buckets.len())
    };
    let Some(other) = other else { return Ok(None) };
    let mut next = config.clone();
    next.buckets.swap(index, other);
    next.save()?;
    *config = next;
    Ok(Some(other))
}

/// Deletes a key from the keyring once no entry uses it; returns a sentence to add to the status.
fn forget_unused(config: &Config, key_id: &str) -> String {
    if !users_of(config, key_id).is_empty() {
        return String::new();
    }
    match creds::delete(key_id) {
        Ok(n) if n > 0 => format!(" Key {key_id} is no longer used and left the keyring."),
        Ok(_) => String::new(),
        Err(error) => format!(" Key {key_id} could not be removed from the keyring: {error}"),
    }
}

/// Signs in with the entry's key and lists the top of what it shows.
pub fn test(entry: &Entry) -> Result<String> {
    let mut key = CheckedKey::stored(&entry.key_id)?;
    let bucket_id = key.client.bucket_id(&entry.bucket)?;
    let (files, more) = key
        .client
        .list_names(&bucket_id, &entry.root(), Some("/"), None, 1000)?;
    let count = files.len();
    let shown = if more.is_some() {
        format!("more than {}", crate::util::plural(count, "item"))
    } else {
        crate::util::plural(count, "item")
    };
    Ok(format!("“{}” works: {shown} at its top level.", entry.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, key_id: &str) -> Entry {
        Entry {
            name: name.into(),
            bucket: "photos".into(),
            folder: String::new(),
            key_id: key_id.into(),
        }
    }

    #[test]
    fn suggested_names_skip_taken_ones() {
        let mut config = Config::default();
        assert_eq!(suggested_name(&config, "photos"), "photos");
        config.buckets.push(entry("photos", "k1"));
        config.buckets.push(entry("photos 2", "k1"));
        assert_eq!(suggested_name(&config, "photos"), "photos 3");
    }

    #[test]
    fn details_are_trimmed_and_checked() {
        let mut config = Config::default();
        config.buckets.push(entry("Photos", "k1"));
        assert_eq!(
            check_details(&config, "  Docs ", " /docs/2024/ ", None).unwrap(),
            ("Docs".to_string(), "docs/2024".to_string())
        );
        assert!(check_details(&config, "Photos", "", None).is_err());
        assert!(check_details(&config, "Photos", "", Some(0)).is_ok());
        assert!(check_details(&config, ".hidden", "", None).is_err());
    }

    #[test]
    fn new_keys_need_both_parts() {
        let error = CheckedKey::new_key(" ", "secret").err().unwrap();
        assert!(error.to_string().contains("Both the key ID"));
    }

    #[test]
    fn users_are_listed_by_key() {
        let mut config = Config::default();
        config.buckets.push(entry("A", "k1"));
        config.buckets.push(entry("B", "k2"));
        config.buckets.push(entry("C", "k1"));
        assert_eq!(users_of(&config, "k1"), ["A", "C"]);
    }
}
