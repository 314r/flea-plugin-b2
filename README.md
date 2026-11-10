<a id="readme-top"></a>

<div align="center">

<pre>
─────────────────────────────
   F  L  E  A    ·    B  2

</pre>

# ☁️ Backblaze B2 for Flea

### *Your buckets, as close as your folders*

Browse your Backblaze B2 buckets in the Flea file manager and copy files in and out with a paste or a drag,<br/>
then manage your buckets and keys in a window that wears your Omarchy theme.

<br/>

<img width="100%" alt="Flea with a bucket under Backblaze B2 in its sidebar, and beside it the bucket manager window listing that bucket, with its Edit, Change key, Test and Remove buttons" src="screenshots/screenshot-2026-11-10_22-36-49-flea.png" />

<br/>

✨ [Features](#-features) • ⚡ [Installation](#-installation) • 🚀 [Getting started](#-getting-started) • 🔑 [Keys and buckets](#-keys-and-buckets) • 🧭 [How it behaves](#-how-it-behaves) • 🚧 [Limits](#-limits) • 🎨 [Made for Omarchy](#-made-for-omarchy) • 💛 [Thanks](#-thanks) • 🔧 [For developers](#-for-developers)

</div>

---

## ✨ Features

- ☁️ **Your buckets in Flea's sidebar.** Each one gets a row under **Backblaze B2**, in the order
  you choose. Several rows can open different folders of the same bucket.
- 📂 **Browse them like folders.** Folders and files with their size and modification time, in
  any of Flea's views.
- 📥 **Copy in, copy out.** Paste or drag files and whole folders from a local folder into a
  bucket, and back out again. Moves work too.
- 🔁 **Copies that stay in B2.** Copy, move and rename inside a bucket, or between two of your
  buckets that share a key, without downloading anything.
- 🛟 **Deletes you can take back.** Deleting hides a file the way B2 does: its earlier versions
  stay until your bucket's lifecycle rules remove them.
- 🧱 **Big files, checked.** Large files go up in parts, each with its SHA-1. Downloads are checked
  against B2's SHA-1, and modification times survive both ways, as with `b2` and `rclone`.
- ⏳ **Long transfers in the background.** A transfer that outlasts Flea's patience carries on in
  its own process, even if you close Flea, and a notification tells you when it's done.
- 🪟 **A window to manage them.** Add, edit, re-key, test, reorder and remove buckets, with the
  keyboard or the mouse. A new key is checked with Backblaze before anything is saved.
- 🔐 **Keys in your keyring.** Application keys live in your keyring, never in a file, and leave
  it with the last bucket that uses them.
- 🎨 **Wears your Omarchy theme.** The window follows your Omarchy colors, monospace font, text
  size and transparency as you change them.

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## ⚡ Installation

You need [Flea](https://github.com/thisisgm/flea) 0.4.0 or later. On Arch Linux and Omarchy:

```bash
sudo pacman -S --needed base-devel rust libxcb libxkbcommon libxkbcommon-x11 wayland vulkan-icd-loader fontconfig
git clone https://github.com/314r/flea-plugin-b2.git
cd flea-plugin-b2
make
make install
```

Flea doesn't compile plugins: `make` builds the plugin and its window into `bin/`, and
`make install` checks the plugin, adds it to Flea and turns it on. **Backblaze B2** then shows
in Flea's sidebar.

On other distributions, you need Rust 1.98 or later, a C compiler, and GPUI's libraries: libxcb,
libxkbcommon and libxkbcommon-x11, which the window links; Wayland and a Vulkan driver, which it
loads at run time; and fontconfig, which tells it your monospace font. If your Rust is older,
`mise exec -- make` uses the version `mise.toml` pins.

At run time the plugin also needs a keyring that speaks the Secret Service API (gnome-keyring,
KeePassXC…) and a systemd user session, which opens the bucket manager's window.

> **Updating:** after a `git pull`, run `make update`. An installed plugin is a copy and doesn't
> follow its source folder.

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## 🚀 Getting started

1. **Make an application key** on Backblaze's
   [App Keys page](https://secure.backblaze.com/app_keys.htm), with read and write access to the
   buckets you want in Flea.
2. **Open the bucket manager.** In Flea's sidebar, choose **Manage buckets…** under
   **Backblaze B2**.
3. **Add a bucket.** Press `a`, paste the key ID and the application key, pick one of the buckets
   the key opens, and name it. A folder inside the bucket is optional.
4. **Open it.** Its row shows in Flea's sidebar within about ten seconds. Paste or drag files
   into it.

> **Tip:** while the window is open, Flea shows an empty folder for **Manage buckets…**: go back,
> or pick another place. Choosing the row again brings the window forward.

<details>
<summary>⌨️ Keyboard shortcuts</summary>

<br/>

| Shortcut | Action |
|---|---|
| `a`, Ctrl+N | Add a bucket |
| `e`, Enter, double click | Edit the selected bucket's name or folder |
| `c` | Change the key it uses, after you rotate keys for example |
| `t` | Test it: sign in and count what its top level holds |
| `d`, Delete | Remove it from Flea. Nothing is deleted in B2 |
| Shift+K, Shift+↑ | Move it up; Flea's sidebar follows this order |
| Shift+J, Shift+↓ | Move it down |
| `k`, ↑ / `j`, ↓ | Select the previous or next bucket |
| Esc | Close the dialog |
| `q`, Ctrl+Q | Quit. Ctrl+Q works wherever the focus is |

The buttons under the list do the same. Anything that waits on Backblaze or the keyring says
what it waits for, and a refusal stays in the dialog so you can correct it.

</details>

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## 🔑 Keys and buckets

- **Checked first.** A new key is checked with Backblaze before anything is saved, and you pick
  among the buckets it can open.
- **One key, many buckets.** Each application key is stored once in your keyring, under its key
  ID, and several entries can share it. It is never written to a file, and it leaves the keyring
  with the last entry that uses it.
- **The bucket list** lives in `~/.config/flea-b2/buckets.json`: names, buckets, folders and key
  IDs, no secrets. Flea reads it again when it changes, so a new or renamed bucket shows in the
  sidebar within about ten seconds, with no restart.
- **Several entries for one bucket.** Give each a different folder, for example *Photos* →
  `my-bucket/photos` and *Docs* → `my-bucket/docs`.
- **At most 63 entries.** Flea draws at most 64 rows in a plugin's sidebar section, and the last
  one is **Manage buckets…**.
- **An unreadable list.** If `buckets.json` isn't valid JSON, the sidebar keeps only
  **Manage buckets…**, and the window shows the error and changes nothing until the file is fixed
  or deleted.
- **A locked keyring** brings up its own unlock prompt. If it isn't answered within about 12
  seconds, the folder shows "The keyring is locked"; refresh after unlocking.

### Why a separate window

Flea runs a plugin with a 512 MiB address-space limit that its child processes inherit and cannot
raise: too little for a GPU-drawn window. Flea also stops a plugin by killing its whole process
group. So the plugin asks your systemd user manager to start the window (`systemd-run --user`),
with ordinary limits and independent of Flea. It passes on the display variables and
`XDG_CONFIG_HOME`, but not Flea's `LC_ALL=C`. Without systemd, start
`~/.config/flea/plugins/b2.bucket/bin/flea-b2-manager` from a launcher.

**Manage buckets…** is a sidebar row because Flea offers a plugin no other way in: its right-click
actions only appear in local folders, and a plugin's Settings page shows only settings fields.

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## 🧭 How it behaves

- **Delete hides files** (`b2_hide_file`). They disappear from listings, but B2 keeps the earlier
  versions until the bucket's lifecycle rules remove them. With the default "keep all versions"
  setting, you can still restore a deleted file from the Backblaze web console.
- **A whole bucket entry can't be deleted, renamed or moved from Flea.** Manage entries in the
  window; open one to work on its files.
- **Between two buckets.** Copies and moves between entries that use the same key happen inside
  B2. Between entries with different keys they are refused; copy through a local folder instead.
- **Rename and move inside B2** copy server-side, then hide the original. B2 has no rename.
- **Nothing is overwritten.** If a name already exists at the destination, the operation is
  refused before anything is transferred.
- **Empty folders** are kept with B2's usual `.bzEmpty` marker file, which the listing hides.
- **Large files** (over B2's recommended part size, usually 100 MB) are uploaded in parts, each
  checked with SHA-1. Downloads are verified against B2's SHA-1 when it has one. Modification
  times are kept both ways (`src_last_modified_millis`, the same field `b2` and `rclone` use).
- **Long operations.** Flea gives a plugin 30 seconds per request, so each change runs in a
  separate worker process. If it isn't done after about 24 seconds, Flea is told it is done and a
  desktop notification says the transfer continues. A second one says when it finishes or why it
  failed. Refresh the folder to see new files.
- **No "All buckets" row.** The plugin declares a place, because Flea sends a plugin listings and
  file operations only then, but with an empty `path`, which tells Flea not to draw its row. Go up
  from a bucket to see every entry.
- **Big folders.** Flea shows at most 256 rows per folder. A bigger folder shows its subfolders
  first, then files, then a last row named "… more items not shown". Operations ignore that row.

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## 🚧 Limits

Flea draws all the UI. In a plugin place, Flea itself offers no New Folder, Trash, Duplicate, Undo
or opening a file in place: to open a file, copy it to a local folder first. Transfers can't be
cancelled from Flea; stop a background one with `pkill -f 'flea-b2 worker'`.

Diagnostics go to `~/.local/state/flea-b2/plugin.log`, since Flea discards a plugin's stderr.

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## 🎨 Made for Omarchy

The bucket manager is built with [gpui-omarchy](https://github.com/huacnlee/gpui-omarchy). It
takes its colors from your current Omarchy theme, and the monospace font, text size and window
transparency you chose in Omarchy, and repaints a moment after you change any of them, with no
restart. Flea draws everything else in its own Omarchy style.

Not on Omarchy? The window uses the Tokyo Night palette and your fontconfig monospace font.

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## 💛 Thanks

- **[Flea](https://github.com/thisisgm/flea)** by GM, the file manager this plugin lives in. Its
  plugin protocol draws every row you see, and its license tool, adapted here, writes
  [THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt).
- **[Backblaze B2](https://www.backblaze.com/cloud-storage)** and its native API, which the plugin
  speaks directly.
- **[GPUI](https://www.gpui.rs)** by Zed Industries, **[gpui-kit](https://github.com/longbridge/gpui-kit)**
  by Longbridge and **[gpui-omarchy](https://github.com/huacnlee/gpui-omarchy)** by Jason Lee,
  which draw the bucket manager.
- **[Lucide](https://lucide.dev)** for the icons, **[Omarchy](https://omarchy.org)** for the look,
  and rustls, ring, ureq and zbus, which talk to B2, the keyring and the notifications.

> Backblaze B2 for Flea is an independent project, not made, endorsed or supported by Backblaze or
> by Flea's author. Backblaze and B2 are trademarks of Backblaze, Inc.

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## 🔧 For developers

The plugin is written in Rust and builds two binaries:

- **`flea-b2`** (`src/`) is the plugin and its transfer worker. It speaks Flea's JSON-lines
  plugin protocol on its standard input and output, B2's native API over rustls, and D-Bus to
  the keyring and notifications. It needs only glibc.
- **`flea-b2-manager`** (`manager/`) is the bucket manager's window, built with GPUI and
  gpui-omarchy on the plugin's own library.

```sh
make test          # unit tests, protocol tests against a build with test hooks, and a license check
make keyring-test  # the real keyring code, against a throwaway keyring on a private D-Bus session
make licenses      # regenerate THIRD-PARTY-LICENSES.txt after a change to Cargo.lock
make update        # reinstall after a change: an installed plugin doesn't follow its source
```

- **The protocol tests** in `tests/` need Python 3, for development only. They start `flea-b2`
  exactly as Flea does, in its own process group with `LC_ALL=C` and a 30-second answer limit, and
  check every listing against Flea's bounds: 256 rows, 64 KiB, and valid, unique names.
- **The fake B2.** `tests/fake_b2.py` is an in-memory B2 with several buckets. It can inject
  faults and delays.
- **Test hooks.** A build with `--features test-hooks` reads the fake API URL, fake keys, test
  timings and a stand-in for `systemd-run` from the environment. The release binaries contain
  none of these hooks.
- **Build output** goes to `.target/` (see `.cargo/config.toml`). `flea plugin add` skips dot
  folders, which keeps the installed plugin under the 64 MiB Flea allows; a debug binary alone
  is bigger.

Flea's plugin manual is [PLUGINS.md](https://github.com/thisisgm/flea/blob/main/PLUGINS.md).

<div align="right"><a href="#readme-top">↑ back to top</a></div>

---

## 📜 License

Backblaze B2 for Flea is free software, released under the [MIT License](LICENSE). Its binaries
contain the Rust crates listed in [THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt), each with
its license and notices. `manager/src/desktop.rs` adapts gpui-omarchy's theme watcher, and
`tools/third-party-licenses` adapts Flea's own license tool, both under the MIT License.

---

<div align="center">

☁️ 🪣 ☁️

### 💛 Carried by Flea, drawn with GPUI, and colored by your Omarchy theme.

</div>
