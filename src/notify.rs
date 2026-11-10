//! Desktop notifications, over D-Bus, for work that outlives Flea's 30-second request.
use crate::util::{clean_text, hook, log};
use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;
use zbus::blocking::{connection, Proxy};
use zbus::zvariant::Value;

/// Best effort: a missing notification daemon never fails the operation.
pub fn send(body: &str, critical: bool) {
    let body = clean_text(body, 500);
    if let Some(path) = hook("FAKE_NOTIFY_LOG") {
        if let Ok(mut out) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(out, "{}", body.replace('\n', " "));
        }
        return;
    }
    let result = (|| -> zbus::Result<u32> {
        let conn = connection::Builder::session()?
            .method_timeout(Duration::from_secs(5))
            .build()?;
        let proxy = Proxy::new(
            &conn,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )?;
        let hints: HashMap<&str, Value> =
            HashMap::from([("urgency", Value::U8(if critical { 2 } else { 1 }))]);
        proxy.call(
            "Notify",
            &(
                "Flea",
                0u32,
                "folder-remote",
                "Backblaze B2",
                body.as_str(),
                Vec::<&str>::new(),
                hints,
                -1i32,
            ),
        )
    })();
    if let Err(error) = result {
        log(&format!("notification failed: {error}"));
    }
}
