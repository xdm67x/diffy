// The mock feature answers the GitHub and Claude calls before the real code, so
// the rest of those functions reads as unreachable in a mock build.
#![cfg_attr(feature = "mock", allow(unreachable_code, unused_variables))]

mod api;
mod app;
mod ask;
mod claude;
mod exec;
mod http;
mod log;
#[cfg(feature = "mock")]
mod mock;
mod models;
mod review;
mod rich_text;
mod state;
mod store;
mod theme;
mod time;
mod update;

use app::{Diffy, bind_keys};
use color_eyre::Result;
use gpui_kit::*;

/// Opens the Diffy window and brings the app to the front. On macOS this is
/// also what the dock icon does after the window was closed.
fn open_diffy(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1180.), px(790.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(880.), px(560.))),
        focus: true,
        titlebar: Some(TitlebarOptions {
            // The sidebar runs to the top edge and carries the wordmark, so the
            // native title bar is transparent and the lights sit inside it.
            title: Some("".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(18.), px(14.))),
        }),
        ..Default::default()
    };

    match open_window(options, cx, |window, cx| {
        let view = cx.new(|cx| Diffy::new(window, cx));
        view.update(cx, |view, cx| {
            view.refresh(cx);
            window.focus(view.focus_handle(), cx);
        });
        view
    }) {
        // A window opened from the terminal does not take focus on its own.
        Ok(_) => cx.activate(true),
        Err(error) => {
            log::error(format!("could not open the Diffy window: {error:#}"));
            eprintln!("could not open the Diffy window: {error:#}");
        }
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    log::info(format!("diffy {} starting", env!("CARGO_PKG_VERSION")));

    let application = application().with_assets(assets::Assets);
    // Closing the last window keeps Diffy in the dock on macOS; other
    // platforms keep the default (quit with the last window).
    #[cfg(target_os = "macos")]
    let application = application.with_quit_mode(QuitMode::Explicit);

    application.on_reopen(|cx| match cx.windows().into_iter().next() {
        Some(window) => {
            cx.activate(true);
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
        None => open_diffy(cx),
    });

    application.run(|cx| {
        init(cx);
        theme::install(cx);
        http::install(cx);
        bind_keys(cx);
        open_diffy(cx);
    });

    log::info("diffy exited");
    Ok(())
}
