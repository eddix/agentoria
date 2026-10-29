mod config_catalog;
mod config_view;
mod indexer;
mod preferences;
mod process_view;
mod raw_view;
mod session_search;
mod session_view;
mod skill_catalog;
mod skills_view;
mod themes;
mod workdir_filter;
mod workspace;

extern crate gpui_kit as gpui;

use gpui::{
    px, size, App, AppContext, Bounds, KeyBinding, Menu, MenuItem, QuitMode, TaskExt, WindowBounds,
    WindowOptions,
};
use gpui_kit::{assets::Assets, component::TitleBar};
use preferences::Preferences;
use workspace::WorkspaceView;

gpui::actions!(agentoria, [Quit]);

#[cfg(not(any(feature = "preview", feature = "formal")))]
compile_error!("either preview or formal feature must be enabled");

#[cfg(feature = "formal")]
pub(crate) const APP_ID: &str = "com.eddix.agentoria";
#[cfg(not(feature = "formal"))]
pub(crate) const APP_ID: &str = "com.eddix.agentoria.gpui-preview";

#[cfg(feature = "formal")]
pub(crate) const IS_PREVIEW: bool = false;
#[cfg(not(feature = "formal"))]
pub(crate) const IS_PREVIEW: bool = true;

fn main() {
    if let Err(error) = indexer::migrate_legacy_data() {
        eprintln!("failed to migrate Agentoria data: {error:#}");
    }
    let preferences = Preferences::load();
    gpui_kit::application()
        .with_assets(Assets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            let app_name = if IS_PREVIEW {
                "Agentoria Preview"
            } else {
                "Agentoria"
            };
            cx.set_menus([
                Menu::new(app_name).items([MenuItem::action(format!("Quit {app_name}"), Quit)])
            ]);
            if let Err(error) = themes::register_builtin_themes(cx) {
                eprintln!("failed to register Agentoria themes: {error:#}");
            }
            themes::apply_saved_themes(
                &preferences.light_theme,
                &preferences.dark_theme,
                preferences.active_theme_name(),
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
