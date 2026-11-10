//! The work behind copy, move, rename and delete, run by the detached worker.
//!
//! Flea always sends overwrite=false, so every job first checks its top-level targets and refuses
//! before any byte moves. A job stops at its first failure and says how far it got.
use crate::b2::{self, Client};
use crate::error::{Error, Result};
use crate::paths::{
    basename, check_key, folder_prefix, is_marker, object_key, parent_prefix, root_prefix,
    valid_local_segment, valid_name, EMPTY_MARKER,
};
use crate::util::{plural, quoted, random_hex, text, write_json_atomic};
use serde_json::{json, Value};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// The job's state for the waiting protocol process, replaced atomically and throttled.
pub struct Status {
    path: PathBuf,
    pub done: u64,
    pub total: u64,
    current: String,
    written: Option<Instant>,
}

impl Status {
    pub fn new(path: PathBuf) -> Self {
        let mut status = Status {
            path,
            done: 0,
            total: 0,
            current: String::new(),
            written: None,
        };
        status.write("running", "", true);
        status
    }

    fn set_total(&mut self, total: u64) {
        self.total = total;
        self.done = 0;
        self.write("running", "", true);
    }

    fn set_done(&mut self, done: u64) {
        self.done = done;
        self.write("running", "", false);
    }

    fn set_current(&mut self, current: &str) {
        self.current = current.to_string();
        self.write("running", "", false);
    }

    fn write(&mut self, state: &str, message: &str, force: bool) {
        if !force
            && self
                .written
                .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.written = Some(Instant::now());
        let _ = write_json_atomic(
            &self.path,
            &json!({"state": state, "done": self.done.min(self.total), "total": self.total,
                    "current": self.current, "message": message, "pid": std::process::id()}),
        );
    }

    pub fn finish(&mut self, state: &str, message: &str) {
        self.write(state, message, true);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Upload,
    Download,
    Remote,
    Rename,
    Delete,
}

pub fn kind(spec: &Value) -> Result<Kind> {
    let local = |key: &str| spec.get(key).and_then(Value::as_bool).unwrap_or(false);
    Ok(
        match (
            text(spec, "op"),
            local("sourceLocal"),
            local("destinationLocal"),
        ) {
            ("rename", _, _) => Kind::Rename,
            ("delete", _, _) => Kind::Delete,
            (_, true, false) => Kind::Upload,
            (_, false, true) => Kind::Download,
            (_, false, false) => Kind::Remote,
            (_, true, true) => {
                return Err(Error::msg("Both sides of this transfer are local folders."))
            }
        },
    )
}

/// The paths a job works on, without Flea's "more items" row.
pub fn selected(spec: &Value) -> Vec<&str> {
    let paths = spec
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    paths
        .filter_map(Value::as_str)
        .filter(|p| !is_marker(p))
        .collect()
}

/// A local file's name as the progress line and messages show it.
fn shown(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One end of a job inside B2: a bucket and the folder its entry shows.
#[derive(Clone)]
struct Side {
    bucket: String,
    bucket_id: String,
    root: String,
}

pub struct Job<'a> {
    spec: &'a Value,
    client: &'a mut Client,
    status: &'a mut Status,
    /// The entry the selection comes from, unless it is local.
    src: Option<Side>,
    /// The entry receiving the files, unless it is local.
    dst: Option<Side>,
    moving: bool,
    skipped: usize,
    count: usize,
    base: u64,
}

fn side(client: &mut Client, entry: Option<&Value>) -> Result<Option<Side>> {
    let Some(entry) = entry.filter(|e| e.is_object()) else {
        return Ok(None);
    };
    let bucket = text(entry, "bucket").to_string();
    Ok(Some(Side {
        bucket_id: client.bucket_id(&bucket)?,
        root: root_prefix(text(entry, "folder"))?,
        bucket,
    }))
}

impl<'a> Job<'a> {
    pub fn new(spec: &'a Value, client: &'a mut Client, status: &'a mut Status) -> Result<Self> {
        let src = side(client, spec.get("source"))?;
        let dst = side(client, spec.get("target"))?;
        Ok(Job {
            moving: text(spec, "op") == "move",
            spec,
            client,
            status,
            src,
            dst,
            skipped: 0,
            count: 0,
            base: 0,
        })
    }

