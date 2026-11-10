//! The B2 key lives only in the keyring, reached through the Secret Service D-Bus API
//! (gnome-keyring, KeePassXC and others implement it). No helper program is needed.
use crate::error::{Error, Result};
use crate::util::hook;
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;
use zbus::blocking::{connection, Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.secrets";
const SERVICE: &str = "flea-b2";
/// How long one keyring call may take; a service that cannot start fails within this.
const CALL_WAIT: Duration = Duration::from_secs(10);

pub fn missing_key(key_id: &str) -> String {
    format!("The key {key_id} is not in the keyring. Choose “Manage buckets…” under Backblaze B2 and change this bucket's key.")
}

type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

fn bus(error: zbus::Error) -> Error {
    if let zbus::Error::MethodError(name, _, _) = &error {
        match name.as_str() {
            "org.freedesktop.DBus.Error.ServiceUnknown" => {
                return Error::msg(
                    "No keyring service is running. Start gnome-keyring or another Secret Service.",
                )
            }
            "org.freedesktop.Secret.Error.IsLocked" => {
                return Error::msg("The keyring is locked. Unlock it, then retry.")
            }
            _ => {}
        }
    }
    if matches!(error, zbus::Error::InputOutput(_)) || error.to_string().contains("timed out") {
        return Error::msg("The keyring did not answer. Unlock it, then retry.");
    }
    Error::msg(format!("The keyring could not be used: {error}"))
}

struct Keyring {
    conn: Connection,
    service: Proxy<'static>,
    session: OwnedObjectPath,
    wait: Duration,
}

impl Keyring {
    /// wait bounds every keyring call, including the time the user takes to answer an unlock prompt.
    fn open(wait: Duration) -> Result<Self> {
        let conn = connection::Builder::session()
            // Each call answers at once; only a prompt waits for the user, for up to `wait`.
            .and_then(|b| b.method_timeout(wait.min(CALL_WAIT)).build())
            .map_err(bus)?;
        let service = Proxy::new(
            &conn,
            DEST,
            "/org/freedesktop/secrets",
            "org.freedesktop.Secret.Service",
        )
        .map_err(bus)?;
        // "plain" sends the secret unencrypted over the user's own session bus, as the spec allows.
        let (_, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(bus)?;
        Ok(Keyring {
            conn,
            service,
            session,
            wait,
        })
    }

    fn object(&self, path: &OwnedObjectPath, interface: &'static str) -> Result<Proxy<'static>> {
        Proxy::new(&self.conn, DEST, path.as_str().to_owned(), interface).map_err(bus)
    }

    fn search(&self, key_id: &str) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)> {
        let attributes = HashMap::from([("service", SERVICE), ("key-id", key_id)]);
        self.service
            .call("SearchItems", &(attributes,))
            .map_err(bus)
    }

    fn unlock(&self, objects: Vec<OwnedObjectPath>) -> Result<()> {
        if objects.is_empty() {
            return Ok(());
        }
        let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
            self.service.call("Unlock", &(objects,)).map_err(bus)?;
        self.prompt(prompt)
    }

    /// Shows the keyring's own prompt (an unlock password, a confirmation) and waits for its answer.
    fn prompt(&self, path: OwnedObjectPath) -> Result<()> {
        if path.as_str() == "/" {
            return Ok(());
        }
        let proxy = self.object(&path, "org.freedesktop.Secret.Prompt")?;
        let mut signals = proxy.receive_signal("Completed").map_err(bus)?;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let dismissed = signals
                .next()
                .and_then(|m| m.body().deserialize::<(bool, OwnedValue)>().ok())
                .map(|(d, _)| d);
            let _ = sender.send(dismissed);
        });
        proxy.call_method("Prompt", &("",)).map_err(bus)?;
        match receiver.recv_timeout(self.wait) {
            Ok(Some(false)) => Ok(()),
            Ok(Some(true)) => Err(Error::msg("The keyring prompt was cancelled.")),
            _ => Err(Error::msg("The keyring is locked. Unlock it, then retry.")),
        }
    }

    fn secret(&self, key_id: &str) -> Result<Option<String>> {
        let (unlocked, locked) = self.search(key_id)?;
        let items = if unlocked.is_empty() && !locked.is_empty() {
            self.unlock(locked.clone())?;
            locked
        } else {
            unlocked
        };
        let Some(item) = items.into_iter().next() else {
            return Ok(None);
        };
        let secrets: HashMap<OwnedObjectPath, Secret> = self
            .service
            .call("GetSecrets", &(vec![item], &self.session))
            .map_err(bus)?;
        Ok(secrets
            .into_values()
            .next()
            .map(|(_, _, value, _)| String::from_utf8_lossy(&value).trim().to_string())
            .filter(|v| !v.is_empty()))
    }

    fn store(&self, key_id: &str, label: &str, value: &str) -> Result<()> {
        let mut collection: OwnedObjectPath =
            self.service.call("ReadAlias", &("default",)).map_err(bus)?;
        if collection.as_str() == "/" {
            collection = OwnedObjectPath::try_from("/org/freedesktop/secrets/collection/login")
                .map_err(|e| Error::msg(e.to_string()))?;
        }
        self.unlock(vec![collection.clone()])?;
        let proxy = self.object(&collection, "org.freedesktop.Secret.Collection")?;
        let attributes = HashMap::from([("service", SERVICE), ("key-id", key_id)]);
        let properties: HashMap<&str, Value> = HashMap::from([
            ("org.freedesktop.Secret.Item.Label", Value::from(label)),
            (
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(attributes),
            ),
        ]);
        let secret = (
            self.session.clone(),
            Vec::<u8>::new(),
            value.as_bytes().to_vec(),
            "text/plain",
        );
        let (_, prompt): (OwnedObjectPath, OwnedObjectPath) = proxy
            .call("CreateItem", &(properties, secret, true))
            .map_err(bus)?;
        self.prompt(prompt)
    }

    fn delete(&self, key_id: &str) -> Result<usize> {
        let (mut items, locked) = self.search(key_id)?;
        self.unlock(locked.clone())?;
        items.extend(locked);
        let count = items.len();
        for item in items {
            let item = self.object(&item, "org.freedesktop.Secret.Item")?;
            let prompt: OwnedObjectPath = item.call("Delete", &()).map_err(bus)?;
            self.prompt(prompt)?;
        }
        Ok(count)
    }
}

