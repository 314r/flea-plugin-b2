//! The manager's window: the bucket entries Flea shows in its sidebar, in order, with what can be
//! done to them, and one dialog that walks through adding, editing, re-keying and removing.
//!
//! The rules and the sentences come from `flea_b2::manage`.
//! Anything that waits on Backblaze or the keyring runs on a worker thread while the window says
//! what it is waiting for.
use crate::desktop::{self, Desktop};
use flea_b2::config::{Config, Entry};
use flea_b2::manage::{self, APP_KEYS_URL, CheckedKey, KeyChoice};
use gpui_kit::base::input::InputState;
use gpui_kit::{
    App, ClickEvent, Context, Div, Entity, FocusHandle, FontWeight, KeyBinding, SharedString,
    Window, div, prelude::*, px, relative, rems, transparent_black,
};
use gpui_omarchy::{
    ActiveTheme, Button, ButtonVariant, IconName, Status, Theme, alert, button, dialog,
    dialog_button, dialog_description, dialog_popup, dialog_title, empty_state, focus_scope, icon,
    input, keycap, link, separator, with_tooltip,
};

gpui_kit::actions!(
    flea_b2_manager,
    [
        AddBucket,
        EditBucket,
        ChangeKey,
        TestBucket,
        RemoveBucket,
        MoveUp,
        MoveDown,
        SelectPrevious,
        SelectNext,
        Quit
    ]
);

/// The list's own context: its single-letter keys never reach a dialog's text fields.
const KEY_CONTEXT: &str = "BucketList";
/// A dialog's list of choices, walked with the arrows.
const CHOICES_CONTEXT: &str = "BucketChoices";
/// The size gpui-omarchy's metrics assume, matching Omarchy's default text size.
const BASE_REM_SIZE: f32 = 16.;