    fn src(&self) -> Result<Side> {
        self.src
            .clone()
            .ok_or_else(|| Error::msg("The job names no source bucket."))
    }

    fn dst(&self) -> Result<Side> {
        self.dst
            .clone()
            .ok_or_else(|| Error::msg("The job names no destination bucket."))
    }

    pub fn run(&mut self) -> Result<String> {
        let paths = selected(self.spec);
        let dest = text(self.spec, "dest").to_string();
        match kind(self.spec)? {
            Kind::Delete => self.delete(&paths),
            Kind::Rename => self.rename(text(self.spec, "path"), text(self.spec, "to")),
            _ if paths.is_empty() => Err(Error::msg("Nothing to transfer.")),
            Kind::Upload => self.upload(&paths, &dest),
            Kind::Download => self.download(&paths, &dest),
            Kind::Remote => self.remote(&paths, &dest),
        }
    }

    // Shared steps.

    fn exists(&mut self, side: &Side, key: &str) -> Result<bool> {
        Ok(self.client.find_file(&side.bucket_id, key)?.is_some()
            || self
                .client
                .folder_exists(&side.bucket_id, &format!("{key}/"))?)
    }

    /// The file at key and every file under key/, as (B2 file, path below key).
    fn items(&mut self, side: &Side, key: &str) -> Result<Vec<(Value, String)>> {
        let mut found = Vec::new();
        if let Some(file) = self.client.find_file(&side.bucket_id, key)? {
            found.push((file, String::new()));
        }
        let prefix = format!("{key}/");
        for file in self.client.all_files(&side.bucket_id, &prefix)? {
            let below = text(&file, "fileName")[prefix.len()..].to_string();
            found.push((file, below));
        }
        Ok(found)
    }

    fn summary(&self, verb: &str) -> String {
        let mut text = format!("{verb} {}.", plural(self.count, "file"));
        if self.skipped > 0 {
            text += &format!(
                " Skipped {} that could not be transferred (links, special files or unusable names).",
                self.skipped
            );
        }
        text
    }

    fn failed(&self, error: Error, verb: &str) -> Error {
        if self.count == 0 {
            return error;
        }
        Error::msg(format!("{verb} {} before the failure. {error}", self.count))
    }

    // Upload: local files into the bucket.

    fn upload(&mut self, paths: &[&str], dest: &str) -> Result<String> {
        let dst = self.dst()?;
        let dest_prefix = folder_prefix(&dst.root, dest)?;
        let mut plan: Vec<(PathBuf, String, fs::Metadata)> = Vec::new();
        let mut markers = Vec::new();
        let mut folders = Vec::new();
        for path in paths {
            let name = basename(path).to_string();
            let top = check_key(format!("{dest_prefix}{name}"), &name)?;
            let meta = fs::metadata(path)
                .map_err(|e| Error::msg(format!("{} cannot be read: {e}.", quoted(&name))))?;
            if self.exists(&dst, &top)? {
                return Err(Error::msg(format!(
                    "{} already exists in the bucket.",
                    quoted(&name)
                )));
            }
            if meta.is_file() {
                plan.push((PathBuf::from(path), top, meta));
            } else if meta.is_dir() {
                self.walk(Path::new(path), &top, &mut plan, &mut markers, &mut folders)?;
            } else {
                return Err(Error::msg(format!(
                    "{} is not a regular file or folder.",
                    quoted(&name)
                )));
            }
        }
        self.status
            .set_total(plan.iter().map(|(_, _, m)| m.len()).sum());
        let result = (|| -> Result<()> {
            for (path, key, meta) in &plan {
                self.status.set_current(&shown(path));
                self.upload_one(&dst.bucket_id, path, key, meta)?;
                self.base += meta.len();
                self.count += 1;
                if self.moving {
                    let _ = fs::remove_file(path);
                }
            }
            for key in &markers {
                self.client.upload_file(
                    &dst.bucket_id,
                    key,
                    Path::new("/dev/null"),
                    0,
                    b2::EMPTY_SHA1,
                    crate::util::now_ms(),
                    &mut |_| {},
                )?;
            }
            Ok(())
        })();
        result.map_err(|e| self.failed(e, "Uploaded"))?;
        if self.moving {
            for folder in folders.iter().rev() {
                let _ = fs::remove_dir(folder); // Something skipped stays behind, and so does its folder.
            }
        }
        Ok(self.summary(if self.moving { "Moved" } else { "Uploaded" }))
    }

