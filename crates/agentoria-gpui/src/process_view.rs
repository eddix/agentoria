use crate::ui;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use agentoria_core::{
    index::{load_process_line, IndexedLine, ProcessBody},
    SessionEvent,
};
use gpui::{
    div, px, AnyElement, AppContext, ClipboardItem, Context, IntoElement, ParentElement, Render,
    SharedString, Styled, Task, Window,
};
use gpui_kit::component::{
    button::ButtonVariants as _, h_flex, skeleton::Skeleton, v_flex, ActiveTheme, Disableable as _,
    IconName, Sizable as _, WindowExt as _,
};

use crate::raw_view::RawRecordView;

const PAGE_SIZE: usize = 50;
const MAX_INLINE_PROCESS_BYTES: u64 = 512 * 1024;

pub struct ProcessDetailsView {
    lines: Arc<[IndexedLine]>,
    page: usize,
    bodies: HashMap<usize, ProcessBody>,
    errors: HashMap<usize, String>,
    loading: bool,
    revision: u64,
    load_task: Option<Task<()>>,
    expanded: HashSet<(usize, usize)>,
    formatted: HashMap<(usize, usize), SharedString>,
}

impl ProcessDetailsView {
    pub fn new(lines: Arc<[IndexedLine]>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            lines,
            page: 0,
            bodies: HashMap::new(),
            errors: HashMap::new(),
            loading: false,
            revision: 0,
            load_task: None,
            expanded: HashSet::new(),
            formatted: HashMap::new(),
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
        self.errors.clear();
        self.expanded.clear();
        self.formatted.clear();
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
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let load = cx.background_spawn(async move {
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
        });
        self.load_task = Some(cx.spawn(async move |weak, cx| {
            let loaded = load.await;
            let _ = weak.update(cx, |view, cx| {
                if view.revision != revision {
                    return;
                }
                view.loading = false;
                for (index, body) in loaded {
                    match body {
                        Ok(body) => {
                            view.bodies.insert(index, body);
                        }
                        Err(error) => {
                            view.errors
                                .insert(index, format!("Unable to read record: {error}"));
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    fn toggle_event(&mut self, index: usize, event_index: usize, cx: &mut Context<Self>) {
        let key = (index, event_index);
        if !self.expanded.remove(&key) {
            let Some(ProcessBody::Loaded(events)) = self.bodies.get(&index) else {
                return;
            };
            let Some(event) = events.get(event_index) else {
                return;
            };
            let detail = serde_json::to_string_pretty(event)
                .unwrap_or_else(|error| format!("Unable to format event: {error}"));
            self.formatted.insert(key, detail.into());
            self.expanded.insert(key);
        } else {
            self.formatted.remove(&key);
        }
        cx.notify();
    }

    fn render_event(
        &self,
        index: usize,
        event_index: usize,
        event: &SessionEvent,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (index, event_index);
        let expanded = self.expanded.contains(&key);
        let (label, summary, error) = event_summary(event);
        let ink = if error {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        let mut row = v_flex().w_full().gap_2().py(px(6.));
        row = row.child(
            ui::button(format!("process-event-{index}-{event_index}"))
                .ghost()
                .w_full()
                .h(px(36.))
                .justify_start()
                .icon(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .label(format!("{label} · {summary}"))
                .text_color(ink)
                .on_click(
                    cx.listener(move |view, _, _, cx| view.toggle_event(index, event_index, cx)),
                ),
        );
        if let Some(detail) = self.formatted.get(&key) {
            let copy = detail.clone();
            row = row.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .rounded(px(12.))
                    .bg(cx.theme().sidebar)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .justify_between()
                            .px_3()
                            .py_1()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(ui::caption("Event JSON", cx))
                            .child(
                                ui::icon_button(
                                    format!("copy-event-{index}-{event_index}"),
                                    IconName::Copy,
                                    "Copy event JSON",
                                )
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy.to_string(),
                                    ))
                                }),
                            ),
                    )
                    .child(
                        div()
                            .p_3()
                            .text_size(px(12.))
                            .line_height(px(20.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(detail.clone()),
                    ),
            );
        }
        row.into_any_element()
    }

    fn render_line(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let line = &self.lines[index];
        let raw_line = line.clone();
        let content =
            match self.bodies.get(&index) {
                Some(ProcessBody::Loaded(events)) if events.is_empty() => div()
                    .text_size(px(ui::TEXT))
                    .text_color(cx.theme().muted_foreground)
                    .child("No displayable process payload in this source line.")
                    .into_any_element(),
                Some(ProcessBody::Loaded(events)) => v_flex()
                    .gap_3()
                    .children(events.iter().enumerate().map(|(event_index, event)| {
                        self.render_event(index, event_index, event, cx)
                    }))
                    .into_any_element(),
                Some(ProcessBody::Oversized { source_bytes }) => v_flex()
                    .gap_1()
                    .child(format!(
                        "Oversized process payload: {:.1} MiB",
                        *source_bytes as f64 / (1024. * 1024.)
                    ))
                    .child(
                        ui::button(format!("process-raw-{}", line.ordinal))
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
                None => {
                    if let Some(error) = self.errors.get(&index) {
                        div()
                            .text_size(px(ui::TEXT))
                            .text_color(cx.theme().danger)
                            .child(error.clone())
                            .into_any_element()
                    } else {
                        Skeleton::new().h_8().rounded_md().into_any_element()
                    }
                }
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
                    .child(ui::caption(line.event_type.clone(), cx))
                    .child(
                        div()
                            .text_size(px(ui::META))
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
                            .text_size(px(ui::TEXT))
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
                                ui::button("process-previous")
                                    .xsmall()
                                    .outline()
                                    .label("Previous")
                                    .disabled(page == 0 || self.loading)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.set_page(view.page.saturating_sub(1), cx)
                                    })),
                            )
                            .child(
                                ui::button("process-next")
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

fn event_summary(event: &SessionEvent) -> (&'static str, String, bool) {
    let (label, text, error) = match event {
        SessionEvent::ToolCall { name, summary, .. } => {
            ("Tool call", summary.as_deref().unwrap_or(name), false)
        }
        SessionEvent::ToolResult {
            content, is_error, ..
        } => ("Tool result", content.as_str(), *is_error),
        SessionEvent::Reasoning { text, .. } => ("Reasoning", text.as_str(), false),
        SessionEvent::SystemNotice { subtype, .. } => {
            ("System", subtype.as_deref().unwrap_or("Context"), false)
        }
        SessionEvent::TurnContext { summary, .. } => (
            "Context",
            summary.as_deref().unwrap_or("Turn settings"),
            false,
        ),
        SessionEvent::TokenUsage { .. } => ("Usage", "Token usage", false),
        SessionEvent::Compaction { summary, .. } => (
            "Compaction",
            summary.as_deref().unwrap_or("Context compacted"),
            false,
        ),
        SessionEvent::Lifecycle { event, .. } => ("Lifecycle", event.as_str(), false),
        SessionEvent::PermissionMode { mode, .. } => ("Permissions", mode.as_str(), false),
        SessionEvent::Attachment { name, .. } => (
            "Attachment",
            name.as_deref().unwrap_or("File attachment"),
            false,
        ),
        SessionEvent::FileSnapshot { .. } => ("Snapshot", "File snapshot", false),
        SessionEvent::Unknown { .. } => ("Event", "Source record", false),
        SessionEvent::User { text, .. } => ("User", text.as_str(), false),
        SessionEvent::AssistantMessage { text, .. } => ("Assistant", text.as_str(), false),
    };
    let mut summary = text
        .trim_start()
        .chars()
        .take(111)
        .map(|character| {
            if character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if summary.chars().count() > 110 {
        summary = summary.chars().take(110).collect::<String>() + "…";
    }
    if summary.is_empty() {
        summary = "Empty result".into();
    }
    (label, summary, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentoria_core::EventMeta;
    use gpui::TestAppContext;

    #[gpui::test]
    fn process_disclosure_formats_only_open_events_and_discards_collapsed_json(
        cx: &mut TestAppContext,
    ) {
        let view = cx.update(|cx| cx.new(|cx| ProcessDetailsView::new(Vec::new().into(), cx)));
        view.update(cx, |view, cx| {
            view.bodies.insert(
                0,
                ProcessBody::Loaded(vec![SessionEvent::ToolCall {
                    meta: EventMeta::new("tool", "fixture"),
                    call_id: None,
                    name: "Read".into(),
                    input: serde_json::json!({"file_path":"/tmp/example"}),
                    summary: None,
                }]),
            );
            assert!(view.formatted.is_empty());
            view.toggle_event(0, 0, cx);
            assert!(view.expanded.contains(&(0, 0)));
            assert!(view.formatted[&(0, 0)].contains("Read"));
            view.toggle_event(0, 0, cx);
            assert!(view.expanded.is_empty() && view.formatted.is_empty());
        });
    }

    #[test]
    fn process_summary_is_bounded_for_large_unicode_and_preserves_error_status() {
        let event = SessionEvent::ToolResult {
            meta: EventMeta::new("result", "fixture"),
            call_id: None,
            content: "内容".repeat(100_000),
            is_error: true,
        };
        let (label, summary, error) = event_summary(&event);
        assert_eq!(label, "Tool result");
        assert!(error && summary.ends_with('…'));
        assert_eq!(summary.chars().count(), 111);
    }
}