pub fn init(cx: &mut App) {
    desktop::init(cx);
    use_mono_font(cx);
    cx.observe_global::<Theme>(use_mono_font).detach();
    cx.observe_global::<Desktop>(use_mono_font).detach();
    cx.on_action(|_: &Quit, cx| cx.quit());
    let list = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("a", AddBucket, list),
        KeyBinding::new("ctrl-n", AddBucket, list),
        KeyBinding::new("e", EditBucket, list),
        KeyBinding::new("enter", EditBucket, list),
        KeyBinding::new("c", ChangeKey, list),
        KeyBinding::new("t", TestBucket, list),
        KeyBinding::new("d", RemoveBucket, list),
        KeyBinding::new("delete", RemoveBucket, list),
        KeyBinding::new("shift-k", MoveUp, list),
        KeyBinding::new("shift-up", MoveUp, list),
        KeyBinding::new("shift-j", MoveDown, list),
        KeyBinding::new("shift-down", MoveDown, list),
        KeyBinding::new("k", SelectPrevious, list),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("j", SelectNext, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("q", Quit, list),
        KeyBinding::new("up", SelectPrevious, Some(CHOICES_CONTEXT)),
        KeyBinding::new("k", SelectPrevious, Some(CHOICES_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(CHOICES_CONTEXT)),
        KeyBinding::new("j", SelectNext, Some(CHOICES_CONTEXT)),
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}

/// Omarchy's shell and Flea use one monospace face throughout, while gpui-omarchy's widgets use
/// the theme's UI font. gpui-omarchy looks the monospace font up once, so one changed since then
/// comes from [`Desktop`]. (The same as Brouyon's.)
fn use_mono_font(cx: &mut App) {
    let theme = cx.omarchy();
    let wanted = cx
        .global::<Desktop>()
        .font
        .clone()
        .unwrap_or_else(|| theme.mono_font.clone());
    if theme.font != wanted || theme.mono_font != wanted {
        let theme = cx.global_mut::<Theme>();
        theme.font = wanted.clone();
        theme.mono_font = wanted;
        cx.refresh_windows();
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Purpose {
    Add,
    ChangeKey(usize),
}

/// The steps of the dialog. Adding goes key, then bucket, then details; changing the key stops
/// after the key; editing and removing are one step.
enum Step {
    PickKey {
        purpose: Purpose,
    },
    NewKey {
        purpose: Purpose,
        key_id: Entity<InputState>,
        secret: Entity<InputState>,
    },
    PickBucket {
        key: KeyChoice,
        names: Vec<String>,
    },
    Details {
        key: KeyChoice,
        bucket: String,
        editing: Option<usize>,
        name: Entity<InputState>,
        folder: Entity<InputState>,
    },
    Remove {
        index: usize,
    },
}

struct Dialog {
    step: Step,
    error: Option<SharedString>,
}

pub struct Manager {
    config: Config,
    /// Why the bucket list cannot be read. Nothing is edited until it can, so the file is never
    /// overwritten by an empty list.
    problem: Option<SharedString>,
    /// Always a valid index while there are entries.
    selected: Option<usize>,
    status: Option<(SharedString, Status)>,
    /// What the window or its dialog waits for. A dialog cannot be cancelled meanwhile, since the
    /// work may be writing.
    busy: Option<SharedString>,
    dialog: Option<Dialog>,
    list_focus: FocusHandle,
    dialog_focus: FocusHandle,
    /// One per choice of the dialog's step, so Enter can take the focused one.
    choices: Vec<FocusHandle>,
}

impl Manager {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (config, problem) = match Config::load() {
            Ok(config) => (config, None),
            Err(error) => (Config::default(), Some(error.to_string().into())),
        };
        let manager = Manager {
            selected: (!config.buckets.is_empty()).then_some(0),
            config,
            problem,
            status: None,
            busy: None,
            dialog: None,
            list_focus: cx.focus_handle(),
            dialog_focus: cx.focus_handle(),
            choices: Vec::new(),
        };
        manager.list_focus.focus(window, cx);
        manager
    }

    fn blocked(&self) -> bool {
        self.dialog.is_some() || self.busy.is_some() || self.problem.is_some()
    }

    fn say(&mut self, text: impl Into<SharedString>) {
        self.status = Some((text.into(), Status::Success));
    }

    fn fail(&mut self, text: impl ToString) {
        self.status = Some((text.to_string().into(), Status::Error));
    }

    // Background work.

    /// Runs `work` on a worker thread, saying `busy` meanwhile, then `done` with its result.
    fn run<T: Send + 'static>(
        &mut self,
        busy: &str,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some(busy.to_string().into());
        match self.dialog.as_mut() {
            Some(dialog) => dialog.error = None,
            None => self.status = None,
        }
        let (sender, receiver) = async_channel::bounded(1);
        let started = std::thread::Builder::new()
            .name("flea-b2-work".into())
            .spawn(move || {
                let _ = sender.send_blocking(work());
            });
        if let Err(error) = started {
            self.busy = None;
            self.fail(format!("The work could not start: {error}"));
            cx.notify();
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                done(this, result, window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// A refusal shown in the dialog, which stays open to correct it.
    fn dialog_error(&mut self, error: impl ToString, cx: &mut Context<Self>) {
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.error = Some(error.to_string().into());
        }
        cx.notify();
    }

    // The dialog.

    fn open(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.status = None;
        self.dialog = Some(Dialog { step, error: None });
        self.focus_step(window, cx);
        cx.notify();
    }

    fn go_to(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.step = step;
            dialog.error = None;
        }
        self.focus_step(window, cx);
        cx.notify();
    }

    /// Puts the keyboard on the step's first field or choice, or on the list without a dialog.
    fn focus_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while self.choices.len() < self.choice_count() {
            self.choices.push(cx.focus_handle());
        }
        match self.dialog.as_ref().map(|d| &d.step) {
            Some(Step::NewKey { key_id: field, .. } | Step::Details { name: field, .. }) => {
                let field = field.clone();
                field.update(cx, |state, cx| state.focus(window, cx));
            }
            // Each has a choice at least: "New key…", or one bucket.
            Some(Step::PickKey { .. } | Step::PickBucket { .. }) => {
                self.choices[0].focus(window, cx)
            }
            Some(Step::Remove { .. }) => self.dialog_focus.focus(window, cx),
            None => self.list_focus.focus(window, cx),
        }
    }

    /// The choices of a picking step: the stored keys and "New key…", or the buckets.
    fn choice_count(&self) -> usize {
        match self.dialog.as_ref().map(|d| &d.step) {
            Some(Step::PickKey { .. }) => self.config.key_ids().len() + 1,
            Some(Step::PickBucket { names, .. }) => names.len(),
            _ => 0,
        }
    }

    fn focused_choice(&self, window: &Window) -> Option<usize> {
        self.choices
            .iter()
            .take(self.choice_count())
            .position(|choice| choice.is_focused(window))
    }

    fn move_choice(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.choice_count();
        if count == 0 {
            return;
        }
        let at = self.focused_choice(window).map_or(-1, |at| at as isize);
        let next = (at + step).clamp(0, count as isize - 1) as usize;
        self.choices[next].focus(window, cx);
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.dialog = None;
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    /// Enter, or the dialog's main button.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.as_ref().filter(|_| self.busy.is_none()) else {
            return;
        };
        match &dialog.step {
            Step::NewKey {
                purpose,
                key_id,
                secret,
            } => {
                let purpose = *purpose;
                let (key_id, secret) = (
                    key_id.read(cx).value().to_string(),
                    secret.read(cx).value().to_string(),
                );
                let busy = "Checking the key with Backblaze…";
                let sign_in = move || CheckedKey::new_key(&key_id, &secret);
                self.with_key(purpose, busy, sign_in, window, cx);
            }
            Step::Details {
                key,
                bucket,
                editing,
                name,
                folder,
            } => {
                let (key, bucket, editing) = (key.clone(), bucket.clone(), *editing);
                let (name, folder) = (
                    name.read(cx).value().to_string(),
                    folder.read(cx).value().to_string(),
                );
                self.save_details(key, bucket, editing, name, folder, window, cx);
            }
            Step::Remove { index } => {
                let index = *index;
                self.remove(index, window, cx);
            }
            // Enter takes the focused choice, as Space and a click on it do.
            Step::PickKey { purpose } => {
                let purpose = *purpose;
                let Some(at) = self.focused_choice(window) else {
                    return;
                };
                match self.config.key_ids().get(at) {
                    Some(key_id) => self.stored_key(purpose, key_id.clone(), window, cx),
                    None => self.new_key(purpose, window, cx),
                }
            }
            Step::PickBucket { key, names } => {
                let chosen = self.focused_choice(window).and_then(|at| names.get(at));
                let Some(bucket) = chosen.cloned() else {
                    return;
                };
                let key = key.clone();
                self.pick_bucket(key, bucket, window, cx);
            }
        }
    }

    // Adding and changing keys.

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked() {
            return;
        }
        if let Err(error) = manage::check_room(&self.config) {
            self.fail(error);
            cx.notify();
            return;
        }
        let step = self.key_step(Purpose::Add, window, cx);
        self.open(step, window, cx);
    }

    fn change_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected.filter(|_| !self.blocked()) else {
            return;
        };
        let step = self.key_step(Purpose::ChangeKey(index), window, cx);
        self.open(step, window, cx);
    }

    /// The first step of adding or re-keying: pick a stored key, or straight to a new one.
    fn key_step(&self, purpose: Purpose, window: &mut Window, cx: &mut Context<Self>) -> Step {
        if self.config.key_ids().is_empty() {
            return new_key_step(purpose, window, cx);
        }
        Step::PickKey { purpose }
    }

    fn new_key(&mut self, purpose: Purpose, window: &mut Window, cx: &mut Context<Self>) {
        let step = new_key_step(purpose, window, cx);
        self.go_to(step, window, cx);
    }

    fn stored_key(
        &mut self,
        purpose: Purpose,
        key_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let busy = "Reading the key from the keyring and checking it with Backblaze…";
        self.with_key(
            purpose,
            busy,
            move || CheckedKey::stored(&key_id),
            window,
            cx,
        );
    }

    /// Signs in with a key, then lists its buckets for a new entry, or moves the entry onto it.
    fn with_key(
        &mut self,
        purpose: Purpose,
        busy: &str,
        sign_in: impl FnOnce() -> flea_b2::error::Result<CheckedKey> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match purpose {
            Purpose::Add => self.run(
                busy,
                move || {
                    let mut key = sign_in()?;
                    let names = key.bucket_names()?;
                    Ok((key.choice, names))
                },
                |this, result: flea_b2::error::Result<(KeyChoice, Vec<String>)>, window, cx| {
                    match result {
                        Ok((key, names)) => this.go_to(Step::PickBucket { key, names }, window, cx),
                        Err(error) => this.dialog_error(error, cx),
                    }
                },
                window,
                cx,
            ),
            Purpose::ChangeKey(index) => {
                let mut config = self.config.clone();
                self.run(
                    busy,
                    move || {
                        let mut key = sign_in()?;
                        let text = manage::change_key(&mut config, index, &mut key)?;
                        Ok((config, text))
                    },
                    |this, result: flea_b2::error::Result<(Config, String)>, window, cx| {
                        this.applied(result, None, window, cx)
                    },
                    window,
                    cx,
                );
            }
        }
    }

    /// The outcome of a change written to the list: shown in the status, or in the open dialog.
    fn applied(
        &mut self,
        result: flea_b2::error::Result<(Config, String)>,
        select: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((config, text)) => {
                self.config = config;
                let count = self.config.buckets.len();
                let wanted = select.or(self.selected).unwrap_or(0);
                self.selected = (count > 0).then(|| wanted.min(count - 1));
                self.dialog = None;
                self.list_focus.focus(window, cx);
                self.say(text);
            }
            Err(error) => self.dialog_error(error, cx),
        }
    }

    fn pick_bucket(
        &mut self,
        key: KeyChoice,
        bucket: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = manage::suggested_name(&self.config, &bucket);
        let step = details_step(key, bucket, None, &name, "", window, cx);
        self.go_to(step, window, cx);
    }

    // Editing, removing, testing and ordering.

    fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected.filter(|_| !self.blocked()) else {
            return;
        };
        let entry = self.config.buckets[index].clone();
        let key = KeyChoice {
            key_id: entry.key_id.clone(),
            new_secret: None,
        };
        let step = details_step(
            key,
            entry.bucket,
            Some(index),
            &entry.name,
            &entry.folder,
            window,
            cx,
        );
        self.open(step, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn save_details(
        &mut self,
        key: KeyChoice,
        bucket: String,
        editing: Option<usize>,
        name: String,
        folder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (name, folder) = match manage::check_details(&self.config, &name, &folder, editing) {
            Ok(details) => details,
            Err(error) => return self.dialog_error(error, cx),
        };
        let entry = Entry {
            name,
            bucket,
            folder,
            key_id: key.key_id.clone(),
        };
        let mut config = self.config.clone();
        let select = editing.or(Some(config.buckets.len()));
        self.run(
            "Saving…",
            move || {
                let text = manage::save_entry(&mut config, editing, entry, &key)?;
                Ok((config, text))
            },
            move |this, result, window, cx| this.applied(result, select, window, cx),
            window,
            cx,
        );
    }

    fn ask_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected.filter(|_| !self.blocked()) else {
            return;
        };
        self.open(Step::Remove { index }, window, cx);
    }

    fn remove(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mut config = self.config.clone();
        self.run(
            "Removing…",
            move || {
                let text = manage::remove(&mut config, index)?;
                Ok((config, text))
            },
            move |this, result, window, cx| this.applied(result, Some(index), window, cx),
            window,
            cx,
        );
    }

    fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected.filter(|_| !self.blocked()) else {
            return;
        };
        let entry = self.config.buckets[index].clone();
        let busy = format!("Testing “{}”…", entry.name);
        let name = entry.name.clone();
        self.run(
            &busy,
            move || manage::test(&entry),
            move |this, result, _, _| match result {
                Ok(text) => this.say(text),
                Err(error) => this.fail(format!("{name}: {error}")),
            },
            window,
            cx,
        );
    }

    fn move_selected(&mut self, up: bool, cx: &mut Context<Self>) {
        let Some(index) = self.selected.filter(|_| !self.blocked()) else {
            return;
        };
        match manage::move_entry(&mut self.config, index, up) {
            Ok(Some(other)) => {
                self.selected = Some(other);
                self.say("Order saved. Flea's sidebar follows it.");
            }
            Ok(None) => {}
            Err(error) => self.fail(error),
        }
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.config.buckets.len() {
            self.selected = Some(index);
            cx.notify();
        }
    }

    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.config.buckets.len();
        if count == 0 {
            return;
        }
        let at = self.selected.unwrap_or(0) as isize;
        self.select((at + step).clamp(0, count as isize - 1) as usize, cx);
    }
}

