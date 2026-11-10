//! Folders, the log and small helpers shared by the protocol process, the worker and the manager.
use serde_json::Value;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const APP: &str = "flea-b2";
const LOG_LIMIT: u64 = 1 << 20;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn base(variable: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(variable).map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => home().join(fallback),
    }
}

pub fn private(path: PathBuf) -> std::io::Result<PathBuf> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)?;
    Ok(path)
}

/// A 0700 folder for job files: the runtime folder (tmpfs) when there is one.
pub fn jobs_dir() -> std::io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let base = match runtime {
        Some(path) if path.is_absolute() => path,
        _ => base("XDG_CACHE_HOME", ".cache"),
    };
    private(base.join(APP).join("jobs"))
}

pub fn log_path() -> PathBuf {
    let folder = base("XDG_STATE_HOME", ".local/state").join(APP);
    let _ = private(folder.clone());
    folder.join("plugin.log")
}

/// Flea discards stderr, so diagnostics go to our own small log. Never log secrets.
pub fn log(message: &str) {
    let path = log_path();
    if fs::metadata(&path)
        .map(|m| m.len() > LOG_LIMIT)
        .unwrap_or(false)
    {
        let _ = fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut out) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
    {
        let _ = writeln!(out, "{} [{}] {}", timestamp(), std::process::id(), message);
    }
}

/// UTC time as YYYY-MM-DD HH:MM:SS, without a date library.
pub fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn write_json_atomic(path: &Path, value: &Value) -> std::io::Result<()> {
    let tmp = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    out.write_all(value.to_string().as_bytes())?;
    drop(out);
    fs::rename(&tmp, path)
}

pub fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Plain text Flea accepts: no NUL or control characters but newline, bounded in characters.
pub fn clean_text(text: &str, limit: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect();
    if cleaned.chars().count() <= limit {
        return cleaned;
    }
    let mut cut: String = cleaned.chars().take(limit.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

pub fn quoted(name: &str) -> String {
    format!("“{name}”")
}

pub fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("{count} {word}")
    } else {
        format!("{count} {word}s")
    }
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf)
        .expect("system random");
    hex(&buf)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A test-only environment value; the shipped binary never reads these.
#[cfg(feature = "test-hooks")]
pub fn hook(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(not(feature = "test-hooks"))]
pub fn hook(_name: &str) -> Option<String> {
    None
}

/// Integers B2 sends either as JSON numbers or as strings.
pub fn int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
