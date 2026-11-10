//! Changes run in a detached worker, so a long transfer survives Flea's 30-second request limit.
//!
//! The protocol process waits for the worker while it can, relaying progress. Past that, it
//! answers done, the user gets a notification that the job continues, and the worker sends
//! another when it ends. Flea kills the plugin's process group when it goes idle; the worker
//! runs in its own session, outside that group.
use crate::config::{Config, Entry};
use crate::error::{Error, Result};
use crate::notify;
use crate::ops::{kind, selected, Kind};
use crate::paths::basename;
use crate::util::{
    clean_text, hook, jobs_dir, log, log_path, plural, quoted, random_hex, read_json, text,
    write_json_atomic,
};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

const STALE: Duration = Duration::from_secs(7 * 24 * 3600);

fn wait_time() -> Duration {
    let seconds = hook("FLEA_B2_WAIT_SECONDS")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(24.0);
    Duration::from_secs_f64(seconds.clamp(0.1, 24.0))
}

/// ("Uploading “a.txt”", "Upload of “a.txt”") for notifications.
pub fn describe(spec: &Value) -> (String, String) {
    let work = kind(spec).unwrap_or(Kind::Remote);
    let (mut doing, mut noun) = match work {
        Kind::Upload => ("Uploading", "Upload"),
        Kind::Download => ("Downloading", "Download"),
        Kind::Remote => ("Copying", "Copy"),
        Kind::Rename => ("Renaming", "Rename"),
        Kind::Delete => ("Deleting", "Delete"),
    };
    if text(spec, "op") == "move" && work != Kind::Rename {
        (doing, noun) = ("Moving", "Move");
    }
    let paths = if work == Kind::Rename {
        vec![text(spec, "path")]
    } else {
        selected(spec)
    };
    let what = match paths.as_slice() {
        [one] => quoted(basename(one)),
        many => plural(many.len(), "item"),
    };
    (format!("{doing} {what}"), format!("{noun} of {what}"))
}

/// The entry and inner path of a place path that must lie inside a bucket, not be one.
fn inside<'a>(config: &'a Config, path: &str, what: &str) -> Result<(&'a Entry, String)> {
    match config.split(path)? {
        (Some(entry), inner) if inner != "/" => Ok((entry, inner)),
        (Some(entry), _) => {
            let name = quoted(&entry.name);
            Err(Error::msg(match what {
                "remove" => format!("{name} is a whole bucket. Take it out of Flea under “Manage buckets…” (its files stay in B2), or open it to delete files."),
                "rename" => format!("Rename {name} under “Manage buckets…”."),
                _ => format!("{name} is a whole bucket: open it and select what to {what}."),
            }))
        }
        (None, _) => Err(Error::msg("Open one of the buckets first.")),
    }
}

/// The entry and inner path of a destination folder: a bucket entry itself is fine.
fn folder<'a>(config: &'a Config, path: &str) -> Result<(&'a Entry, String)> {
    match config.split(path)? {
        (Some(entry), inner) => Ok((entry, inner)),
        (None, _) => Err(Error::msg(
            "Open one of the buckets first: files go inside a bucket.",
        )),
    }
}

/// The selection's entry, which every path must share, and the paths inside it.
fn selection<'a>(
    config: &'a Config,
    paths: &[String],
    what: &str,
) -> Result<(&'a Entry, Vec<String>)> {
    let mut entry: Option<&Entry> = None;
    let mut inner = Vec::new();
    for path in paths {
        let (found, path) = inside(config, path, what)?;
        if entry.is_some_and(|e| e.name != found.name) {
            return Err(Error::msg("The selection spans several buckets."));
        }
        entry = Some(found);
        inner.push(path);
    }
    entry
        .map(|e| (e, inner))
        .ok_or_else(|| Error::msg("Flea sent no paths."))
}

pub fn spec_for(request: &Value, config: &Config) -> Result<Value> {
    let op = text(request, "op");
    let paths: Vec<String> = request
        .get("paths")
        .and_then(Value::as_array)
        .filter(|l| l.iter().all(Value::is_string))
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .ok_or_else(|| Error::msg("Flea sent no paths."))?;
    let flag = |key: &str| request.get(key).and_then(Value::as_bool).unwrap_or(false);
    let mut spec = json!({"op": op, "source": null, "target": null});
    match op {
        "delete" => {
            let (entry, inner) = selection(config, &paths, "remove")?;
            spec["source"] = entry.to_json();
            spec["paths"] = json!(inner);
        }
        "rename" => {
            let (Some(path), Some(to)) = (
                request.get("path").and_then(Value::as_str),
                request.get("to").and_then(Value::as_str),
            ) else {
                return Err(Error::msg("Flea sent an incomplete rename."));
            };
            let (entry, inner) = inside(config, path, "rename")?;
            spec["source"] = entry.to_json();
            spec["path"] = inner.into();
            spec["to"] = to.into();
        }
        "copy" | "move" => {
            let dest = request
                .get("dest")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::msg("Flea sent no destination."))?;
            let (source_local, destination_local) = (flag("sourceLocal"), flag("destinationLocal"));
            spec["sourceLocal"] = source_local.into();
            spec["destinationLocal"] = destination_local.into();
            let source = if source_local {
                spec["paths"] = json!(paths);
                None
            } else {
                let (entry, inner) = selection(config, &paths, op)?;
                spec["source"] = entry.to_json();
                spec["paths"] = json!(inner);
                Some(entry)
            };
            if destination_local {
                spec["dest"] = dest.into();
            } else {
                let (entry, inner) = folder(config, dest)?;
                if let Some(source) = source.filter(|s| s.key_id != entry.key_id) {
                    return Err(Error::msg(format!(
                        "{} and {} use different keys, so B2 cannot copy between them. Copy through a local folder.",
                        quoted(&source.name),
                        quoted(&entry.name)
                    )));
                }
                spec["target"] = entry.to_json();
                spec["dest"] = inner.into();
            }
        }
        _ => return Err(Error::msg("Backblaze B2 does not support this request.")),
    }
    kind(&spec)?;
    Ok(spec)
}