fn new_key_step(purpose: Purpose, window: &mut Window, cx: &mut Context<Manager>) -> Step {
    Step::NewKey {
        purpose,
        key_id: cx.new(|cx| InputState::new(window, cx).placeholder("0051a2b3c4d5e6f0000000001")),
        secret: cx.new(|cx| InputState::new(window, cx).masked(true)),
    }
}

fn details_step(
    key: KeyChoice,
    bucket: String,
    editing: Option<usize>,
    name: &str,
    folder: &str,
    window: &mut Window,
    cx: &mut Context<Manager>,
) -> Step {
    let name = name.to_string();
    let folder = folder.to_string();
    Step::Details {
        key,
        bucket,
        editing,
        name: cx.new(|cx| InputState::new(window, cx).default_value(name)),
        folder: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("The whole bucket")
                .default_value(folder)
        }),
    }
}

// Drawing.

fn secondary(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_color(cx.omarchy().secondary).child(text.into())
}

/// A button with an icon before its label.
fn labelled(
    id: impl Into<gpui_kit::ElementId>,
    label: &str,
    name: IconName,
    variant: ButtonVariant,
    cx: &App,
) -> Button {
    let label: SharedString = label.to_string().into();
    button(id, "", variant, cx)
        .accessibility_label(label.clone())
        .child(icon(name).size(rems(0.875)))
        .child(label)
}

