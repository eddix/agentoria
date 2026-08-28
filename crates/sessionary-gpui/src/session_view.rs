use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    rc::Rc,
};

use gpui::{
    div, list, prelude::FluentBuilder as _, px, relative, AnyElement, App, AppContext, Context,
    FollowMode, InteractiveElement, IntoElement, ListAlignment, ListOffset, ListState,
    ParentElement, Render, SharedString, Styled, Window,
};
use gpui_component::{
    button::Button, h_flex, skeleton::Skeleton, text::TextView, v_flex, ActiveTheme, IconName,
    Sizable as _, StyledExt as _, WindowExt as _,
};
use sessionary_core::index::{
    load_conversation_line, ConversationBody, ConversationKind, IndexedFile, IndexedLine,
};

use crate::preferences::ScrollAnchor;
use crate::process_view::ProcessDetailsView;
use crate::raw_view::RawRecordView;

const MAX_INLINE_SOURCE_BYTES: u64 = 512 * 1024;
const MAX_CACHED_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHED_BODIES: usize = 128;
type ScrollCallback = dyn Fn(ScrollAnchor, &mut Window, &mut App);

pub struct SessionView {
    file: IndexedFile,
    conversation_lines: Vec<IndexedLine>,
    process_lines: Vec<IndexedLine>,
    list_state: ListState,
    bodies: HashMap<usize, ConversationBody>,
    loading: HashSet<usize>,
    cache_order: VecDeque<usize>,
    cached_source_bytes: usize,
    visible_range: Range<usize>,
    render_markdown: bool,
    show_jump_to_bottom: bool,
    on_scroll: Rc<ScrollCallback>,
}

