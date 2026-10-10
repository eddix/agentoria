use agentoria_core::index::{read_line_chunk, IndexedLine};
use gpui::{div, px, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window};
use gpui_kit::component::{
    button::Button, h_flex, skeleton::Skeleton, v_flex, ActiveTheme, Disableable as _, Sizable as _,
};

const PAGE_BYTES: usize = 64 * 1024;

pub struct RawRecordView {
    line: IndexedLine,
    page: usize,
    content: Option<String>,
    loading: bool,
}

impl RawRecordView {
    pub fn new(line: IndexedLine, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            line,
            page: 0,
            content: None,
            loading: false,
        };
        view.load_page(cx);
        view
    }

    fn page_count(&self) -> usize {
        usize::try_from(self.line.byte_len)
            .unwrap_or(usize::MAX)
            .max(1)
            .div_ceil(PAGE_BYTES)
    }

    fn set_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.page = page.min(self.page_count().saturating_sub(1));
        self.content = None;
        self.load_page(cx);
        cx.notify();
    }

    fn load_page(&mut self, cx: &mut Context<Self>) {
        let line = self.line.clone();
        let offset = (self.page as u64).saturating_mul(PAGE_BYTES as u64);
        self.loading = true;
        let load = cx.background_spawn(async move {
            smol::unblock(move || {
                read_line_chunk(&line, offset, PAGE_BYTES)
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .map_err(|error| format!("{error:#}"))
            })
            .await
        });
        cx.spawn(async move |weak, cx| {
            let result = load.await;
            let _ = weak.update(cx, |view, cx| {
                view.loading = false;
                view.content = Some(match result {
                    Ok(content) => content,
                    Err(error) => format!("Failed to load raw record: {error}"),
                });
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for RawRecordView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.page;
        let page_count = self.page_count();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} bytes · page {} of {} · 64 KiB per page",
                                self.line.byte_len,
                                page + 1,
                                page_count
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("raw-previous")
                                    .xsmall()
                                    .outline()
                                    .label("Previous")
                                    .disabled(page == 0 || self.loading)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.set_page(view.page.saturating_sub(1), cx)
                                    })),
                            )
                            .child(
                                Button::new("raw-next")
                                    .xsmall()
                                    .outline()
                                    .label("Next")
                                    .disabled(page + 1 >= page_count || self.loading)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.set_page(view.page.saturating_add(1), cx)
                                    })),
                            ),
                    ),
            )
            .child(if let Some(content) = &self.content {
                div()
                    .p_3()
                    .rounded_md()
                    .bg(cx.theme().muted.opacity(0.35))
                    .text_xs()
                    .line_height(px(18.))
                    .child(content.clone())
                    .into_any_element()
            } else {
                Skeleton::new().h_20().rounded_md().into_any_element()
            })
    }
}
