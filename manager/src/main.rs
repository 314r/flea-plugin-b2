//! The Backblaze B2 bucket manager for Flea, in a window: which buckets Flea shows in its sidebar,
//! with which key, folder and order. Flea's "Manage buckets…" row starts it; it also runs on its
//! own. It edits ~/.config/flea-b2/buckets.json, which the plugin reads, and the keyring.

// GPUI macros expand through the canonical `gpui` crate path.
extern crate gpui_kit as gpui;

mod app;
mod assets;
mod desktop;
mod instance;

use desktop::Desktop;
use gpui_kit::{App, AppContext, Bounds, WindowBounds, WindowDecorations, WindowOptions, px, size};
use instance::Instance;

const APP_ID: &str = "flea-b2-manager";

fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        app_id: Some(APP_ID.into()),
        // Hyprland draws the border and handles tiling: no title bar of our own.
        titlebar: None,
        window_decorations: Some(WindowDecorations::Server),
        // Set from the start, so the first frame isn't a solid one.
        window_background: cx.global::<Desktop>().transparency.surface(),
        window_min_size: Some(size(px(600.), px(360.))),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(940.), px(600.)),
            cx,
        ))),
        ..Default::default()
    }
}

fn main() {
    if std::env::args().nth(1).is_some() {
        println!(
            "flea-b2-manager: the Backblaze B2 bucket manager for Flea.\n\n\
             It takes no arguments. Flea's \u{201c}Manage buckets\u{2026}\u{201d} row opens it."
        );
        return;
    }
    let mut first = match instance::acquire() {
        Instance::First(first) => first,
        Instance::Second => return,
    };
    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(move |cx| {
            gpui_omarchy::init(cx);
            app::init(cx);
            desktop::follow(cx);
            let options = window_options(cx);
            let window = cx
                .open_window(options, |window, cx| {
                    window.set_window_title("Backblaze B2 buckets");
                    cx.new(|cx| app::Manager::new(window, cx))
                })
                .expect("open the manager window");
            if let Some(activations) = first.activations.take() {
                cx.spawn(async move |cx| {
                    while activations.recv().await.is_ok() {
                        let _ = cx.update(|cx| {
                            window
                                .update(cx, |_, window, _| window.activate_window())
                                .ok()
                        });
                    }
                })
                .detach();
            }
            // Kept for the life of the app: the lock goes with it.
            cx.set_global(first);
            cx.activate(true);
        });
}
