//! What the manager follows from the desktop besides the palette, which gpui-omarchy
//! already follows: Omarchy's window transparency, the text size and the monospace
//! font. All of it applies live, and the manager only ever reads it.
//!
//! Adapted from Brouyon's `desktop.rs` (MIT, by the same author).

use gpui_kit::{App, Global, Hsla, SharedString, Window, WindowBackgroundAppearance};
use gpui_omarchy::ActiveTheme;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::env;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

/// The size Omarchy's shell metrics are defined against.
const OMARCHY_BASELINE_FONT_SIZE: f32 = 12.;
/// What `o.transparent_window` gives a window, when it's active and when it's not.
const WINDOW_OPACITY: (f32, f32) = (0.985, 0.96);
/// What `omarchy-theme-set-glass` gives a terminal's background.
const GLASS_OPACITY: f32 = 0.65;
/// A theme switch removes a folder before moving the next one into place, and a font
/// change empties a file before writing it, so reading waits for such a burst to end.
const SETTLE: Duration = Duration::from_millis(100);

/// The choice under Omarchy's Appearance > Windows > Window Opacity.
///
/// Omarchy leaves the effect to each app: its opacity rules only cover windows that
/// opt in, and glassy terminals paint their own see-through background, which keeps
/// their text crisp. The manager paints its background the same way.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Transparency {
    #[default]
    Opaque,
    Transparent,
    Glassy,
}

impl Transparency {
    pub fn surface(self) -> WindowBackgroundAppearance {
        match self {
            Self::Opaque => WindowBackgroundAppearance::Opaque,
            // Not Blurred: Hyprland already blurs behind any see-through window
            // when Omarchy asks it to.
            Self::Transparent | Self::Glassy => WindowBackgroundAppearance::Transparent,
        }
    }

