use std::collections::HashMap;

use agentoria_core::{
    index::{load_process_line, IndexedLine, ProcessBody},
    SessionEvent,
};
use gpui::{
    div, px, AnyElement, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window,
};
use gpui_kit::component::{
    button::Button, h_flex, skeleton::Skeleton, v_flex, ActiveTheme, Disableable as _,
    Sizable as _, StyledExt as _, WindowExt as _,
};

use crate::raw_view::RawRecordView;

const PAGE_SIZE: usize = 50;
const MAX_INLINE_PROCESS_BYTES: u64 = 512 * 1024;

pub struct ProcessDetailsView {
    lines: Vec<IndexedLine>,
    page: usize,
    bodies: HashMap<usize, ProcessBody>,
    loading: bool,
}

impl ProcessDetailsView {
    pub fn new(lines: Vec<IndexedLine>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            lines,
            page: 0,
            bodies: HashMap::new(),
            loading: false,
        };
        view.load_page(cx);
        view
    }

    fn page_count(&self) -> usize {
        self.lines.len().max(1).div_ceil(PAGE_SIZE)
    }

    fn page_range(&self) -> std::ops::Range<usize> {
        let start = self.page.saturating_mul(PAGE_SIZE).min(self.lines.len());
        start..(start + PAGE_SIZE).min(self.lines.len())
    }

    fn set_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.page = page.min(self.page_count().saturating_sub(1));
        self.bodies.clear();
        self.load_page(cx);
        cx.notify();
    }

    fn load_page(&mut self, cx: &mut Context<Self>) {
        let pending = self
            .page_range()
            .map(|index| (index, self.lines[index].clone()))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return;
        }
        self.loading = true;
        let load = cx.background_spawn(async move {
            smol::unblock(move || {
                pending
                    .into_iter()
                    .map(|(index, line)| {
                        (
                            index,
                            load_process_line(&line, MAX_INLINE_PROCESS_BYTES)
                                .map_err(|error| format!("{error:#}")),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .await
        });
        cx.spawn(async move |weak, cx| {
            let loaded = load.await;
            let _ = weak.update(cx, |view, cx| {
                view.loading = false;
                for (index, body) in loaded {
                    if let Ok(body) = body {
                        view.bodies.insert(index, body);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_line(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let line = &self.lines[index];
        let raw_line = line.clone();
        let content = match self.bodies.get(&index) {
            Some(ProcessBody::Loaded(events)) if events.is_empty() => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No displayable process payload in this source line.")
                .into_any_element(),
            Some(ProcessBody::Loaded(events)) => v_flex()
                .gap_3()
                .children(events.iter().map(|event| render_event(event, cx)))
                .into_any_element(),
            Some(ProcessBody::Oversized { source_bytes }) => v_flex()
                .gap_1()
                .child(format!(
                    "Oversized process payload: {:.1} MiB",
                    *source_bytes as f64 / (1024. * 1024.)
                ))
                .child(
                    Button::new(format!("process-raw-{}", line.ordinal))
                        .xsmall()
                        .outline()
                        .label("View raw record")
                        .on_click(move |_, window, cx| {
                            let raw = cx.new(|cx| RawRecordView::new(raw_line.clone(), cx));
                            window.open_sheet(cx, move |sheet, _, _| {
                                sheet
                                    .title("Raw process record")
                                    .size(px(760.))
                                    .child(raw.clone())
                            });
                        }),
                )
                .into_any_element(),
            None => Skeleton::new().h_16().rounded_md().into_any_element(),
        };

        v_flex()
            .gap_2()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(div().font_medium().text_sm().child(line.event_type.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(line.timestamp.clone().unwrap_or_default()),
                    ),
            )
            .child(content)
            .into_any_element()
    }
}

impl Render for ProcessDetailsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page_count = self.page_count();
        let page = self.page;
        let range = self.page_range();
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
                                "{} indexed process lines · page {} of {}",
                                self.lines.len(),
                                page + 1,
                                page_count
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("process-previous")
                                    .xsmall()
                                    .outline()
                                    .label("Previous")
                                    .disabled(page == 0 || self.loading)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.set_page(view.page.saturating_sub(1), cx)
                                    })),
                            )
                            .child(
                                Button::new("process-next")
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
            .children(range.map(|index| self.render_line(index, cx)))
    }
}

fn render_event(event: &SessionEvent, cx: &mut Context<ProcessDetailsView>) -> AnyElement {
    let label = match event {
        SessionEvent::Reasoning { .. } => "Reasoning",
        SessionEvent::ToolCall { .. } => "Tool call",
        SessionEvent::ToolResult { .. } => "Tool result",
        SessionEvent::TurnContext { .. } => "Turn context",
        SessionEvent::TokenUsage { .. } => "Token usage",
        SessionEvent::Compaction { .. } => "Compaction",
        SessionEvent::Lifecycle { .. } => "Lifecycle",
        SessionEvent::SystemNotice { .. } => "System notice",
        SessionEvent::PermissionMode { .. } => "Permission mode",
        SessionEvent::Attachment { .. } => "Attachment",
        SessionEvent::FileSnapshot { .. } => "File snapshot",
        SessionEvent::Unknown { .. } => "Unknown",
        SessionEvent::User { .. } => "User",
        SessionEvent::AssistantMessage { .. } => "Assistant",
    };
    let detail = serde_json::to_string_pretty(event).unwrap_or_else(|_| "{}".to_string());
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .p_3()
                .rounded_md()
                .bg(cx.theme().muted.opacity(0.35))
                .text_xs()
                .line_height(px(18.))
                .child(detail),
        )
        .into_any_element()
}