/// An icon-only button, named in its tooltip and for screen readers.
fn icon_button(id: &'static str, label: &str, name: IconName, cx: &App) -> Button {
    let label: SharedString = label.to_string().into();
    with_tooltip(
        button(id, "", ButtonVariant::Secondary, cx)
            .accessibility_label(label.clone())
            .child(icon(name)),
        label,
    )
}

/// The list's columns, as fractions of its width.
const COLUMNS: [(&str, f32); 4] = [
    ("Name in Flea", 0.3),
    ("Bucket", 0.26),
    ("Folder", 0.24),
    ("Key ID", 0.2),
];

impl Manager {
    fn header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.omarchy();
        div()
            .flex()
            .items_center()
            .gap(rems(1.))
            .px(rems(1.))
            .py(rems(0.75))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(rems(0.125))
                    .child(
                        div()
                            .text_size(rems(0.875))
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.bright)
                            .child("Backblaze B2 in Flea"),
                    )
                    .child(secondary(
                        "The buckets in Flea's sidebar, in this order. Removing one deletes nothing in B2.",
                        cx,
                    )),
            )
            // An empty list offers its own button, in its middle.
            .when(!self.config.buckets.is_empty(), |header| {
                header.child(
                    labelled("add", "Add bucket", IconName::Plus, ButtonVariant::Primary, cx)
                        .disabled(self.blocked())
                        .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                )
            })
    }

    fn list(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.omarchy().clone();
        let columns = div()
            .flex()
            .px(rems(1.))
            .py(rems(0.375))
            .text_color(t.secondary)
            .children(COLUMNS.iter().map(|(label, width)| {
                div()
                    .w(relative(*width))
                    .pr(rems(0.75))
                    .truncate()
                    .child(*label)
            }));
        let rows = self
            .config
            .buckets
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let selected = self.selected == Some(index);
                let folder: SharedString = if entry.folder.is_empty() {
                    "(whole bucket)".into()
                } else {
                    entry.folder.clone().into()
                };
                let cells = [
                    (entry.name.clone().into(), t.bright),
                    (entry.bucket.clone().into(), t.foreground),
                    (
                        folder,
                        if entry.folder.is_empty() {
                            t.secondary
                        } else {
                            t.foreground
                        },
                    ),
                    (entry.key_id.clone().into(), t.secondary),
                ];
                div()
                    .id(("bucket", index))
                    .flex()
                    .items_center()
                    .h(rems(2.))
                    .pl(rems(0.875))
                    .pr(rems(1.))
                    .border_l_2()
                    .border_color(if selected {
                        t.accent
                    } else {
                        transparent_black()
                    })
                    .when(selected, |row| row.bg(t.selected_fill()))
                    .when(!selected, |row| row.hover(|style| style.bg(t.hover_fill())))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.list_focus.focus(window, cx);
                        this.select(index, cx);
                        if event.click_count() >= 2 {
                            this.edit(window, cx);
                        }
                    }))
                    .children(cells.into_iter().zip(COLUMNS).map(
                        |((text, color), (_, width)): ((SharedString, _), _)| {
                            div()
                                .w(relative(width))
                                .pr(rems(0.75))
                                .truncate()
                                .text_color(color)
                                .child(text)
                        },
                    ))
            })
            .collect::<Vec<_>>();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(columns)
            .child(separator(cx))
            .child(
                div()
                    .id("bucket-rows")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }

    /// No entry yet: what one needs, and the way to add it.
    fn empty(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // Centred by its margins, so a window too small for it scrolls rather than clipping its top.
        div()
            .id("empty")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .p(rems(1.))
            .child(
            empty_state(
                "No bucket yet",
                "Add a bucket to see it in Flea's sidebar. You need a Backblaze application key with read and write access to it.",
                cx,
            )
            .w_full()
            .max_w(rems(30.))
            .m_auto()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rems(1.))
                    .pt(rems(0.5))
                    .child(
                        labelled("add-first", "Add bucket", IconName::Plus, ButtonVariant::Primary, cx)
                            .disabled(self.blocked())
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    )
                    .child(link("app-keys", "Create a key", APP_KEYS_URL, cx)),
            ),
        )
    }

    /// What can be done to the selected entry.
    fn actions(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let none = self.selected.is_none() || self.blocked();
        let index = self.selected.unwrap_or(0);
        let last = self.config.buckets.len().saturating_sub(1);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(rems(0.375))
            .px(rems(1.))
            .py(rems(0.5))
            .child(
                labelled(
                    "edit",
                    "Edit",
                    IconName::Pencil,
                    ButtonVariant::Secondary,
                    cx,
                )
                .disabled(none)
                .on_click(cx.listener(|this, _, window, cx| this.edit(window, cx))),
            )
            .child(
                labelled(
                    "change-key",
                    "Change key",
                    IconName::Key,
                    ButtonVariant::Secondary,
                    cx,
                )
                .disabled(none)
                .on_click(cx.listener(|this, _, window, cx| this.change_key(window, cx))),
            )
            .child(
                labelled("test", "Test", IconName::Plug, ButtonVariant::Secondary, cx)
                    .disabled(none)
                    .on_click(cx.listener(|this, _, window, cx| this.test(window, cx))),
            )
            .child(
                icon_button("move-up", "Move up", IconName::ArrowUp, cx)
                    .disabled(none || index == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.move_selected(true, cx))),
            )
            .child(
                icon_button("move-down", "Move down", IconName::ArrowDown, cx)
                    .disabled(none || index == last)
                    .on_click(cx.listener(|this, _, _, cx| this.move_selected(false, cx))),
            )
            .child(div().flex_1())
            .child(
                labelled(
                    "remove",
                    "Remove from Flea",
                    IconName::Trash,
                    ButtonVariant::Danger,
                    cx,
                )
                .disabled(none)
                .on_click(cx.listener(|this, _, window, cx| this.ask_remove(window, cx))),
            )
    }

    /// The last outcome, or what the window waits for, and the keys that apply, when there is room.
    fn status_bar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.omarchy();
        let message = match (&self.busy, &self.status) {
            (Some(busy), _) => Some((busy.clone(), t.secondary, IconName::LoaderCircle)),
            (None, Some((text, Status::Error))) => {
                Some((text.clone(), t.danger, IconName::CircleAlert))
            }
            (None, Some((text, _))) => Some((text.clone(), t.success, IconName::CircleCheck)),
            (None, None) => None,
        };
        let room = window.viewport_size().width >= rems(56.).to_pixels(window.rem_size());
        let hints: &[(&str, &str)] = if room && !self.config.buckets.is_empty() {
            &[
                ("a", "add"),
                ("e", "edit"),
                ("c", "key"),
                ("t", "test"),
                ("d", "remove"),
                ("⇧↑↓", "move"),
            ]
        } else {
            &[]
        };
        div()
            .flex()
            .items_center()
            .gap(rems(1.))
            .min_h(rems(2.25))
            .px(rems(1.))
            .py(rems(0.375))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.5))
                    .when_some(message, |bar, (text, color, name)| {
                        bar.text_color(color)
                            .child(icon(name).size(rems(0.875)).flex_none())
                            .child(div().min_w_0().child(text))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(rems(0.75))
                    .text_color(t.secondary)
                    .children(hints.iter().map(|&(key, what)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(rems(0.25))
                            .child(keycap(key, cx))
                            .child(what)
                    })),
            )
    }

    fn render_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let dialog_state = self.dialog.as_ref()?;
        let t = cx.omarchy().clone();
        let busy = self.busy.clone();
        let error = dialog_state.error.clone();
        let entry_name = |index: usize| {
            self.config
                .buckets
                .get(index)
                .map(|e| e.name.clone())
                .unwrap_or_default()
        };
        let (title, description): (String, String) = match &dialog_state.step {
            Step::PickKey { purpose: Purpose::Add } | Step::NewKey { purpose: Purpose::Add, .. } => (
                "Add a bucket".into(),
                "Which application key opens it? Each key is stored once in your keyring, and several buckets can share it.".into(),
            ),
            Step::PickKey { purpose: Purpose::ChangeKey(index) }
            | Step::NewKey { purpose: Purpose::ChangeKey(index), .. } => (
                format!("Change the key of “{}”", entry_name(*index)),
                format!(
                    "The key must be able to open the bucket {}, for example after you rotated keys.",
                    self.config.buckets.get(*index).map(|e| e.bucket.as_str()).unwrap_or_default()
                ),
            ),
            Step::PickBucket { .. } => (
                "Which bucket?".into(),
                "The buckets this key can open.".into(),
            ),
            Step::Details { editing: None, .. } => (
                "Add a bucket".into(),
                "How Flea shows it.".into(),
            ),
            Step::Details { editing: Some(index), .. } => (
                format!("Edit “{}”", entry_name(*index)),
                "How Flea shows it.".into(),
            ),
            Step::Remove { index } => (
                format!("Remove “{}” from Flea?", entry_name(*index)),
                "Nothing is deleted in B2: only this entry goes.".into(),
            ),
        };
        let disabled = busy.is_some();
        let mut primary: Option<(&str, ButtonVariant)> = None;
        let body: gpui_kit::AnyElement = match &dialog_state.step {
            Step::PickKey { purpose } => {
                let purpose = *purpose;
                let mut choices = choice_list(cx);
                for (index, key_id) in self.config.key_ids().into_iter().enumerate() {
                    let users = manage::users_of(&self.config, &key_id).join(", ");
                    let id = key_id.clone();
                    choices = choices.child(
                        choice(("key", index), self.choices.get(index), cx)
                            .disabled(disabled)
                            .child(div().flex_none().text_color(t.bright).child(key_id.clone()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(t.secondary)
                                    .child(format!("used by {users}")),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.stored_key(purpose, id.clone(), window, cx)
                            })),
                    );
                }
                choices
                    .child(
                        choice("new-key", self.choices.get(self.config.key_ids().len()), cx)
                            .disabled(disabled)
                            .child(icon(IconName::Plus).size(rems(0.875)))
                            .child("New key…")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.new_key(purpose, window, cx)
                            })),
                    )
                    .into_any_element()
            }
            Step::NewKey { key_id, secret, .. } => {
                primary = Some(("Check key", ButtonVariant::Primary));
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.375))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(rems(0.25))
                            .text_color(t.secondary)
                            .child("Create one with read and write access at")
                            .child(link("new-key-link", "secure.backblaze.com", APP_KEYS_URL, cx)),
                    )
                    .child(field("Key ID", input("key-id", key_id, window, cx), None, cx))
                    .child(field(
                        "Application key",
                        input("secret", secret, window, cx),
                        Some("Shown once by Backblaze when the key is created. It is kept only in your keyring."),
                        cx,
                    ))
                    .into_any_element()
            }
            Step::PickBucket { key, names } => {
                let mut choices = choice_list(cx);
                for (index, name) in names.iter().enumerate() {
                    let (key, bucket) = (key.clone(), name.clone());
                    choices = choices.child(
                        choice(("bucket-choice", index), self.choices.get(index), cx)
                            .disabled(disabled)
                            .child(icon(IconName::Cloud).size(rems(0.875)))
                            .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_bucket(key.clone(), bucket.clone(), window, cx)
                            })),
                    );
                }
                choices.into_any_element()
            }
            Step::Details {
                key,
                bucket,
                editing,
                name,
                folder,
            } => {
                primary = Some((
                    if editing.is_some() {
                        "Save"
                    } else {
                        "Add bucket"
                    },
                    ButtonVariant::Primary,
                ));
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.375))
                    .child(fact("Bucket", bucket.clone(), cx))
                    .child(fact("Key ID", key.key_id.clone(), cx))
                    .child(field(
                        "Name in Flea",
                        input("name", name, window, cx),
                        Some("The sidebar row and the folder name in Flea."),
                        cx,
                    ))
                    .child(field(
                        "Folder",
                        input("folder", folder, window, cx),
                        Some("Optional: show only this folder of the bucket."),
                        cx,
                    ))
                    .into_any_element()
            }
            Step::Remove { index } => {
                primary = Some(("Remove", ButtonVariant::Danger));
                let key_id = self
                    .config
                    .buckets
                    .get(*index)
                    .map(|e| e.key_id.clone())
                    .unwrap_or_default();
                let last_user = manage::users_of(&self.config, &key_id).len() == 1;
                div()
                    .when(last_user, |body| {
                        body.child(secondary(
                            format!(
                                "No other entry uses key {key_id}, so it leaves the keyring too."
                            ),
                            cx,
                        ))
                    })
                    .into_any_element()
            }
        };
        let close = cx.listener(|this, confirmed: &bool, window, cx| {
            if *confirmed {
                this.confirm(window, cx)
            } else {
                this.close_dialog(window, cx)
            }
        });
        let footer = div()
            .flex()
            .items_center()
            .justify_end()
            .gap(rems(0.5))
            .child(
                dialog_button("cancel", "Cancel", ButtonVariant::Secondary, cx)
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            )
            .when_some(primary, |footer, (label, variant)| {
                footer.child(
                    dialog_button("confirm", label, variant, cx)
                        .disabled(disabled)
                        .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                )
            });
        // The title and the buttons stay in view; what lies between scrolls in a short window.
        let content = div()
            .id("dialog-content")
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(dialog_description(description, cx))
            .child(div().pt(rems(0.25)).child(body));
        let popup = dialog_popup(cx)
            .w(rems(32.))
            .max_h(window.viewport_size().height - px(16.))
            .child(dialog_title(title, cx))
            .child(content)
            .when_some(busy, |popup, text| {
                popup.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.5))
                        .text_color(t.secondary)
                        .child(icon(IconName::LoaderCircle).size(rems(0.875)))
                        .child(text),
                )
            })
            .when_some(error, |popup, text| {
                popup.child(alert(text, Status::Error, cx))
            })
            .child(footer);
        Some(
            dialog(&self.dialog_focus, cx)
                .popup(popup)
                .request_close(move |confirmed, window, cx| close(&confirmed, window, cx)),
        )
    }
}