    fn walk(
        &mut self,
        folder: &Path,
        key: &str,
        plan: &mut Vec<(PathBuf, String, fs::Metadata)>,
        markers: &mut Vec<String>,
        folders: &mut Vec<PathBuf>,
    ) -> Result<()> {
        folders.push(folder.to_path_buf());
        let unreadable =
            |e: std::io::Error| Error::msg(format!("{} cannot be read: {e}.", folder.display()));
        let mut entries: Vec<_> = fs::read_dir(folder)
            .map_err(unreadable)?
            .collect::<std::io::Result<_>>()
            .map_err(unreadable)?;
        entries.sort_by_key(|e| e.file_name());
        let mut kept = 0;
        for entry in entries {
            let path = entry.path();
            let (Ok(meta), Some(name)) = (
                fs::symlink_metadata(&path),
                entry.file_name().to_str().map(String::from),
            ) else {
                self.skipped += 1;
                continue;
            };
            let Ok(child) = check_key(format!("{key}/{name}"), &name) else {
                self.skipped += 1;
                continue;
            };
            if meta.is_dir() {
                self.walk(&path, &child, plan, markers, folders)?;
                kept += 1;
            } else if meta.is_file() {
                plan.push((path, child, meta));
                kept += 1;
            } else {
                self.skipped += 1; // Links and special files.
            }
        }
        if kept == 0 {
            markers.push(check_key(format!("{key}/{EMPTY_MARKER}"), key)?);
        }
        Ok(())
    }

    fn upload_one(
        &mut self,
        bucket_id: &str,
        path: &Path,
        key: &str,
        meta: &fs::Metadata,
    ) -> Result<()> {
        let size = meta.len();
        let mtime_ms = meta.mtime() * 1000 + meta.mtime_nsec() / 1_000_000;
        let base = self.base;
        let status = &mut *self.status;
        let mut progress = |sent: u64| status.set_done(base + sent);
        match self.client.large_part(size) {
            None => {
                let (sha1, _) = b2::hash_file(path, size, None)?;
                self.client
                    .upload_file(bucket_id, key, path, size, &sha1, mtime_ms, &mut progress)
            }
            Some(part) => {
                let (whole, hashes) = b2::hash_file(path, size, Some(part))?;
                self.client.upload_large(
                    bucket_id,
                    key,
                    path,
                    size,
                    part,
                    &hashes,
                    &whole,
                    mtime_ms,
                    &mut progress,
                )
            }
        }
    }

    // Download: bucket files into a local folder.

