//! Backblaze B2 buckets for the Flea file manager.
//!
//! Started by Flea without arguments, this is the plugin: JSON lines on stdin, one request at a
//! time, answers on stdout. stdout carries protocol lines only, and every failure becomes an
//! ordinary error answer, because a malformed answer or a crash would mark the plugin failed.
use crate::config::{self, Config, Entry};
use crate::error::{Error, Result};
use crate::{b2, creds, jobs, launch, listing, paths, util, worker};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::time::{Duration, Instant, SystemTime};

const LIST_BUDGET: Duration = Duration::from_secs(20);
const KEYRING_WAIT: Duration = Duration::from_secs(12);
const ID: &str = "b2.bucket";

const USAGE: &str = "\
flea-b2: Backblaze B2 buckets in the Flea file manager.

Flea starts flea-b2 with its input and output on pipes and speaks its plugin protocol to it.
The buckets are managed in flea-b2-manager, which Flea's “Manage buckets…” row opens.";

struct Plugin {
    config: Config,
    loaded: Option<SystemTime>,
    /// One authorized client per key ID, and the bucket IDs it has looked up.
    clients: HashMap<String, b2::Client>,
    buckets: HashMap<(String, String), String>,
    workers: jobs::Workers,
}

impl Plugin {
    /// Reads the bucket list again when the manager changed it, and forgets what was cached for it.
    fn refresh(&mut self) -> Result<()> {
        let modified = config::modified();
        if self.loaded.is_none() || modified != self.loaded {
            self.config = Config::load()?;
            self.loaded = modified.or(Some(SystemTime::UNIX_EPOCH));
            self.clients.clear();
            self.buckets.clear();
        }
        Ok(())
    }

    fn handle(&mut self, request: &Value, emit: &mut dyn FnMut(Value)) -> Result<Value> {
        match util::text(request, "op") {
            "hello" => Ok(json!({"op": "ready", "protocolVersion": 1})),
            "settings" => Ok(json!({"op": "done"})),
            "section" => {
                self.refresh()?;
                Ok(json!({"op": "section", "rows": self.rail_rows()}))
            }
            // The manager row: its listing opens the manager's window and shows nothing itself.
            "list" if launch::is_manage_path(util::text(request, "path")) => {
                launch::open_manager()?;
                Ok(json!({"op": "list", "entries": []}))
            }
            "list" => {
                self.refresh()?;
                self.list(util::text(request, "path"))
            }
            "copy" | "move" | "rename" | "delete" => {
                self.refresh()?;
                self.workers.run(request, &self.config, emit)
            }
            _ => {
                Ok(json!({"op": "error", "message": "Backblaze B2 does not support this request."}))
            }
        }
    }

    /// One sidebar row per bucket entry, opening it inside the place, then the manager's row.
    fn rail_rows(&self) -> Vec<Value> {
        let mut rows: Vec<Value> = self
            .config
            .buckets
            .iter()
            .map(|e| json!({"id": e.name, "label": e.name, "glyph": "cloud", "opens": format!("plugin://{ID}/{}", e.name)}))
            .collect();
        rows.push(manager_row());
        rows
    }

    fn list(&mut self, path: &str) -> Result<Value> {
        let (entry, inner) = match self.config.split(path)? {
            (None, _) if self.config.buckets.is_empty() => {
                return Err(Error::msg(config::EMPTY_HINT))
            }
            (None, _) => {
                let rows: Vec<Value> = self
                    .config
                    .buckets
                    .iter()
                    .map(|e| json!({"name": e.name, "kind": "dir"}))
                    .collect();
                return Ok(json!({"op": "list", "entries": rows}));
            }
            (Some(entry), inner) => (entry.clone(), inner),
        };
        let deadline = Instant::now() + LIST_BUDGET;
        let prefix = paths::folder_prefix(&entry.root(), &inner)?;
        let client = client(&mut self.clients, &entry, deadline)?;
        let cache = (entry.key_id.clone(), entry.bucket.clone());
        let result = (|| {
            if !self.buckets.contains_key(&cache) {
                let id = client.bucket_id(&entry.bucket)?;
                self.buckets.insert(cache.clone(), id);
            }
            listing::entries(client, &self.buckets[&cache], &prefix, deadline)
        })();
        client.deadline = None;
        Ok(json!({"op": "list", "entries": result?}))
    }
}

/// The sidebar row that opens the bucket manager.
fn manager_row() -> Value {
    json!({
        "id": launch::ROW_ID,
        "label": launch::ROW_LABEL,
        "glyph": "server",
        "opens": format!("plugin://{ID}/{}", launch::ROW_ID),
    })
}

/// The authorized client of an entry's key, kept for the next requests.
fn client<'a>(
    clients: &'a mut HashMap<String, b2::Client>,
    entry: &Entry,
    deadline: Instant,
) -> Result<&'a mut b2::Client> {
    if !clients.contains_key(&entry.key_id) {
        let key = creds::lookup(&entry.key_id, KEYRING_WAIT)?;
        let mut client = b2::Client::new(entry.key_id.clone(), key);
        client.deadline = Some(deadline);
        client.authorize()?;
        clients.insert(entry.key_id.clone(), client);
    }
    let client = clients.get_mut(&entry.key_id).expect("client");
    client.deadline = Some(deadline);
    Ok(client)
}

fn write_line(out: &mut impl Write, message: &Value) -> std::io::Result<()> {
    out.write_all(message.to_string().as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

fn serve() -> i32 {
    let mut plugin = Plugin {
        config: Config::default(),
        loaded: None,
        clients: HashMap::new(),
        buckets: HashMap::new(),
        workers: Default::default(),
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(request) = serde_json::from_slice::<Value>(&line) else {
            util::log("ignored an unreadable request line");
            continue;
        };
        if !request.is_object() {
            continue;
        }
        if util::text(&request, "op") == "cancel" {
            break;
        }
        let mut out = std::io::stdout().lock();
        let mut broken = false;
        let mut emit = |message: Value| broken |= write_line(&mut out, &message).is_err();
        let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            plugin.handle(&request, &mut emit)
        }))
        .unwrap_or_else(|_| Err(Error::msg("The plugin hit an internal error.")))
        .unwrap_or_else(|error| {
            util::log(&format!("{} failed: {error}", util::text(&request, "op")));
            // A section answer that is an error would empty the rail rows. An unreadable bucket list
            // still leaves the row that opens the manager, which says what is wrong.
            if util::text(&request, "op") == "section" {
                return json!({"op": "section", "rows": [manager_row()]});
            }
            json!({"op": "error", "message": util::clean_text(&error.to_string(), 1000)})
        });
        if broken || write_line(&mut out, &answer).is_err() {
            break;
        }
    }
    plugin.workers.reap();
    0
}

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.as_slice() {
        // In a terminal, waiting for protocol lines would only look stuck.
        [] if !std::io::stdin().is_terminal() => serve(),
        [op, job] if op == "worker" => worker::main(std::path::Path::new(job)),
        [help] if matches!(help.as_str(), "-h" | "--help" | "help") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}