/// One choice in a dialog's list, chosen with a click, Enter or Space.
fn choice(id: impl Into<gpui_kit::ElementId>, focus: Option<&FocusHandle>, cx: &App) -> Button {
    button(id, "", ButtonVariant::Outline, cx)
        .w_full()
        .justify_start()
        .gap(rems(0.75))
        .when_some(focus, |choice, focus| choice.track_focus(focus))
}

/// The column a step's choices go in, walked with the arrows.
fn choice_list(cx: &mut Context<Manager>) -> Div {
    div()
        .key_context(CHOICES_CONTEXT)
        .flex()
        .flex_col()
        .gap(rems(0.25))
        .on_action(
            cx.listener(|this, _: &SelectPrevious, window, cx| this.move_choice(-1, window, cx)),
        )
        .on_action(cx.listener(|this, _: &SelectNext, window, cx| this.move_choice(1, window, cx)))
}

/// A form field: its label, the control, and a hint under it.
fn field(label: &str, control: impl IntoElement, hint: Option<&str>, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(rems(0.25))
        .pt(rems(0.25))
        .child(
            div()
                .text_color(cx.omarchy().bright)
                .child(label.to_string()),
        )
        .child(control)
        .when_some(hint, |field, hint| {
            field.child(secondary(hint.to_string(), cx))
        })
}