impl SessionView {
    pub fn new(
        file: IndexedFile,
        conversation_lines: Vec<IndexedLine>,
        process_lines: Vec<IndexedLine>,
        initial_anchor: Option<ScrollAnchor>,
        on_scroll: impl Fn(ScrollAnchor, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        let process_row = usize::from(!process_lines.is_empty());
        let item_count = conversation_lines.len() + process_row;
        let list_state = ListState::new(item_count, ListAlignment::Bottom, px(1200.))
            .with_uniform_item_height(px(180.));
        if let Some(anchor) = initial_anchor.filter(|anchor| !anchor.is_following_tail) {
            list_state.set_follow_mode(FollowMode::Normal);
            list_state.scroll_to(ListOffset {
                item_ix: anchor.item_index.min(item_count),
                offset_in_item: px(anchor.offset_in_item.max(0.)),
            });
        } else {
            list_state.set_follow_mode(FollowMode::Tail);
        }
        let weak = cx.weak_entity();
        let on_scroll = Rc::new(on_scroll);
        let scroll_callback = on_scroll.clone();
        list_state.set_scroll_handler(move |event, window, cx| {
            let _ = weak.update(cx, |view, cx| {
                view.visible_range = event.visible_range.clone();
                view.show_jump_to_bottom = !event.is_following_tail;
                view.load_visible_bodies(cx);
            });
            scroll_callback(
                ScrollAnchor {
                    item_index: event.visible_range.start,
                    offset_in_item: 0.,
                    is_following_tail: event.is_following_tail,
                },
                window,
                cx,
            );
        });

        let mut view = Self {
            file,
            conversation_lines,
            process_lines,
            list_state,
            bodies: HashMap::new(),
            loading: HashSet::new(),
            cache_order: VecDeque::new(),
            cached_source_bytes: 0,
            visible_range: item_count.saturating_sub(16)..item_count,
            render_markdown: true,
            show_jump_to_bottom: initial_anchor.is_some_and(|anchor| !anchor.is_following_tail),
            on_scroll,
        };
        view.load_visible_bodies(cx);
        view
    }

    fn load_visible_bodies(&mut self, cx: &mut Context<Self>) {
        let process_row = usize::from(!self.process_lines.is_empty());
        let start = self
            .visible_range
            .start
            .saturating_sub(process_row)
            .saturating_sub(6);
        let end = self
            .visible_range
            .end
            .saturating_sub(process_row)
            .saturating_add(6)
            .min(self.conversation_lines.len());
        if start >= end {
            return;
        }

        let pending = (start..end)
            .filter(|index| !self.bodies.contains_key(index) && self.loading.insert(*index))
            .map(|index| (index, self.conversation_lines[index].clone()))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return;
        }

        let load = cx.background_spawn(async move {
            smol::unblock(move || {
                pending
                    .into_iter()
                    .map(|(index, line)| {
                        let body = load_conversation_line(&line, MAX_INLINE_SOURCE_BYTES)
                            .map_err(|error| format!("{error:#}"));
                        (index, line.byte_len, body)
                    })
                    .collect::<Vec<_>>()
            })
            .await
        });
        cx.spawn(async move |weak, cx| {
            let loaded = load.await;
            let _ = weak.update(cx, |view, cx| {
                let mut changed = Vec::new();
                for (index, source_bytes, result) in loaded {
                    view.loading.remove(&index);
                    if let Ok(Some(body)) = result {
                        view.cached_source_bytes = view
                            .cached_source_bytes
                            .saturating_add(source_bytes as usize);
                        view.bodies.insert(index, body);
                        view.cache_order.push_back(index);
                        changed.push(index);
                    }
                }
                view.evict_cache();
                for index in changed {
                    view.list_state.remeasure_items(
                        view.list_index(index)..view.list_index(index).saturating_add(1),
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn evict_cache(&mut self) {
        let process_row = usize::from(!self.process_lines.is_empty());
        let visible = self.visible_range.start.saturating_sub(process_row)
            ..self.visible_range.end.saturating_sub(process_row);
        let mut attempts = self.cache_order.len();
        while (self.bodies.len() > MAX_CACHED_BODIES
            || self.cached_source_bytes > MAX_CACHED_SOURCE_BYTES)
            && attempts > 0
        {
            attempts -= 1;
            let Some(index) = self.cache_order.pop_front() else {
                break;
            };
            if visible.contains(&index) {
                self.cache_order.push_back(index);
                continue;
            }
            if let Some(body) = self.bodies.remove(&index) {
                self.cached_source_bytes = self
                    .cached_source_bytes
                    .saturating_sub(body_source_bytes(&body));
            }
        }
    }

    fn list_index(&self, conversation_index: usize) -> usize {
        conversation_index + usize::from(!self.process_lines.is_empty())
    }

    fn toggle_markdown(&mut self, cx: &mut Context<Self>) {
        self.render_markdown = !self.render_markdown;
        self.list_state.remeasure();
        cx.notify();
    }

    fn jump_to_bottom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list_state.set_follow_mode(FollowMode::Tail);
        self.list_state.scroll_to_end();
        self.show_jump_to_bottom = false;
        let offset = self.list_state.logical_scroll_top();
        (self.on_scroll)(
            ScrollAnchor {
                item_index: offset.item_ix,
                offset_in_item: offset.offset_in_item.as_f32(),
                is_following_tail: true,
            },
            window,
            cx,
        );
        cx.notify();
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let markdown_label = if self.render_markdown {
            "Markdown"
        } else {
            "Raw"
        };
        v_flex()
            .flex_shrink_0()
            .gap_2()
            .px_5()
            .py_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("TRANSCRIPT"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xl()
                                    .font_semibold()
                                    .child(session_title(&self.file)),
                            ),
                    )
                    .child(
                        Button::new("toggle-markdown")
                            .small()
                            .outline()
                            .icon(IconName::File)
                            .label(markdown_label)
                            .on_click(cx.listener(|view, _, _, cx| view.toggle_markdown(cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} user · {} assistant · {} process",
                        self.file.user_count, self.file.assistant_count, self.file.process_count
                    ))
                    .child(
                        self.file
                            .working_dir
                            .as_deref()
                            .map(std::path::Path::new)
                            .map(format_path)
                            .unwrap_or_else(|| format_path(&self.file.path)),
                    ),
            )
    }

    fn render_item(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.process_lines.is_empty() && index == 0 {
            return self.render_process_summary(window, cx).into_any_element();
        }
        let conversation_index = index - usize::from(!self.process_lines.is_empty());
        let Some(line) = self.conversation_lines.get(conversation_index) else {
            return div().into_any_element();
        };
        let kind = line
            .conversation_kind
            .unwrap_or(ConversationKind::Assistant);
        let accent = match kind {
            ConversationKind::User => cx.theme().primary,
            ConversationKind::Assistant => cx.theme().border,
        };
        let role = match kind {
            ConversationKind::User => "USER",
            ConversationKind::Assistant => "ASSISTANT",
        };
        let raw_line = line.clone();
        let content = match self.bodies.get(&conversation_index) {
            Some(ConversationBody::Loaded(message)) if self.render_markdown => TextView::markdown(
                SharedString::from(format!("message-{}-{}", self.file.id, line.ordinal)),
                message.text.clone(),
            )
            .selectable(true)
            .w_full()
            .into_any_element(),
            Some(ConversationBody::Loaded(message)) => div()
                .text_sm()
                .line_height(px(22.))
                .child(message.text.clone())
                .into_any_element(),
            Some(ConversationBody::Oversized { source_bytes, .. }) => v_flex()
                .gap_2()
                .child(format!(
                    "This message is {} and is kept out of inline Markdown rendering.",
                    format_bytes(*source_bytes)
                ))
                .child(
                    Button::new(SharedString::from(format!(
                        "open-raw-{}-{}",
                        self.file.id, line.ordinal
                    )))
                    .small()
                    .outline()
                    .label("View raw record")
                    .on_click(move |_, window, cx| {
                        let raw = cx.new(|cx| RawRecordView::new(raw_line.clone(), cx));
                        window.open_sheet(cx, move |sheet, _, _| {
                            sheet
                                .title("Raw session record")
                                .size(px(760.))
                                .child(raw.clone())
                        });
                    }),
                )
                .into_any_element(),
            None => Skeleton::new().h_16().rounded_md().into_any_element(),
        };

        v_flex()
            .id(SharedString::from(format!(
                "message-row-{}-{}",
                self.file.id, line.ordinal
            )))
            .w_full()
            .gap_2()
            .px_5()
            .py_4()
            .border_b_1()
            .border_l_2()
            .border_color(cx.theme().border)
            .when(kind == ConversationKind::User, |this| {
                this.bg(accent.opacity(0.025))
            })
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(role)
                    .child(line.timestamp.clone().unwrap_or_default()),
            )
            .child(content)
            .into_any_element()
    }

    fn render_process_summary(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let lines = self.process_lines.clone();
        h_flex()
            .min_h(px(72.))
            .justify_between()
            .gap_4()
            .px_5()
            .py_3()
            .border_b_1()
            .border_l_2()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.2))
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(format!(
                        "{} process events aggregated",
                        self.file.process_count
                    ))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Reasoning, tools, token usage, context and system events"),
                    ),
            )
            .child(
                Button::new("open-process-details")
                    .small()
                    .outline()
                    .icon(IconName::PanelRightOpen)
                    .label("View details")
                    .on_click(move |_, window, cx| {
                        let details = cx.new(|cx| ProcessDetailsView::new(lines.clone(), cx));
                        window.open_sheet(cx, move |sheet, _, _| {
                            sheet
                                .title("Process details")
                                .size(px(680.))
                                .child(details.clone())
                        });
                    }),
            )
    }
}

