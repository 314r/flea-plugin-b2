//! Place paths, B2 keys and the entry-name rules Flea enforces on a listing.
use crate::error::{Error, Result};

/// Flea refuses a listing row whose name is longer than the longest Linux filename.
pub const MAX_NAME_BYTES: usize = 255;
/// B2 refuses a file name longer than this.
pub const MAX_KEY_BYTES: usize = 1024;
/// The B2 web console keeps an empty folder alive with this zero-byte file.
pub const EMPTY_MARKER: &str = ".bzEmpty";
/// Flea takes at most 256 rows per listing; this row says the folder holds more.
pub const MORE_MARKER: &str = "… more items not shown (Flea lists 256 per folder)";

fn bidi(c: char) -> bool {
    matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// True when Flea accepts this name as a listing row (the same checks as its place model).
pub fn valid_name(name: &str) -> bool {
    valid_local_segment(name)
        && !name.contains('\\')
        && !name.chars().any(|c| c.is_control() || bidi(c))
}

/// The Folder setting as a key prefix: "" for the whole bucket, else "a/b/".
pub fn root_prefix(setting: &str) -> Result<String> {
    let parts: Vec<&str> = setting
        .trim()
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    if parts.iter().any(|p| *p == "." || *p == "..") {
        return Err(Error::msg(
            "The Folder setting cannot contain . or .. folders.",
        ));
    }
    Ok(parts.iter().map(|p| format!("{p}/")).collect())
}

pub fn segments(place_path: &str) -> Result<Vec<&str>> {
    if !place_path.starts_with('/') || place_path.contains('\0') {
        return Err(Error::msg("Flea sent an invalid place path."));
    }
    let parts: Vec<&str> = place_path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| *p == "." || *p == "..") {
        return Err(Error::msg("Flea sent an invalid place path."));
    }
    Ok(parts)
}

/// The key of a file, or of a folder without its trailing slash.
pub fn object_key(root: &str, place_path: &str) -> Result<String> {
    let parts = segments(place_path)?;
    if parts.is_empty() {
        return Err(Error::msg("The bucket itself cannot be changed."));
    }
    Ok(format!("{root}{}", parts.join("/")))
}

/// The key prefix of the items inside a folder, with its trailing slash.
pub fn folder_prefix(root: &str, place_path: &str) -> Result<String> {
    let parts = segments(place_path)?;
    Ok(format!(
        "{root}{}",
        parts.iter().map(|p| format!("{p}/")).collect::<String>()
    ))
}

pub fn parent_prefix(key: &str) -> &str {
    match key.rfind('/') {
        Some(at) => &key[..=at],
        None => "",
    }
}

pub fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

pub fn is_marker(path: &str) -> bool {
    basename(path) == MORE_MARKER
}

/// B2's own rule: at most 1024 bytes, no character below space and no DEL.
pub fn check_key(key: String, shown: &str) -> Result<String> {
    if key.len() > MAX_KEY_BYTES {
        return Err(Error::msg(format!(
            "“{shown}” has a path longer than B2's 1024 bytes."
        )));
    }
    if key.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
        return Err(Error::msg(format!(
            "“{shown}” has a control character B2 cannot store."
        )));
    }
    Ok(key)
}

/// A path segment from B2 that is safe to create on the local disk.
pub fn valid_local_segment(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['\0', '/'])
        && name.len() <= MAX_NAME_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flea_name_rules() {
        for good in [
            "a.txt",
            "Photos",
            ".hidden",
            "é è",
            &"x".repeat(255),
            MORE_MARKER,
        ] {
            assert!(valid_name(good), "{good}");
        }
        let long_utf8 = "é".repeat(128);
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "tab\there",
            "nul\0",
            "\x7f",
            "rtl\u{202e}",
            "iso\u{2066}",
            &"x".repeat(256),
            &long_utf8,
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn b2_key_rules() {
        assert_eq!(check_key("a/b c.txt".into(), "x").unwrap(), "a/b c.txt");
        for bad in ["bell\x07", "del\x7f", &"x".repeat(1025)] {
            assert!(check_key(bad.into(), "x").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn root_prefix_setting() {
        assert_eq!(root_prefix("").unwrap(), "");
        assert_eq!(root_prefix("/backups/").unwrap(), "backups/");
        assert_eq!(root_prefix("a//b").unwrap(), "a/b/");
        assert!(root_prefix("a/../b").is_err());
    }

    #[test]
    fn place_paths() {
        assert_eq!(folder_prefix("", "/").unwrap(), "");
        assert_eq!(folder_prefix("r/", "/").unwrap(), "r/");
        assert_eq!(
            folder_prefix("r/", "/Photos/2024").unwrap(),
            "r/Photos/2024/"
        );
        assert_eq!(object_key("r/", "/Photos/a.png").unwrap(), "r/Photos/a.png");
        assert_eq!(object_key("", "/Photos/").unwrap(), "Photos");
        assert_eq!(parent_prefix("r/Photos/a.png"), "r/Photos/");
        assert_eq!(parent_prefix("a.png"), "");
        assert_eq!(basename("/a/b/"), "b");
        for bad in ["relative", "/a/../b", "/./a", "/a\0"] {
            assert!(folder_prefix("", bad).is_err(), "{bad:?}");
        }
        assert!(object_key("", "/").is_err());
    }
}