/// A value the dialog shows but does not edit.
fn fact(label: &str, value: String, cx: &App) -> Div {
    div()
        .flex()
        .gap(rems(0.75))
        .child(
            div()
                .w(rems(6.))
                .flex_none()
                .text_color(cx.omarchy().secondary)
                .child(label.to_string()),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(cx.omarchy().bright)
                .child(value),
        )
}

impl Render for Manager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(BASE_REM_SIZE * cx.global::<Desktop>().text_scale));
        // Nothing focused, such as after a click on empty space: the list takes the keys back.
        if window.focused(cx).is_none() {
            cx.defer_in(window, |this, window, cx| {
                if window.focused(cx).is_none() {
                    this.focus_step(window, cx);
                }
            });
        }
        let t = cx.omarchy().clone();
        let blocked = self.dialog.is_some();
        let header = self.header(cx);
        let body = match (&self.problem, self.config.buckets.is_empty()) {
            (Some(problem), _) => div()
                .id("problem")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .gap(rems(0.5))
                .p(rems(1.))
                .child(alert(problem.clone(), Status::Error, cx))
                .child(secondary(
                    "Fix or delete the file, then reopen the manager. Nothing is changed until then.",
                    cx,
                ))
                .into_any_element(),
            (None, true) => self.empty(cx).into_any_element(),
            (None, false) => self.list(cx).into_any_element(),
        };
        let actions = (!self.config.buckets.is_empty()).then(|| self.actions(cx));
        let status = self.status_bar(window, cx);
        let dialog = self.render_dialog(window, cx);
        let list = div()
            .id("bucket-list")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.list_focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            // An open dialog keeps the keyboard to itself.
            .when(!blocked, |list| {
                list.on_action(cx.listener(|this, _: &AddBucket, window, cx| this.add(window, cx)))
                    .on_action(
                        cx.listener(|this, _: &EditBucket, window, cx| this.edit(window, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &ChangeKey, window, cx| this.change_key(window, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &TestBucket, window, cx| this.test(window, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &RemoveBucket, window, cx| {
                            this.ask_remove(window, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.move_selected(true, cx)))
                    .on_action(
                        cx.listener(|this, _: &MoveDown, _, cx| this.move_selected(false, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &SelectPrevious, _, cx| this.select_by(-1, cx)),
                    )
                    .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_by(1, cx)))
            })
            .child(body);
        focus_scope("manager")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(desktop::root_background(window, cx))
            .text_color(t.foreground)
            .font_family(t.mono_font.clone())
            .text_size(rems(0.75))
            .child(header)
            .child(separator(cx))
            .child(list)
            .when_some(actions, |root, actions| {
                root.child(separator(cx)).child(actions)
            })
            .child(separator(cx))
            .child(status)
            .children(dialog)
    }
}