    pub fn alpha(self, active: bool) -> f32 {
        let window = if active {
            WINDOW_OPACITY.0
        } else {
            WINDOW_OPACITY.1
        };
        match self {
            Self::Opaque => 1.,
            Self::Transparent => window,
            Self::Glassy => GLASS_OPACITY * window,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Desktop {
    pub transparency: Transparency,
    /// How much larger than Omarchy's baseline the user set their desktop text.
    pub text_scale: f32,
    /// The monospace font, once it has changed since launch. Until then
    /// gpui-omarchy's choice is current, and it only looks once per process.
    pub font: Option<SharedString>,
}

impl Global for Desktop {}

impl Default for Desktop {
    /// Solid and unscaled, so nothing depends on the real desktop until
    /// [`follow`] is called, which only `main` does.
    fn default() -> Self {
        Self {
            transparency: Transparency::Opaque,
            text_scale: 1.,
            font: None,
        }
    }
}

impl Desktop {
    fn read(paths: &Paths) -> Self {
        Self {
            transparency: paths.transparency(),
            text_scale: paths.text_scale(),
            font: None,
        }
    }
}

/// The background of a window's root. Only the root is see-through: dialogs, popups
/// and tooltips paint the theme's solid background, and should keep doing so.
pub fn root_background(window: &Window, cx: &App) -> Hsla {
    let alpha = cx
        .global::<Desktop>()
        .transparency
        .alpha(window.is_window_active());
    cx.omarchy().background.opacity(alpha)
}

/// Keeps every window's surface in step with the desktop. Needs no real desktop.
pub fn init(cx: &mut App) {
    cx.default_global::<Desktop>();
    cx.observe_global::<Desktop>(|cx| {
        let surface = cx.global::<Desktop>().transparency.surface();
        for window in cx.windows() {
            window
                .update(cx, |_, window, _| {
                    window.set_background_appearance(surface);
                })
                .ok();
        }
        cx.refresh_windows();
    })
    .detach();
}

/// Reads the desktop's settings and keeps following them.
pub fn follow(cx: &mut App) {
    let paths = Paths::from_env();
    // Armed before reading, so a change during startup isn't missed.
    let watcher = DesktopWatcher::new(&paths);
    let mut fonts = fonts_stamp(&paths);
    let mut desktop = Desktop::read(&paths);
    cx.set_global(desktop.clone());
    let (mut watcher, events) = match watcher {
        Ok(watcher) => watcher,
        Err(error) => {
            eprintln!("brouyon: can't watch the desktop's settings: {error}");
            return;
        }
    };
    cx.spawn(async move |cx| {
        while events.recv().await.is_ok() {
            cx.background_executor().timer(SETTLE).await;
            while events.try_recv().is_ok() {}
            let paths = paths.clone();
            let previous = desktop.clone();
            let (next_watcher, next_fonts, next) = cx
                .background_executor()
                .spawn(async move {
                    watcher.arm(&paths);
                    let (next, fonts) = reread(&paths, &previous, fonts, monospace_family);
                    (watcher, fonts, next)
                })
                .await;
            watcher = next_watcher;
            fonts = next_fonts;
            if next != desktop {
                desktop = next.clone();
                cx.update(|cx| cx.set_global(next));
            }
        }
    })
    .detach();
}

/// Reads everything again. The font is only looked up when its file has changed,
/// because that starts a process and these folders see other changes too.
fn reread(
    paths: &Paths,
    previous: &Desktop,
    fonts: Option<FontsStamp>,
    monospace: impl FnOnce() -> Option<SharedString>,
) -> (Desktop, Option<FontsStamp>) {
    let mut next = Desktop {
        font: previous.font.clone(),
        ..Desktop::read(paths)
    };
    let stamp = fonts_stamp(paths);
    if stamp != fonts
        && let Some(font) = monospace()
    {
        next.font = Some(font);
    }
    (next, stamp)
}

type FontsStamp = (SystemTime, u64);

fn fonts_stamp(paths: &Paths) -> Option<FontsStamp> {
    let metadata = fs::metadata(paths.fontconfig.join("fonts.conf")).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// fontconfig is where `omarchy font set` records the choice.
fn monospace_family() -> Option<SharedString> {
    let output = Command::new("fc-match")
        .args(["monospace", "-f", "%{family}"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    first_family(&String::from_utf8(output.stdout).ok()?)
}

/// fc-match lists aliases, such as "JetBrainsMono Nerd Font,JetBrainsMono NF".
fn first_family(families: &str) -> Option<SharedString> {
    let family = families.lines().next()?.split(',').next()?.trim();
    (!family.is_empty()).then(|| family.to_owned().into())
}

#[derive(Clone, Debug)]
struct Paths {
    toggles: PathBuf,
    current: PathBuf,
    config: PathBuf,
    fontconfig: PathBuf,
}

impl Paths {
    fn from_env() -> Self {
        let home = home_dir();
        let state_home = xdg_dir("XDG_STATE_HOME", ".local/state", &home);
        Self::new(&home, &state_home)
    }

    /// Only the toggles honour `XDG_STATE_HOME`: `omarchy-theme-set` and
    /// `omarchy-font-set` write under `$HOME` whatever the XDG variables say.
    fn new(home: &Path, state_home: &Path) -> Self {
        Self {
            toggles: state_home.join("omarchy/toggles/hypr"),
            current: home.join(".local/state/omarchy/current"),
            config: home.join(".config/omarchy"),
            fontconfig: home.join(".config/fontconfig"),
        }
    }

    fn theme(&self) -> PathBuf {
        self.current.join("theme")
    }

    /// The same answer as `omarchy hyprland window transparency`. Omarchy treats no
    /// toggle as transparent, but no Omarchy at all has to mean a normal solid window.
    /// `current` is what's checked because its `theme` folder vanishes during a switch.
    fn transparency(&self) -> Transparency {
        if !self.current.exists() || self.toggles.join("window-opaque.lua").exists() {
            Transparency::Opaque
        } else if self.toggles.join("window-glassy.lua").exists() {
            Transparency::Glassy
        } else {
            Transparency::Transparent
        }
    }

    /// `[font] base-size` from the file `omarchy display text size` writes, then
    /// from the theme's own shell file.
    fn text_scale(&self) -> f32 {
        [
            self.config.join("shell.toml"),
            self.theme().join("shell.toml"),
        ]
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .find_map(|contents| text_scale_from(&contents))
        .unwrap_or(1.)
    }

    fn folders(&self) -> [PathBuf; 5] {
        [
            self.toggles.clone(),
            self.current.clone(),
            self.theme(),
            self.config.clone(),
            self.fontconfig.clone(),
        ]
    }
}

fn text_scale_from(shell_toml: &str) -> Option<f32> {
    let table: toml::Table = shell_toml.parse().ok()?;
    let font = table.get("font")?.as_table()?;
    let value = font.get("base-size")?;
    let size = value
        .as_float()
        .or_else(|| value.as_integer().map(|n| n as f64))? as f32;
    (size > 0.).then_some(size / OMARCHY_BASELINE_FONT_SIZE)
}

/// Watches folders rather than files, so atomic saves and replaced folders survive.
/// Adapted from gpui-omarchy's palette watcher, which is private to that crate.
struct DesktopWatcher {
    watcher: RecommendedWatcher,
    armed: BTreeMap<PathBuf, Identity>,
}

/// A folder can be deleted and recreated at the same path before its events are read,
/// and the old watch then refers to a folder that's gone.
type Identity = (u64, u64);

impl DesktopWatcher {
    fn new(paths: &Paths) -> notify::Result<(Self, async_channel::Receiver<()>)> {
        // One waiting event is enough, because each one reloads everything.
        let (sender, receiver) = async_channel::bounded(1);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            // Reading a watched file reports an access, which must not cause another read.
            if !matches!(&event, Ok(event) if event.kind.is_access()) {
                let _ = sender.try_send(());
            }
        })?;
        let mut watcher = Self {
            watcher,
            armed: BTreeMap::new(),
        };
        watcher.arm(paths);
        Ok((watcher, receiver))
    }

    /// A folder's parent sees it being replaced, and the nearest ancestor of a missing
    /// one sees it appear, so Omarchy can be set up after the manager starts.
    fn arm(&mut self, paths: &Paths) {
        let mut wanted = BTreeMap::new();
        for folder in paths.folders() {
            let Some(nearest) = folder.ancestors().find(|path| path.is_dir()) else {
                continue;
            };
            add_folder(nearest, &mut wanted);
            if nearest == folder
                && let Some(parent) = nearest.parent()
            {
                add_folder(parent, &mut wanted);
            }
        }
        self.armed.retain(|path, identity| {
            let unchanged = wanted.get(path) == Some(identity);
            if !unchanged {
                let _ = self.watcher.unwatch(path);
            }
            unchanged
        });
        for (path, identity) in wanted {
            if !self.armed.contains_key(&path) {
                match self.watcher.watch(&path, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        self.armed.insert(path, identity);
                    }
                    Err(error) => eprintln!("brouyon: can't watch {}: {error}", path.display()),
                }
            }
        }
    }
}

fn add_folder(path: &Path, folders: &mut BTreeMap<PathBuf, Identity>) {
    if let Ok(path) = fs::canonicalize(path)
        && let Ok(metadata) = fs::metadata(&path)
    {
        folders.insert(path, (metadata.dev(), metadata.ino()));
    }
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn xdg_dir(variable: &str, fallback: &str, home: &Path) -> PathBuf {
    env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(fallback))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Instant};

    /// A desktop inside a temporary folder, with Omarchy set up.
    fn omarchy() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path(), &dir.path().join(".local/state"));
        for folder in paths.folders() {
            fs::create_dir_all(folder).unwrap();
        }
        (dir, paths)
    }