    fn download(&mut self, paths: &[&str], dest: &str) -> Result<String> {
        let src = self.src()?;
        if !Path::new(dest).is_dir() {
            return Err(Error::msg(format!("{dest} is not a folder.")));
        }
        let mut plan: Vec<(Value, PathBuf)> = Vec::new();
        let mut folders: Vec<PathBuf> = Vec::new();
        let mut markers: Vec<Value> = Vec::new();
        for path in paths {
            let name = basename(path).to_string();
            let target = Path::new(dest).join(&name);
            if fs::symlink_metadata(&target).is_ok() {
                return Err(Error::msg(format!(
                    "{} already exists in {dest}.",
                    quoted(&name)
                )));
            }
            let found = self.items(&src, &object_key(&src.root, path)?)?;
            if found.is_empty() {
                return Err(Error::msg(format!(
                    "{} is no longer in the bucket.",
                    quoted(&name)
                )));
            }
            for (file, below) in found {
                if below.is_empty() {
                    plan.push((file, target.clone()));
                    continue;
                }
                folders.push(target.clone());
                let parts: Vec<&str> = below.split('/').collect();
                if !parts.iter().all(|p| valid_local_segment(p)) {
                    self.skipped += 1;
                    continue;
                }
                let mut local = target.clone();
                local.extend(&parts[..parts.len() - 1]);
                if parts[parts.len() - 1] == EMPTY_MARKER {
                    folders.push(local);
                    markers.push(file);
                } else {
                    local.push(parts[parts.len() - 1]);
                    plan.push((file, local));
                }
            }
        }
        self.status
            .set_total(plan.iter().map(|(f, _)| b2::size(f)).sum());
        let result = (|| -> Result<()> {
            for folder in &folders {
                fs::create_dir_all(folder)?;
            }
            for (file, target) in &plan {
                self.status.set_current(&shown(target));
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                self.download_one(&src.bucket, file, target)?;
                self.base += b2::size(file);
                self.count += 1;
                if self.moving {
                    self.client.hide(&src.bucket_id, text(file, "fileName"))?;
                }
            }
            if self.moving {
                for file in &markers {
                    self.client.hide(&src.bucket_id, text(file, "fileName"))?;
                }
            }
            Ok(())
        })();
        result.map_err(|e| self.failed(e, "Downloaded"))?;
        Ok(self.summary(if self.moving { "Moved" } else { "Downloaded" }))
    }

    fn download_one(&mut self, bucket: &str, file: &Value, target: &Path) -> Result<()> {
        let folder = target.parent().unwrap_or(Path::new("/"));
        let temporary = folder.join(format!(".flea-b2-{}.part", random_hex(8)));
        let mut out: File = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .open(&temporary)?;
        let shown = shown(target);
        let result = (|| -> Result<()> {
            let base = self.base;
            let status = &mut *self.status;
            let (headers, digest) =
                self.client
                    .download(bucket, text(file, "fileName"), &mut out, &mut |n| {
                        status.set_done(base + n)
                    })?;
            let mut expected = headers
                .get("x-bz-content-sha1")
                .cloned()
                .unwrap_or_default();
            if let Some(rest) = expected.strip_prefix("unverified:") {
                expected = rest.to_string();
            }
            if expected == "none" {
                expected = headers
                    .get("x-bz-info-large_file_sha1")
                    .cloned()
                    .unwrap_or_default();
            }
            if expected.len() == 40 && !expected.eq_ignore_ascii_case(&digest) {
                return Err(Error::msg(format!(
                    "{} arrived damaged (checksum mismatch).",
                    quoted(&shown)
                )));
            }
            let millis = headers
                .get("x-bz-info-src_last_modified_millis")
                .or_else(|| headers.get("x-bz-upload-timestamp"))
                .and_then(|v| v.parse::<u64>().ok());
            if let Some(ms) = millis {
                let _ = out.set_times(
                    FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(ms)),
                );
            }
            out.sync_all()?;
            publish(&temporary, target, &shown)
        })();
        let _ = fs::remove_file(&temporary);
        result
    }

    // Copy and move inside the bucket.

    fn remote(&mut self, paths: &[&str], dest: &str) -> Result<String> {
        let (src, dst) = (self.src()?, self.dst()?);
        let dest_prefix = folder_prefix(&dst.root, dest)?;
        let mut pairs = Vec::new();
        for path in paths {
            let name = basename(path).to_string();
            let key = object_key(&src.root, path)?;
            if src.bucket_id == dst.bucket_id && dest_prefix.starts_with(&format!("{key}/")) {
                return Err(Error::msg(format!(
                    "{} cannot go inside itself.",
                    quoted(&name)
                )));
            }
            pairs.push((key, format!("{dest_prefix}{name}"), name));
        }
        let verb = if self.moving { "Moved" } else { "Copied" };
        self.copy_all(&src, &dst, pairs, verb)
    }