/// The key ID a job's B2 calls use.
pub fn key_id(spec: &Value) -> &str {
    let from = |side: &str| spec.get(side).map(|e| text(e, "keyId")).unwrap_or("");
    match from("source") {
        "" => from("target"),
        id => id,
    }
}

pub struct Files {
    pub spec: PathBuf,
    pub status: PathBuf,
    pub flag: PathBuf,
}

impl Files {
    pub fn for_spec(spec: &Path) -> Self {
        let base = spec.with_extension("");
        Files {
            spec: spec.into(),
            status: base.with_extension("status"),
            flag: base.with_extension("detached"),
        }
    }

    pub fn remove(&self) {
        for path in [&self.spec, &self.status, &self.flag] {
            let _ = fs::remove_file(path);
        }
    }
}

fn sweep(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > STALE);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn final_answer(status: &Value, files: &Files) -> Option<Value> {
    let state = text(status, "state");
    if state != "done" && state != "error" {
        return None;
    }
    files.remove();
    let message = clean_text(text(status, "message"), 1000);
    Some(match (state, message.is_empty()) {
        ("error", true) => json!({"op": "error", "message": "The B2 operation failed."}),
        ("error", false) => json!({"op": "error", "message": message}),
        (_, true) => json!({"op": "done"}),
        (_, false) => json!({"op": "done", "message": message}),
    })
}

/// Workers this process started; collected as they end so none lingers as a zombie.
#[derive(Default)]
pub struct Workers(Vec<Child>);

impl Workers {
    pub fn reap(&mut self) {
        self.0
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    }

    /// Runs one change and returns Flea's final answer; emit sends progress lines.
    pub fn run(
        &mut self,
        request: &Value,
        config: &Config,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Value> {
        self.reap();
        let spec = spec_for(request, config)?;
        let folder = jobs_dir()?;
        sweep(&folder);
        let files = Files::for_spec(&folder.join(format!("{}.json", random_hex(16))));
        write_json_atomic(&files.spec, &spec)?;
        let started = Instant::now();
        let stderr = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path())?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("worker")
            .arg(&files.spec)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        // Its own session: out of the process group Flea kills, and free of this process's end.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = command.spawn().map_err(|e| {
            files.remove();
            Error::msg(format!("The B2 worker could not start: {e}"))
        })?;
        let pid = child.id();
        self.0.push(child);
        let deadline = started + wait_time();
        let mut sent = 0;
        let mut last: Option<(i64, i64, String)> = None;
        let mut last_time: Option<Instant> = None;
        loop {
            let status = read_json(&files.status).unwrap_or(Value::Null);
            if let Some(answer) = final_answer(&status, &files) {
                return Ok(answer);
            }
            let running = self
                .0
                .iter_mut()
                .find(|c| c.id() == pid)
                .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
            if !running {
                let status = read_json(&files.status).unwrap_or(Value::Null);
                if let Some(answer) = final_answer(&status, &files) {
                    return Ok(answer);
                }
                files.remove();
                return Err(Error::msg(format!(
                    "The B2 worker stopped unexpectedly. See {}.",
                    log_path().display()
                )));
            }
            if Instant::now() >= deadline {
                break;
            }
            let total = status
                .get("total")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0);
            let done = status
                .get("done")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .clamp(0, total);
            let current = text(&status, "current").to_string();
            let update = (done, total, current.clone());
            let due = last_time.is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
            if (total > 0 || !current.is_empty())
                && last.as_ref() != Some(&update)
                && due
                && sent < 60
            {
                emit(
                    json!({"op": "progress", "done": done, "total": total, "message": clean_text(&current, 200)}),
                );
                sent += 1;
                last = Some(update);
                last_time = Some(Instant::now());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // Still running: from here the worker reports through notifications.
        let _ = fs::write(&files.flag, b"");
        let status = read_json(&files.status).unwrap_or(Value::Null);
        if let Some(answer) = final_answer(&status, &files) {
            return Ok(answer);
        }
        let (doing, _) = describe(&spec);
        notify::send(
            &format!("{doing} continues in the background. A notification will say when it ends."),
            false,
        );
        log(&format!(
            "job {} continues in the background",
            files.spec.display()
        ));
        Ok(json!({"op": "done", "message": format!("{doing} continues in the background.")}))
    }
}
