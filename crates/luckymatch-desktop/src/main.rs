#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_view;
mod audio_capture;
mod backend;
mod win32_window;

use gpui::{
    prelude::*, px, size, App, Application, Bounds, Focusable, WindowBackgroundAppearance,
    WindowBounds, WindowOptions,
};

use crate::app_view::LuckyMatchView;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    Application::new().run(|cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(560.0), px(720.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            window_background: WindowBackgroundAppearance::Opaque,
            is_movable: true,
            is_resizable: true,
            is_minimizable: true,
            ..Default::default()
        };

        let window = cx
            .open_window(options, |window, cx| {
                cx.new(|cx| LuckyMatchView::new(window, cx))
            })
            .expect("failed to open LuckyMatch desktop window");

        let _ = window.update(cx, |view, window, cx| {
            window.focus(&view.focus_handle(cx));
        });

        cx.activate(true);
    });
}