    fn rename(&mut self, path: &str, to: &str) -> Result<String> {
        if !valid_name(to) {
            return Err(Error::msg(format!("{} is not a usable name.", quoted(to))));
        }
        if is_marker(path) {
            return Err(Error::msg(
                "This row only says that the folder holds more items.",
            ));
        }
        let src = self.src()?;
        let key = object_key(&src.root, path)?;
        let new_key = format!("{}{to}", parent_prefix(&key));
        if new_key == key {
            return Ok("Nothing to rename.".into());
        }
        self.moving = true;
        self.copy_all(
            &src,
            &src,
            vec![(key, new_key, basename(path).to_string())],
            "Renamed",
        )
    }

    fn copy_all(
        &mut self,
        src: &Side,
        dst: &Side,
        pairs: Vec<(String, String, String)>,
        verb: &str,
    ) -> Result<String> {
        let mut plan = Vec::new();
        for (key, new_key, name) in pairs {
            let new_key = check_key(new_key, &name)?;
            if self.exists(dst, &new_key)? {
                return Err(Error::msg(format!(
                    "{} already exists in the bucket.",
                    quoted(basename(&new_key))
                )));
            }
            let found = self.items(src, &key)?;
            if found.is_empty() {
                return Err(Error::msg(format!(
                    "{} is no longer in the bucket.",
                    quoted(&name)
                )));
            }
            for (file, below) in found {
                let target = if below.is_empty() {
                    new_key.clone()
                } else {
                    format!("{new_key}/{below}")
                };
                plan.push((file, check_key(target, &name)?));
            }
        }
        self.status
            .set_total(plan.iter().map(|(f, _)| b2::size(f)).sum());
        let result = (|| -> Result<()> {
            for (file, new_key) in &plan {
                self.status.set_current(basename(text(file, "fileName")));
                let base = self.base;
                let status = &mut *self.status;
                self.client.copy(file, new_key, &dst.bucket_id, &mut |n| {
                    status.set_done(base + n)
                })?;
                self.base += b2::size(file);
                self.count += 1;
                if self.moving {
                    self.client.hide(&src.bucket_id, text(file, "fileName"))?;
                }
            }
            Ok(())
        })();
        result.map_err(|e| self.failed(e, verb))?;
        Ok(self.summary(verb))
    }

    // Delete hides: B2 keeps older versions until the bucket's lifecycle rules remove them.

    fn delete(&mut self, paths: &[&str]) -> Result<String> {
        let src = self.src()?;
        let mut names = Vec::new();
        for path in paths {
            for (file, _) in self.items(&src, &object_key(&src.root, path)?)? {
                names.push(text(&file, "fileName").to_string());
            }
        }
        self.status.set_total(names.len() as u64);
        let result = (|| -> Result<()> {
            for (number, name) in names.iter().enumerate() {
                self.status.set_current(basename(name));
                self.client.hide(&src.bucket_id, name)?;
                self.count = number + 1;
                self.status.set_done(self.count as u64);
            }
            Ok(())
        })();
        result.map_err(|e| self.failed(e, "Deleted"))?;
        Ok(self.summary("Deleted"))
    }
}

/// Gives the finished file its name without ever replacing one that appeared meanwhile.
fn publish(temporary: &Path, target: &Path, shown: &str) -> Result<()> {
    let appeared = || {
        Error::msg(format!(
            "{} appeared in the destination meanwhile.",
            quoted(shown)
        ))
    };
    match fs::hard_link(temporary, target) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(appeared()),
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::EPERM | libc::EOPNOTSUPP | libc::ENOSYS | libc::EXDEV | libc::EMLINK)
            ) => {}
        Err(e) => return Err(e.into()),
    }
    // A filesystem without hard links (FAT, some FUSE mounts): check, then rename.
    if fs::symlink_metadata(target).is_ok() {
        return Err(appeared());
    }
    fs::rename(temporary, target)?;
    Ok(())
}