impl Render for SessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_h_0()
            .child(self.render_header(cx))
            .child(
                div()
                    .relative()
                    .min_h_0()
                    .flex_1()
                    .child(
                        list(
                            self.list_state.clone(),
                            cx.processor(|view, index: usize, window, cx| {
                                view.render_item(index, window, cx)
                            }),
                        )
                        .size_full(),
                    )
                    .when(self.show_jump_to_bottom, |this| {
                        this.child(
                            Button::new("jump-to-bottom")
                                .absolute()
                                .bottom_4()
                                .left(relative(0.5))
                                .ml(px(-16.))
                                .rounded_full()
                                .outline()
                                .icon(IconName::ArrowDown)
                                .tooltip("Jump to session bottom")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.jump_to_bottom(window, cx)
                                })),
                        )
                    }),
            )
            .when(!window.is_window_active(), |this| this)
    }
}

fn body_source_bytes(body: &ConversationBody) -> usize {
    match body {
        ConversationBody::Loaded(message) => message.source_bytes as usize,
        ConversationBody::Oversized { source_bytes, .. } => *source_bytes as usize,
    }
}

fn session_title(file: &IndexedFile) -> String {
    file.title
        .clone()
        .or_else(|| file.session_id.clone())
        .or_else(|| {
            file.path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "Session".to_string())
}

fn format_path(path: &std::path::Path) -> String {
    let display = path.to_string_lossy();
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy();
        if let Some(rest) = display.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    display.into_owned()
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024. * 1024.))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet, VecDeque},
        path::PathBuf,
        rc::Rc,
        time::Instant,
    };

    use gpui::{
        point, px, AppContext as _, FollowMode, ListAlignment, ListState, ScrollDelta,
        ScrollWheelEvent, TestAppContext, VisualTestContext,
    };
    use sessionary_core::{
        index::{
            ConversationBody, ConversationKind, ConversationMessage, IndexedFile, IndexedLine,
        },
        SessionSource,
    };

    use super::SessionView;

    #[gpui::test]
    fn session_view_starts_at_tail_and_toggles_markdown(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let file = test_file(0);
        let view = cx.update(|cx| {
            cx.new(|cx| SessionView::new(file, Vec::new(), Vec::new(), None, |_, _, _| {}, cx))
        });

        view.read_with(cx, |view, _| {
            assert!(view.render_markdown);
            assert!(!view.show_jump_to_bottom);
        });
        view.update(cx, |view, cx| view.toggle_markdown(cx));
        view.read_with(cx, |view, _| assert!(!view.render_markdown));
    }

    #[gpui::test]
    fn transcript_scroll_p95_stays_within_one_frame(cx: &mut TestAppContext) {
        const ROWS: usize = 1_000;
        const SAMPLES: usize = 40;

        cx.update(gpui_component::init);
        let file = test_file(ROWS);
        let lines = (0..ROWS)
            .map(|index| IndexedLine {
                file_id: file.id,
                source: SessionSource::Codex,
                path: PathBuf::from("/tmp/performance.jsonl"),
                ordinal: index as u64,
                line_number: index as u64 + 1,
                byte_offset: 0,
                byte_len: 2_048,
                event_type: "event_msg.agent_message".to_string(),
                timestamp: Some("2026-08-28T01:00:00.000Z".to_string()),
                conversation_kind: Some(ConversationKind::Assistant),
                process_count: 0,
            })
            .collect::<Vec<_>>();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| {
                let list_state = ListState::new(ROWS, ListAlignment::Bottom, px(1200.))
                    .with_uniform_item_height(px(180.));
                list_state.set_follow_mode(FollowMode::Tail);
                let mut view = SessionView {
                    file,
                    conversation_lines: lines,
                    process_lines: Vec::new(),
                    list_state,
                    bodies: HashMap::new(),
                    loading: HashSet::new(),
                    cache_order: VecDeque::new(),
                    cached_source_bytes: 0,
                    visible_range: ROWS.saturating_sub(16)..ROWS,
                    render_markdown: true,
                    show_jump_to_bottom: false,
                    on_scroll: Rc::new(|_, _, _| {}),
                };
                let markdown =
                    "## Result\n\n- first item\n- second item\n\n```rust\nfn main() {}\n```";
                for index in 0..ROWS {
                    view.bodies.insert(
                        index,
                        ConversationBody::Loaded(ConversationMessage {
                            kind: ConversationKind::Assistant,
                            text: markdown.to_string(),
                            model: Some("test".to_string()),
                            stop_reason: None,
                            source_bytes: markdown.len() as u64,
                        }),
                    );
                }
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let cx: &mut VisualTestContext = cx;
        draw(cx);

        for _ in 0..8 {
            scroll_and_draw(cx);
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = Instant::now();
            scroll_and_draw(cx);
            samples.push(started.elapsed().as_secs_f64() * 1000.);
        }
        samples.sort_by(f64::total_cmp);
        let p95 = samples[((SAMPLES as f64 * 0.95).ceil() as usize).saturating_sub(1)];
        let max = *samples.last().unwrap_or(&0.);
        eprintln!("session transcript scroll p95={p95:.2}ms max={max:.2}ms");
        if !cfg!(debug_assertions) {
            assert!(p95 <= 16., "scroll p95 {p95:.2}ms exceeds 16ms");
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn scroll_and_draw(cx: &mut VisualTestContext) {
        cx.simulate_event(ScrollWheelEvent {
            position: point(px(600.), px(400.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(180.))),
            ..Default::default()
        });
        draw(cx);
    }

    fn test_file(assistant_count: usize) -> IndexedFile {
        IndexedFile {
            id: 1,
            source: SessionSource::Codex,
            path: PathBuf::from("/tmp/session.jsonl"),
            file_size: 0,
            scanned_offset: 0,
            pending_bytes: 0,
            line_count: 0,
            user_count: 0,
            assistant_count: assistant_count as u64,
            process_count: 0,
            error_count: 0,
            started_at: None,
            updated_at: String::new(),
            session_id: Some("session".to_string()),
            title: Some("Session".to_string()),
            working_dir: Some("/tmp".to_string()),
            git_branch: None,
            model: None,
            parent_session_id: None,
            is_sidechain: false,
        }
    }
}
