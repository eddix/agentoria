//! Native equivalents of affectUI Classic's shared visual primitives.
use gpui::{div, px, App, Div, ElementId, Hsla, IntoElement, ParentElement, Styled};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable as _, StyledExt as _,
};

pub const TEXT: f32 = 13.;
pub const META: f32 = 12.;
pub const BODY: f32 = 14.;
pub const CONTROL: f32 = 32.;
pub const SIDEBAR: f32 = 320.;

pub fn button(id: impl Into<ElementId>) -> Button {
    Button::new(id)
        .small()
        .h(px(CONTROL))
        .text_size(px(TEXT))
        .rounded(px(10.))
}

pub fn icon_button(id: impl Into<ElementId>, icon: IconName, label: &'static str) -> Button {
    button(id).ghost().icon(icon).tooltip(label).label(label)
}

pub fn caption(label: impl IntoElement, cx: &App) -> Div {
    div()
        .text_size(px(META))
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

pub fn badge(label: impl IntoElement, ink: Hsla, wash: Hsla) -> Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .py(px(2.))
        .rounded_full()
        .text_size(px(META))
        .line_height(px(16.))
        .font_medium()
        .text_color(ink)
        .bg(wash)
        .child(label)
}

pub fn count(count: usize, cx: &App) -> Div {
    badge(
        count.to_string(),
        cx.theme().muted_foreground,
        cx.theme().muted,
    )
}

pub fn shortcut(label: &'static str, cx: &App) -> Div {
    div()
        .px(px(5.))
        .h(px(20.))
        .rounded(px(5.))
        .bg(cx.theme().background)
        .shadow_sm()
        .border_1()
        .border_color(cx.theme().border)
        .text_size(px(10.))
        .line_height(px(18.))
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

pub fn empty(
    icon: IconName,
    title: &'static str,
    description: &'static str,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(32.))
        .child(
            h_flex()
                .size(px(48.))
                .mb(px(8.))
                .justify_center()
                .rounded(px(12.))
                .border_1()
                .border_color(cx.theme().border)
                .shadow_sm()
                .bg(cx.theme().background)
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(icon).size(px(18.))),
        )
        .child(div().text_size(px(15.)).font_medium().child(title))
        .child(
            div()
                .max_w(px(360.))
                .text_center()
                .text_size(px(TEXT))
                .line_height(px(20.))
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
}

/// A selection marker with explicit insets, so it cannot extend scroll bounds.
pub fn selection_marker(cx: &App) -> Div {
    div()
        .absolute()
        .left_0()
        .top(px(12.))
        .bottom(px(12.))
        .w(px(2.))
        .bg(cx.theme().primary)
}
