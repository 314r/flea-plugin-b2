//! Opening the graphical bucket manager from Flea's sidebar.
//!
//! The sidebar's last row opens `plugin://b2.bucket/.manage`, and listing that path starts the
//! manager. It cannot be a child of the plugin: Flea gives the plugin a 512 MiB address-space limit
//! that every child inherits and none can raise, too little for a GPU-drawn window, and it kills
//! the plugin's whole process group when it stops it. The user's systemd manager starts it instead,
//! with ordinary limits and a life of its own. The manager keeps a single window: a second start
//! raises the first window and exits.
use crate::error::{Error, Result};
use crate::util::hook;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The sidebar row's id and the place path it opens. A name starting with a dot is never a bucket
/// entry's (`Config::check_name` refuses it), so the two cannot meet.
pub const ROW_ID: &str = ".manage";
pub const ROW_LABEL: &str = "Manage buckets…";

/// What the manager needs from the session, passed on when the plugin has it: the unit gets only
/// these, not the plugin's environment (whose `LC_ALL=C` Flea set). The display variables are
/// usually in the systemd manager already; `XDG_CONFIG_HOME` must match so that the plugin and
/// the manager use the same bucket list.
const PASSED: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_CONFIG_HOME",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "LANG",
];

/// Is this place path the manager row's?
pub fn is_manage_path(path: &str) -> bool {
    path.trim_end_matches('/') == format!("/{ROW_ID}")
}

/// The manager's binary, next to `flea-b2` in the plugin's `bin/`. Checked here, because
/// systemd-run does not wait to see whether it starts.
fn manager_path() -> Result<PathBuf> {
    let path = match hook("FAKE_MANAGER_BIN") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe()?.with_file_name("flea-b2-manager"),
    };
    let executable = std::fs::metadata(&path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !executable {
        return Err(Error::msg(format!(
            "The bucket manager is missing from {}. Rebuild the plugin with make.",
            path.display()
        )));
    }
    Ok(path)
}

/// Starts the manager as a transient systemd user service, without waiting for its window.
pub fn open_manager() -> Result<()> {
    let path = manager_path()?;
    let mut command =
        Command::new(hook("FAKE_SYSTEMD_RUN").unwrap_or_else(|| "systemd-run".into()));
    command.args([
        "--user",
        "--collect",
        "--quiet",
        "--no-block",
        "--description=Backblaze B2 bucket manager for Flea",
    ]);
    for name in PASSED {
        if std::env::var_os(name).is_some() {
            // A name without a value copies it from systemd-run's environment, which is ours.
            command.arg(format!("--setenv={name}"));
        }
    }
    command
        .arg("--")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = command.output().map_err(|error| {
        Error::msg(format!(
            "The bucket manager could not start: systemd-run is unavailable ({error}). Run {} from a launcher instead.",
            path.display()
        ))
    })?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        return Err(Error::msg(format!(
            "The bucket manager could not start: {}",
            crate::util::clean_text(reason.trim(), 300)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_row_path_opens_the_manager() {
        assert!(is_manage_path("/.manage"));
        assert!(is_manage_path("/.manage/"));
        assert!(!is_manage_path("/"));
        assert!(!is_manage_path("/.manage/inside"));
        assert!(!is_manage_path("/Photos"));
    }
}
