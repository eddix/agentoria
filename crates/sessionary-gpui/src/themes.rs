use anyhow::{Context as _, Result};
use gpui::{App, Window};
use gpui_component::{Theme, ThemeMode, ThemeRegistry};

const BUILTIN_THEMES: &[(&str, &str)] = &[
    (
        "adventure",
        include_str!("../../../assets/themes/adventure.json"),
    ),
    ("alduin", include_str!("../../../assets/themes/alduin.json")),
    (
        "asciinema",
        include_str!("../../../assets/themes/asciinema.json"),
    ),
    ("aurora", include_str!("../../../assets/themes/aurora.json")),
    ("ayu", include_str!("../../../assets/themes/ayu.json")),
    (
        "catppuccin",
        include_str!("../../../assets/themes/catppuccin.json"),
    ),
    (
        "everforest",
        include_str!("../../../assets/themes/everforest.json"),
    ),
    (
        "fahrenheit",
        include_str!("../../../assets/themes/fahrenheit.json"),
    ),
    (
        "flexoki",
        include_str!("../../../assets/themes/flexoki.json"),
    ),
    (
        "gruvbox",
        include_str!("../../../assets/themes/gruvbox.json"),
    ),
    ("harper", include_str!("../../../assets/themes/harper.json")),
    ("hybrid", include_str!("../../../assets/themes/hybrid.json")),
    (
        "jellybeans",
        include_str!("../../../assets/themes/jellybeans.json"),
    ),
    ("kibble", include_str!("../../../assets/themes/kibble.json")),
    (
        "macos-classic",
        include_str!("../../../assets/themes/macos-classic.json"),
    ),
    (
        "mellifluous",
        include_str!("../../../assets/themes/mellifluous.json"),
    ),
    (
        "molokai",
        include_str!("../../../assets/themes/molokai.json"),
    ),
    (
        "solarized",
        include_str!("../../../assets/themes/solarized.json"),
    ),
    (
        "spaceduck",
        include_str!("../../../assets/themes/spaceduck.json"),
    ),
    (
        "tokyonight",
        include_str!("../../../assets/themes/tokyonight.json"),
    ),
    (
        "twilight",
        include_str!("../../../assets/themes/twilight.json"),
    ),
];

pub fn register_builtin_themes(cx: &mut App) -> Result<()> {
    let registry = ThemeRegistry::global_mut(cx);
    for (name, content) in BUILTIN_THEMES {
        registry
            .load_themes_from_str(content)
            .with_context(|| format!("failed to load builtin theme set {name}"))?;
    }
    Ok(())
}

pub fn apply_saved_themes(light: &str, dark: &str, active: &str, cx: &mut App) {
    if let Some(config) = theme_config(light, cx) {
        Theme::global_mut(cx).apply_config(&config);
    }
    if let Some(config) = theme_config(dark, cx) {
        Theme::global_mut(cx).apply_config(&config);
    }
    apply_theme(active, None, cx);
}

pub fn apply_theme(name: &str, window: Option<&mut Window>, cx: &mut App) -> Option<ThemeMode> {
    let config = theme_config(name, cx)?;
    let mode = config.mode;
    Theme::global_mut(cx).apply_config(&config);
    Theme::change(mode, window, cx);
    cx.refresh_windows();
    Some(mode)
}

pub fn available_themes(cx: &App) -> Vec<(String, ThemeMode)> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .map(|theme| (theme.name.to_string(), theme.mode))
        .collect()
}

fn theme_config(name: &str, cx: &App) -> Option<std::rc::Rc<gpui_component::ThemeConfig>> {
    ThemeRegistry::global(cx).themes().get(name).cloned()
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use gpui_component::Theme;

    use super::{apply_theme, available_themes, register_builtin_themes};

    #[gpui::test]
    fn builtin_theme_registry_switches_named_theme(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| register_builtin_themes(cx).expect("builtin themes should load"));
        cx.update(|cx| {
            assert!(available_themes(cx).len() > 20);
        });
        cx.update(|cx| {
            assert!(apply_theme("Flexoki Light", None, cx).is_some());
            assert_eq!(Theme::global(cx).theme_name().as_ref(), "Flexoki Light");
        });
    }
}
