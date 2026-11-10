//! The detached worker: runs one job spec and leaves its result in the status file.
use crate::b2::Client;
use crate::error::Error;
use crate::jobs::{describe, Files};
use crate::notify;
use crate::ops::{Job, Status};
use crate::util::{clean_text, log, read_json};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

fn work(spec: &Value, status: &mut Status) -> Result<String, Error> {
    let key_id = crate::jobs::key_id(spec).to_string();
    let key = crate::creds::lookup(&key_id, Duration::from_secs(120))?;
    let mut client = Client::new(key_id, key);
    client.authorize()?;
    Job::new(spec, &mut client, status)?.run()
}

pub fn main(spec_path: &Path) -> i32 {
    let files = Files::for_spec(spec_path);
    let mut status = Status::new(files.status.clone());
    let spec = read_json(&files.spec).unwrap_or(Value::Null);
    let outcome = if spec.is_object() {
        // A panic still ends with a status, so the waiting side never reports a silent stop.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&spec, &mut status)))
            .unwrap_or_else(|_| Err(Error::msg("The B2 worker hit an internal error.")))
    } else {
        Err(Error::msg("The job description could not be read."))
    };
    let (state, message) = match &outcome {
        Ok(message) => ("done", clean_text(message, 1000)),
        Err(error) => {
            log(&format!("job {} failed: {error}", spec_path.display()));
            ("error", clean_text(&error.to_string(), 1000))
        }
    };
    status.finish(state, &message);
    if files.flag.exists() {
        // The protocol process stopped waiting; this notification is the only report.
        let (_, noun) = describe(&spec);
        if outcome.is_ok() {
            notify::send(&message, false);
        } else {
            notify::send(&format!("{noun} failed: {message}"), true);
        }
        files.remove();
    }
    i32::from(outcome.is_err())
}
