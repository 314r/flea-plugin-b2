//! One manager window per user. The first start holds `manager.lock` in the runtime folder and
//! listens on `manager.sock`; a later one, such as Flea's "Manage buckets…" row chosen again,
//! asks it to raise its window and exits.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ACTIVATE: &[u8] = b"activate\n";
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The longest path a Unix socket address holds.
const MAX_SOCKET_PATH: usize = 107;

pub enum Instance {
    /// This start holds the lock; each later start sends to `activations`.
    First(First),
    /// Another start holds the lock and was asked to raise its window.
    Second,
}

/// The default holds no lock and hears no later start: the manager runs alone.
#[derive(Default)]
pub struct First {
    /// Held open for the life of the process: the lock goes with it.
    _lock: Option<File>,
    pub activations: Option<async_channel::Receiver<()>>,
}

impl gpui_kit::Global for First {}

/// Where the lock and the socket live: `$XDG_RUNTIME_DIR/flea-b2`.
fn folder() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    runtime.is_absolute().then(|| runtime.join("flea-b2"))
}

/// Without a runtime folder, or when locking fails, the manager runs alone rather than not at all.
pub fn acquire() -> Instance {
    let Some(folder) = folder() else {
        return Instance::First(First::default());
    };
    if let Err(error) = fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
    {
        eprintln!(
            "flea-b2-manager: {} cannot be created: {error}",
            folder.display()
        );
        return Instance::First(First::default());
    }
    let socket = folder.join("manager.sock");
    let lock = match lock_exclusively(&folder.join("manager.lock")) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            if let Err(error) = ask_first_to_activate(&socket) {
                eprintln!("flea-b2-manager: already running, and it could not be raised: {error}");
            }
            return Instance::Second;
        }
        Err(error) => {
            eprintln!("flea-b2-manager: the instance lock failed: {error}");
            return Instance::First(First::default());
        }
    };
    let activations = listen(&socket).unwrap_or_else(|error| {
        eprintln!(
            "flea-b2-manager: {} cannot be used: {error}",
            socket.display()
        );
        None
    });
    Instance::First(First {
        _lock: Some(lock),
        activations,
    })
}

/// The lock, or `None` while another process holds it. Only a dead holder frees it.
fn lock_exclusively(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    // SAFETY: flock takes a descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EWOULDBLOCK) => Ok(None),
        _ => Err(error),
    }
}

fn ask_first_to_activate(socket: &Path) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(ACTIVATE)
}

/// Listens on the socket, which a holder that died may have left: the lock says it is ours now.
fn listen(socket: &Path) -> io::Result<Option<async_channel::Receiver<()>>> {
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        return Ok(None);
    }
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    // One waiting activation is enough: raising twice is raising once.
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("flea-b2-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = [0u8; 16];
                let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                let read = stream.read(&mut request).unwrap_or(0);
                if request[..read].starts_with(b"activate") {
                    let _ = sender.try_send(());
                }
                if sender.is_closed() {
                    break;
                }
            }
        })?;
    Ok(Some(receiver))
}