    fn touch(path: PathBuf) {
        fs::write(path, "").unwrap();
    }

    #[test]
    fn the_mode_is_read_the_way_omarchy_reads_it() {
        let (_dir, paths) = omarchy();
        assert_eq!(paths.transparency(), Transparency::Transparent);

        touch(paths.toggles.join("window-glassy.lua"));
        assert_eq!(paths.transparency(), Transparency::Glassy);

        touch(paths.toggles.join("window-opaque.lua"));
        assert_eq!(paths.transparency(), Transparency::Opaque, "opaque wins");

        fs::remove_file(paths.toggles.join("window-glassy.lua")).unwrap();
        assert_eq!(paths.transparency(), Transparency::Opaque);
    }

    #[test]
    fn the_mode_stays_put_while_a_theme_is_being_switched() {
        let (_dir, paths) = omarchy();
        touch(paths.toggles.join("window-glassy.lua"));
        fs::remove_dir_all(paths.theme()).unwrap();
        assert_eq!(paths.transparency(), Transparency::Glassy);
    }

    #[test]
    fn without_omarchy_a_window_is_solid_and_unscaled() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path(), &dir.path().join(".local/state"));
        assert_eq!(Desktop::read(&paths), Desktop::default());
    }

    #[test]
    fn toggles_follow_xdg_state_home_but_the_theme_stays_under_home() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path(), state.path());
        fs::create_dir_all(&paths.current).unwrap();
        assert!(paths.current.starts_with(home.path()));

        fs::create_dir_all(&paths.toggles).unwrap();
        assert!(paths.toggles.starts_with(state.path()));
        touch(paths.toggles.join("window-glassy.lua"));
        assert_eq!(paths.transparency(), Transparency::Glassy);
    }

    #[test]
    fn each_mode_has_its_surface_and_alpha() {
        use WindowBackgroundAppearance as Surface;
        for (mode, surface, active, inactive) in [
            (Transparency::Opaque, Surface::Opaque, 1., 1.),
            (Transparency::Transparent, Surface::Transparent, 0.985, 0.96),
            (
                Transparency::Glassy,
                Surface::Transparent,
                0.65 * 0.985,
                0.65 * 0.96,
            ),
        ] {
            assert_eq!(mode.surface(), surface);
            assert_eq!(mode.alpha(true), active);
            assert_eq!(mode.alpha(false), inactive);
        }
    }

    #[test]
    fn the_users_text_size_comes_before_the_themes() {
        let (_dir, paths) = omarchy();
        assert_eq!(paths.text_scale(), 1.);

        fs::write(paths.theme().join("shell.toml"), "[font]\nbase-size = 18").unwrap();
        assert_eq!(paths.text_scale(), 1.5);

        fs::write(paths.config.join("shell.toml"), "[font]\nbase-size = 24").unwrap();
        assert_eq!(paths.text_scale(), 2.);

        fs::write(paths.config.join("shell.toml"), "[bar]\nsize = 3").unwrap();
        assert_eq!(paths.text_scale(), 1.5, "no size there: the theme's");
    }

    #[test]
    fn text_scale_reads_the_shell_font_base_size() {
        assert_eq!(text_scale_from("[font]\nbase-size = 16"), Some(16. / 12.));
        assert_eq!(
            text_scale_from("[font]\nbase-size = 13.5"),
            Some(13.5 / 12.)
        );
        assert_eq!(text_scale_from("[bar]\nsize = 3"), None);
        assert_eq!(text_scale_from("[font]\nbase-size = 0"), None);
        assert_eq!(text_scale_from("not toml ="), None);
    }

    #[test]
    fn the_first_alias_is_the_font() {
        assert_eq!(
            first_family("JetBrainsMono Nerd Font,JetBrainsMono NF\n"),
            Some("JetBrainsMono Nerd Font".into())
        );
        assert_eq!(first_family("Menlo"), Some("Menlo".into()));
        assert_eq!(first_family(""), None);
        assert_eq!(first_family("\n"), None);
    }

    #[test]
    fn the_font_is_only_looked_up_when_its_file_changes() {
        let (_dir, paths) = omarchy();
        let previous = Desktop::default();
        let unexpected = || panic!("the file hasn't changed");

        let stamp = fonts_stamp(&paths);
        let (next, stamp) = reread(&paths, &previous, stamp, unexpected);
        assert_eq!(next.font, None);

        fs::write(paths.fontconfig.join("fonts.conf"), "<fontconfig/>").unwrap();
        let (next, stamp) = reread(&paths, &previous, stamp, || Some("Chosen".into()));
        assert_eq!(next.font, Some("Chosen".into()));

        let (later, stamp) = reread(&paths, &next, stamp, unexpected);
        assert_eq!(later.font, Some("Chosen".into()), "and then it's kept");

        fs::write(
            paths.fontconfig.join("fonts.conf"),
            "<fontconfig></fontconfig>",
        )
        .unwrap();
        let (failed, _) = reread(&paths, &later, stamp, || None);
        assert_eq!(
            failed.font,
            Some("Chosen".into()),
            "a failed lookup keeps it"
        );
    }

    /// Real file events, which arrive on their own thread, so these wait in real time.
    fn wait_for_event(events: &async_channel::Receiver<()>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while events.try_recv().is_err() {
            assert!(Instant::now() < deadline, "no event arrived");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Lets a burst of events finish and forgets them, as `follow` does.
    fn settle(events: &async_channel::Receiver<()>) {
        thread::sleep(SETTLE);
        while events.try_recv().is_ok() {}
    }

    #[test]
    fn a_change_of_mode_is_noticed() {
        let (_dir, paths) = omarchy();
        let (_watcher, events) = DesktopWatcher::new(&paths).unwrap();
        touch(paths.toggles.join("window-glassy.lua"));
        wait_for_event(&events);
        assert_eq!(paths.transparency(), Transparency::Glassy);
    }

    #[test]
    fn a_toggles_folder_created_later_is_noticed() {
        let (_dir, paths) = omarchy();
        fs::remove_dir_all(paths.toggles.parent().unwrap()).unwrap();
        let (mut watcher, events) = DesktopWatcher::new(&paths).unwrap();

        fs::create_dir_all(&paths.toggles).unwrap();
        wait_for_event(&events);
        settle(&events);
        watcher.arm(&paths);

        touch(paths.toggles.join("window-glassy.lua"));
        wait_for_event(&events);
        assert_eq!(paths.transparency(), Transparency::Glassy);
    }

    #[test]
    fn a_replaced_theme_folder_is_still_watched() {
        let (dir, paths) = omarchy();
        let (mut watcher, events) = DesktopWatcher::new(&paths).unwrap();

        // What omarchy-theme-set does: the old folder goes, the next one moves in.
        let next = dir.path().join(".local/state/omarchy/current/next-theme");
        fs::create_dir_all(&next).unwrap();
        fs::remove_dir_all(paths.theme()).unwrap();
        fs::rename(&next, paths.theme()).unwrap();
        wait_for_event(&events);
        settle(&events);
        watcher.arm(&paths);

        fs::write(paths.theme().join("shell.toml"), "[font]\nbase-size = 18").unwrap();
        wait_for_event(&events);
        assert_eq!(paths.text_scale(), 1.5);
    }

    #[test]
    fn a_file_renamed_into_place_is_noticed() {
        let (_dir, paths) = omarchy();
        let (_watcher, events) = DesktopWatcher::new(&paths).unwrap();
        let temporary = paths.config.join(".shell.toml.tmp");
        fs::write(&temporary, "[font]\nbase-size = 24").unwrap();
        settle(&events);

        fs::rename(&temporary, paths.config.join("shell.toml")).unwrap();
        wait_for_event(&events);
        assert_eq!(paths.text_scale(), 2.);
    }

    #[test]
    fn reading_the_settings_does_not_set_off_the_watcher() {
        let (_dir, paths) = omarchy();
        fs::write(paths.config.join("shell.toml"), "[font]\nbase-size = 18").unwrap();
        let (_watcher, events) = DesktopWatcher::new(&paths).unwrap();
        settle(&events);

        Desktop::read(&paths);
        thread::sleep(SETTLE);
        assert!(events.try_recv().is_err());
    }
}