/// Test builds only: a JSON file {keyId: applicationKey} standing in for the keyring, so the
/// manager can be driven against the fake B2 without touching the user's keyring.
fn fake_keyring() -> Option<std::path::PathBuf> {
    hook("FAKE_KEYRING").map(std::path::PathBuf::from)
}

fn fake_read(path: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    crate::util::read_json(path)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The application key of a key ID, or an error that says what to do.
pub fn lookup(key_id: &str, wait: Duration) -> Result<String> {
    if let Some(keys) = hook("FAKE_B2_KEYS") {
        // Test builds only: "id:key,id:key".
        return keys
            .split(',')
            .filter_map(|pair| pair.split_once(':'))
            .find(|(id, key)| *id == key_id && !key.is_empty())
            .map(|(_, key)| key.to_string())
            .ok_or_else(|| Error::msg(missing_key(key_id)));
    }
    if let Some(path) = fake_keyring() {
        return fake_read(&path)
            .get(key_id)
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| Error::msg(missing_key(key_id)));
    }
    Keyring::open(wait)?
        .secret(key_id)?
        .ok_or_else(|| Error::msg(missing_key(key_id)))
}

pub fn store(key_id: &str, key: &str) -> Result<()> {
    if let Some(path) = fake_keyring() {
        let mut keys = fake_read(&path);
        keys.insert(key_id.into(), key.into());
        return Ok(crate::util::write_json_atomic(&path, &keys.into())?);
    }
    Keyring::open(Duration::from_secs(300))?.store(
        key_id,
        &format!("Flea Backblaze B2 key {key_id}"),
        key,
    )
}

/// Removes a key ID's application key from the keyring; returns how many items went.
pub fn delete(key_id: &str) -> Result<usize> {
    if let Some(path) = fake_keyring() {
        let mut keys = fake_read(&path);
        let removed = usize::from(keys.remove(key_id).is_some());
        crate::util::write_json_atomic(&path, &keys.into())?;
        return Ok(removed);
    }
    Keyring::open(Duration::from_secs(300))?.delete(key_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real Secret Service path. It runs only when asked (`--ignored`), and only on a private
    /// session bus with a throwaway keyring, as `make keyring-test` sets up: never the user's own.
    #[test]
    #[ignore]
    fn store_lookup_delete_on_a_throwaway_keyring() {
        let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
        let sandbox = std::env::var("FLEA_B2_KEYRING_SANDBOX").unwrap_or_default();
        assert!(
            !sandbox.is_empty() && bus.starts_with("unix:path=/tmp/dbus-"),
            "refusing to touch a keyring outside a private test session"
        );
        let wait = Duration::from_secs(10);
        // A previous run's sandbox keyring may still hold these.
        let _ = delete("flea-b2-test-a");
        let _ = delete("flea-b2-test-b");
        assert!(lookup("flea-b2-test-a", wait).is_err());
        store("flea-b2-test-a", "secret-a").unwrap();
        store("flea-b2-test-b", "secret-b").unwrap();
        store("flea-b2-test-a", "secret-a2").unwrap();
        assert_eq!(lookup("flea-b2-test-a", wait).unwrap(), "secret-a2");
        assert_eq!(lookup("flea-b2-test-b", wait).unwrap(), "secret-b");
        assert_eq!(
            delete("flea-b2-test-a").unwrap(),
            1,
            "a replaced key is one item"
        );
        assert!(lookup("flea-b2-test-a", wait).is_err());
        assert_eq!(lookup("flea-b2-test-b", wait).unwrap(), "secret-b");
        assert_eq!(delete("flea-b2-test-b").unwrap(), 1);
        assert_eq!(delete("flea-b2-test-b").unwrap(), 0);
    }
}
