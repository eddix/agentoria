mod indexer;
mod preferences;
mod process_view;
mod raw_view;
mod session_view;
mod workspace;

use gpui::{px, size, App, AppContext, Bounds, TaskExt, WindowBounds, WindowOptions};
use gpui_component::{Theme, ThemeMode, TitleBar};
use gpui_component_assets::Assets;
use preferences::{Preferences, ThemePreference};
use workspace::WorkspaceView;

#[cfg(not(any(feature = "preview", feature = "formal")))]
compile_error!("either preview or formal feature must be enabled");

#[cfg(feature = "formal")]
pub(crate) const APP_ID: &str = "com.eddix.sessionary";
#[cfg(not(feature = "formal"))]
pub(crate) const APP_ID: &str = "com.eddix.sessionary.gpui-preview";

#[cfg(feature = "formal")]
pub(crate) const IS_PREVIEW: bool = false;
#[cfg(not(feature = "formal"))]
pub(crate) const IS_PREVIEW: bool = true;

fn main() {
    let preferences = Preferences::load();
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            Theme::change(
                match preferences.theme {
                    ThemePreference::Light => ThemeMode::Light,
                    ThemePreference::Dark => ThemeMode::Dark,
                },
                None,
                cx,
            );

            let window_options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1440.), px(920.)),
                    cx,
                ))),
                window_min_size: Some(size(px(1120.), px(700.))),
                titlebar: Some(TitleBar::title_bar_options()),
                app_id: Some(APP_ID.to_string()),
                ..Default::default()
            };
            let preferences = preferences.clone();
            cx.spawn(async move |cx| {
                cx.open_window(window_options, |window, cx| {
                    let workspace = cx.new(|cx| WorkspaceView::new(preferences, cx));
                    cx.new(|cx| workspace::root_view(workspace, window, cx))
                })?;
                Ok::<_, anyhow::Error>(())
            })
            .detach_and_log_err(cx);
        });
}
