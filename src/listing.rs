//! One folder of the bucket as Flea's list answer, held inside every bound Flea checks.
//!
//! A listing Flea refuses marks the whole plugin failed, so every row is checked here:
//! at most 256 rows, a message under 64 KiB, unique valid names.
use crate::b2::Client;
use crate::error::Result;
use crate::paths::{valid_name, EMPTY_MARKER, MORE_MARKER};
use crate::util::{int, text};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const MAX_ROWS: usize = 256;
/// Flea's message limit is 65,536 bytes; keep a margin for the envelope.
pub const MAX_BYTES: usize = 60_000;
const PAGE: u32 = 1000;
/// Past the first 256 rows, a few more pages are read only to find the subfolders, so a folder
/// that sorts after many files can still be opened. Each page is one B2 class C transaction.
const MAX_PAGES: usize = 5;

fn mtime(file: &Value) -> i64 {
    int(file.pointer("/fileInfo/src_last_modified_millis"))
        .or_else(|| int(file.get("uploadTimestamp")))
        .map_or(0, |ms| ms.max(0) / 1000)
}

fn encoded(value: &Value) -> usize {
    value.to_string().len()
}

/// The rows of the folder whose keys start with prefix ("" for the root).
pub fn entries(
    client: &mut Client,
    bucket_id: &str,
    prefix: &str,
    deadline: Instant,
) -> Result<Vec<Value>> {
    let mut folders: BTreeMap<String, Value> = BTreeMap::new();
    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    let mut start: Option<String> = None;
    let mut more = false;
    let mut pages = 0;
    loop {
        let (page, next) =
            client.list_names(bucket_id, prefix, Some("/"), start.as_deref(), PAGE)?;
        pages += 1;
        for file in &page {
            let Some(rest) = text(file, "fileName").strip_prefix(prefix) else {
                continue;
            };
            match text(file, "action") {
                "folder" => {
                    let name = rest.strip_suffix('/').unwrap_or(rest);
                    if valid_name(name) {
                        folders
                            .entry(name.into())
                            .or_insert_with(|| json!({"name": name, "kind": "dir"}));
                    }
                }
                "upload" if files.len() <= MAX_ROWS && rest != EMPTY_MARKER && valid_name(rest) => {
                    let size = crate::b2::size(file);
                    files
                        .entry(rest.into())
                        .or_insert_with(|| json!({"name": rest, "kind": "file", "size": size, "mtime": mtime(file)}));
                }
                _ => {}
            }
        }
        let Some(next) = next else { break };
        let full = folders.len() > MAX_ROWS || pages >= MAX_PAGES;
        if (folders.len() + files.len() > MAX_ROWS && full)
            || Instant::now() + Duration::from_secs(5) > deadline
        {
            more = true;
            break;
        }
        start = Some(next);
    }
    // A name can be both a file and a folder in B2; Flea needs unique names, so the folder wins.
    let mut rows: Vec<Value> = files
        .into_iter()
        .filter(|(name, _)| !folders.contains_key(name))
        .map(|(_, row)| row)
        .collect();
    rows.splice(0..0, folders.into_values());
    Ok(fit(rows, more))
}

/// Folders first, cut to Flea's row and byte bounds, with a marker row when cut.
pub fn fit(rows: Vec<Value>, more: bool) -> Vec<Value> {
    let marker = json!({"name": MORE_MARKER, "kind": "file", "size": 0});
    // A real object with the marker's own name would repeat a row name, which Flea refuses.
    let rows: Vec<Value> = rows
        .into_iter()
        .filter(|r| text(r, "name") != MORE_MARKER)
        .collect();
    let envelope = encoded(&json!({"op": "list", "entries": []}));
    let budget = MAX_BYTES - envelope - encoded(&marker) - 1;
    if !more && rows.len() <= MAX_ROWS {
        let total: usize = rows.iter().map(|r| encoded(r) + 1).sum();
        if total <= budget + encoded(&marker) + 1 {
            return rows;
        }
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for row in rows.into_iter().take(MAX_ROWS - 1) {
        let size = encoded(&row) + 1;
        if used + size > budget {
            break;
        }
        used += size;
        kept.push(row);
    }
    kept.push(marker);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(count: usize, width: usize) -> Vec<Value> {
        (0..count)
            .map(|i| json!({"name": format!("{i:0width$}"), "kind": "file", "size": 1, "mtime": 1}))
            .collect()
    }

    #[test]
    fn small_listing_untouched() {
        assert_eq!(fit(rows(256, 8), false), rows(256, 8));
    }

    #[test]
    fn row_cap_adds_marker() {
        let kept = fit(rows(300, 8), false);
        assert_eq!(kept.len(), 256);
        assert_eq!(text(&kept[255], "name"), MORE_MARKER);
    }

    #[test]
    fn more_pages_adds_marker() {
        let kept = fit(rows(10, 8), true);
        assert_eq!(kept.len(), 11);
        assert_eq!(text(&kept[10], "name"), MORE_MARKER);
    }

    #[test]
    fn byte_cap() {
        let kept = fit(rows(250, 250), false);
        assert!(kept.len() < 250);
        assert_eq!(text(kept.last().unwrap(), "name"), MORE_MARKER);
        assert!(json!({"op": "list", "entries": kept}).to_string().len() < MAX_BYTES + 100);
    }

    #[test]
    fn real_row_named_like_marker_is_dropped() {
        let mut list = rows(2, 8);
        list.push(json!({"name": MORE_MARKER, "kind": "file", "size": 0}));
        let names: Vec<_> = fit(list, false)
            .iter()
            .map(|r| text(r, "name").to_string())
            .collect();
        assert_eq!(names, ["00000000", "00000001"]);
    }
}
