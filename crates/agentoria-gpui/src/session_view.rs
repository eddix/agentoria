use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use agentoria_core::index::{
    load_conversation_line, search::search_conversations, ConversationBody, ConversationKind,
    IndexedFile, IndexedLine,
};
use gpui::{
    div, list, prelude::FluentBuilder as _, px, relative, AnyElement, App, AppContext, Context,
    Focusable as _, FollowMode, InteractiveElement, IntoElement, ListAlignment, ListOffset,
    ListState, ParentElement, Render, SharedString, Styled, StyledText, Task, Window,
};
use gpui_kit::base::text::{RangeHighlight, TextViewState};
use gpui_kit::component::{
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    skeleton::Skeleton,
    text::{TextView, TextViewStyle},
    v_flex, ActiveTheme, Disableable as _, IconName, Sizable as _, StyledExt as _, WindowExt as _,
};

use crate::preferences::ScrollAnchor;
use crate::process_view::ProcessDetailsView;
use crate::raw_view::RawRecordView;
use crate::session_search::{MatchHighlight, SessionSearch, MAX_SEARCH_MATCHES};

const MAX_INLINE_SOURCE_BYTES: u64 = 512 * 1024;
const MAX_CACHED_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHED_BODIES: usize = 128;
const MAX_LOAD_BATCH: usize = 16;
const PREFETCH_ROWS: usize = 6;
type ScrollCallback = dyn Fn(ScrollAnchor, &mut Window, &mut App);

enum CachedBody {
    Loaded(SharedString),
    Oversized { source_bytes: u64 },
    Unavailable(SharedString),
}

impl CachedBody {
    fn from_result(result: Result<Option<ConversationBody>, String>) -> Self {
        match result {
            Ok(Some(ConversationBody::Loaded(message))) => Self::Loaded(message.text.into()),
            Ok(Some(ConversationBody::Oversized { source_bytes, .. })) => {
                Self::Oversized { source_bytes }
            }
            Ok(None) => {
                Self::Unavailable("No conversation text remains in this source record.".into())
            }
            Err(error) => Self::Unavailable(error.into()),
        }
    }

    fn text_bytes(&self) -> usize {
        match self {
            Self::Loaded(text) | Self::Unavailable(text) => text.len(),
            Self::Oversized { .. } => 0,
        }
    }
}

pub struct SessionView {
    file: IndexedFile,
    conversation_lines: Arc<[IndexedLine]>,
    process_lines: Arc<[IndexedLine]>,
    list_state: ListState,
    bodies: HashMap<usize, CachedBody>,
    loading: HashSet<usize>,
    load_task: Option<Task<()>>,
    cache_order: VecDeque<usize>,
    cached_text_bytes: usize,
    visible_range: Range<usize>,
    rendered_range: Option<Range<usize>>,
    render_markdown: bool,
    show_jump_to_bottom: bool,
    on_scroll: Rc<ScrollCallback>,
    search: SessionSearch,
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
        list_state.set_scroll_handler(move |_, window, cx| {
            let weak = weak.clone();
            let scroll_callback = scroll_callback.clone();
            // ListState is mutably borrowed during this callback. Read the exact
            // new offset only after GPUI finishes handling the scroll event.
            window.defer(cx, move |window, cx| {
                if let Ok(anchor) = weak.update(cx, |view, cx| {
                    let offset = view.list_state.logical_scroll_top();
                    let is_following_tail = view.list_state.is_following_tail();
                    view.show_jump_to_bottom = !is_following_tail;
                    cx.notify();
                    ScrollAnchor {
                        item_index: offset.item_ix,
                        offset_in_item: offset.offset_in_item.as_f32(),
                        is_following_tail,
                    }
                }) {
                    scroll_callback(anchor, window, cx);
                }
            });
        });

        let first_item = initial_anchor
            .filter(|anchor| !anchor.is_following_tail)
            .map(|anchor| anchor.item_index.min(item_count.saturating_sub(1)))
            .unwrap_or_else(|| item_count.saturating_sub(16));

        let mut view = Self {
            file,
            conversation_lines: conversation_lines.into(),
            process_lines: process_lines.into(),
            list_state,
            bodies: HashMap::new(),
            loading: HashSet::new(),
            load_task: None,
            cache_order: VecDeque::new(),
            cached_text_bytes: 0,
            visible_range: first_item..first_item.saturating_add(16).min(item_count),
            rendered_range: None,
            render_markdown: true,
            show_jump_to_bottom: initial_anchor.is_some_and(|anchor| !anchor.is_following_tail),
            on_scroll,
            search: SessionSearch::default(),
        };
        view.load_visible_bodies(cx);
        view
    }

    fn load_visible_bodies(&mut self, cx: &mut Context<Self>) {
        // One bounded batch at a time; scrolling changes the next batch's range
        // instead of queuing detached work for every intermediate viewport.
        if !self.loading.is_empty() {
            return;
        }
        let process_row = usize::from(!self.process_lines.is_empty());
        let visible_start = self
            .visible_range
            .start
            .saturating_sub(process_row)
            .min(self.conversation_lines.len());
        let visible_end = self
            .visible_range
            .end
            .saturating_sub(process_row)
            .min(self.conversation_lines.len());
        let range = self.prefetch_range();
        if range.is_empty() {
            return;
        }

        let pending = (visible_start..visible_end)
            .chain(range.start..visible_start)
            .chain(visible_end..range.end)
            .filter(|index| !self.bodies.contains_key(index))
            .take(MAX_LOAD_BATCH)
            .map(|index| (index, self.conversation_lines[index].clone()))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return;
        }
        self.loading.extend(pending.iter().map(|(index, _)| *index));

        let load = cx.background_spawn(async move {
            pending
                .into_iter()
                .map(|(index, line)| {
                    let body = load_conversation_line(&line, MAX_INLINE_SOURCE_BYTES)
                        .map_err(|error| format!("{error:#}"));
                    (index, CachedBody::from_result(body))
                })
                .collect::<Vec<_>>()
        });
        self.load_task = Some(cx.spawn(async move |weak, cx| {
            let loaded = load.await;
            let _ = weak.update(cx, |view, cx| {
                let mut changed = Vec::new();
                view.loading.clear();
                let wanted = view.prefetch_range();
                for (index, body) in loaded {
                    if wanted.contains(&index) {
                        view.insert_body(index, body);
                        changed.push(index);
                    }
                }
                view.evict_cache();
                for index in changed {
                    view.list_state.remeasure_items(
                        view.list_index(index)..view.list_index(index).saturating_add(1),
                    );
                }
                view.load_visible_bodies(cx);
                cx.notify();
            });
        }));
    }

    fn prefetch_range(&self) -> Range<usize> {
        let process_row = usize::from(!self.process_lines.is_empty());
        let count = self.conversation_lines.len();
        self.visible_range
            .start
            .saturating_sub(process_row)
            .saturating_sub(PREFETCH_ROWS)
            .min(count)
            ..self
                .visible_range
                .end
                .saturating_sub(process_row)
                .saturating_add(PREFETCH_ROWS)
                .min(count)
    }

    fn insert_body(&mut self, index: usize, body: CachedBody) {
        self.cached_text_bytes += body.text_bytes();
        if let Some(previous) = self.bodies.insert(index, body) {
            self.cached_text_bytes -= previous.text_bytes();
        }
        self.touch_body(index);
    }

    fn touch_body(&mut self, index: usize) {
        self.cache_order.retain(|cached| *cached != index);
        self.cache_order.push_back(index);
    }

    fn retry_body(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(body) = self.bodies.remove(&index) {
            self.cached_text_bytes -= body.text_bytes();
            self.cache_order.retain(|cached| *cached != index);
        }
        self.load_visible_bodies(cx);
        self.list_state
            .remeasure_items(self.list_index(index)..self.list_index(index) + 1);
        cx.notify();
    }

    fn evict_cache(&mut self) {
        let process_row = usize::from(!self.process_lines.is_empty());
        let visible = self.visible_range.start.saturating_sub(process_row)
            ..self.visible_range.end.saturating_sub(process_row);
        let mut attempts = self.cache_order.len();
        while (self.bodies.len() > MAX_CACHED_BODIES
            || self.cached_text_bytes > MAX_CACHED_TEXT_BYTES)
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
                self.cached_text_bytes -= body.text_bytes();
            }
        }
    }

    fn list_index(&self, conversation_index: usize) -> usize {
        conversation_index + usize::from(!self.process_lines.is_empty())
    }

    fn toggle_markdown(&mut self, cx: &mut Context<Self>) {
        self.render_markdown = !self.render_markdown;
        self.search.highlight = None;
        self.list_state.remeasure();
        cx.notify();
    }

    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.input.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Find in conversation"));
            self.search.subscription =
                Some(cx.subscribe(&input, |view, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        view.set_search_query(input.read(cx).value().to_string(), cx);
                    }
                }));
            self.search.input = Some(input);
        }
        if !self.search.open {
            self.search.previous_focus = window.focused(cx);
        }
        self.search.open = true;
        if let Some(input) = &self.search.input {
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.open = false;
        self.search.reset(String::new());
        if let Some(input) = &self.search.input {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        if let Some(focus) = self.search.previous_focus.take() {
            focus.focus(window, cx);
        } else {
            window.blur(cx);
        }
        cx.notify();
    }

    pub fn handle_search_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = event.keystroke.modifiers;
        let key = &event.keystroke.key;
        let command = modifiers.platform && !modifiers.control && !modifiers.alt;
        if command && !modifiers.shift && key.eq_ignore_ascii_case("f") {
            self.focus_search(window, cx);
        } else if self.search.open && command && key.eq_ignore_ascii_case("g") {
            self.step_search_match(modifiers.shift, cx);
        } else if self.search.open && !modifiers.modified() && key.eq_ignore_ascii_case("escape") {
            self.close_search(window, cx);
        } else if self.search.open
            && !modifiers.platform
            && !modifiers.control
            && !modifiers.alt
            && key.eq_ignore_ascii_case("enter")
            && self
                .search
                .input
                .as_ref()
                .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
        {
            self.step_search_match(modifiers.shift, cx);
        } else {
            return false;
        }
        true
    }

    fn set_search_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query == self.search.query {
            return;
        }
        self.search.reset(query);
        if self.search.searching {
            let revision = self.search.revision;
            let cancellation = self.search.cancellation.clone();
            let query = self.search.query.clone();
            let lines = self.conversation_lines.clone();
            let timer = cx.background_executor().timer(Duration::from_millis(200));
            self.search.task = Some(cx.spawn(async move |weak, cx| {
                timer.await;
                let results = cx
                    .background_spawn(async move {
                        search_conversations(
                            &lines,
                            &query,
                            MAX_INLINE_SOURCE_BYTES,
                            MAX_SEARCH_MATCHES,
                            &cancellation,
                        )
                    })
                    .await;
                let _ = weak.update(cx, |view, cx| {
                    if view.search.revision != revision {
                        return;
                    }
                    view.search.searching = false;
                    if let Some(results) = results {
                        view.search.results = results;
                        if !view.search.results.matches.is_empty() {
                            view.select_search_match(0, cx);
                        }
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    fn step_search_match(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let count = self.search.results.matches.len();
        if count == 0 || self.search.searching {
            return;
        }
        let current = self.search.active.unwrap_or(0);
        let index = if backwards {
            (current + count - 1) % count
        } else {
            (current + 1) % count
        };
        self.select_search_match(index, cx);
    }

    fn select_search_match(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(found) = self.search.results.matches.get(index) else {
            return;
        };
        let conversation_index = found.conversation_index;
        let item_index = self.list_index(found.conversation_index);
        self.search.active = Some(index);
        if let Some(highlight) = self
            .search
            .highlight
            .as_mut()
            .filter(|highlight| highlight.conversation_index == conversation_index)
        {
            highlight.stamp = None;
        } else {
            self.search.highlight = None;
        }
        self.list_state.set_follow_mode(FollowMode::Normal);
        self.list_state.scroll_to(ListOffset {
            item_ix: item_index,
            offset_in_item: px(0.),
        });
        self.visible_range = item_index..item_index + 1;
        self.show_jump_to_bottom = true;
        self.load_visible_bodies(cx);
        cx.notify();
    }

    fn highlighted_message(
        &mut self,
        index: usize,
        text: SharedString,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Entity<TextViewState>> {
        let found = self
            .search
            .active
            .and_then(|active| self.search.results.matches.get(active))?
            .clone();
        if found.conversation_index != index {
            return None;
        }
        if self
            .search
            .highlight
            .as_ref()
            .is_none_or(|highlight| highlight.conversation_index != index)
        {
            let view = cx.new(|cx| TextViewState::markdown(text.as_ref(), cx));
            let subscription = cx.observe(&view, |_, _, cx| cx.notify());
            self.search.highlight = Some(MatchHighlight {
                conversation_index: index,
                view,
                _subscription: subscription,
                stamp: None,
            });
        }
        let color = cx.theme().yellow.opacity(0.4);
        let highlight = self.search.highlight.as_mut()?;
        let snapshot = highlight.view.read(cx).rendered_text();
        if snapshot.source() == text.as_ref() {
            let stamp = (snapshot.clone(), found.range.clone(), color);
            if highlight.stamp.as_ref() != Some(&stamp) {
                if let Some(range) = snapshot.range_for_source(found.range) {
                    let reveal = highlight.stamp.is_none();
                    highlight.view.update(cx, |state, cx| {
                        if let Err(error) = state
                            .set_range_highlights([RangeHighlight::new(range.clone(), color)], cx)
                        {
                            eprintln!("failed to highlight session search match: {error:?}");
                        }
                        if reveal {
                            if let Err(error) = state.reveal_range(range, cx) {
                                eprintln!("failed to reveal session search match: {error:?}");
                            }
                        }
                    });
                }
                highlight.stamp = Some(stamp);
            }
        }
        Some(highlight.view.clone())
    }

    fn render_search_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(input) = &self.search.input else {
            return div().into_any_element();
        };
        let disabled = self.search.searching || self.search.results.matches.is_empty();
        v_flex()
            .flex_shrink_0()
            .gap_1()
            .px_5()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Input::new(input)
                            .id("session-search")
                            .aria_label("Find in conversation")
                            .cleanable(true)
                            .small()
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(
                        Button::new("session-search-previous")
                            .small()
                            .outline()
                            .icon(IconName::ChevronUp)
                            .tooltip("Previous match (Shift+Enter)")
                            .disabled(disabled)
                            .on_click(
                                cx.listener(|view, _, _, cx| view.step_search_match(true, cx)),
                            ),
                    )
                    .child(
                        Button::new("session-search-next")
                            .small()
                            .outline()
                            .icon(IconName::ChevronDown)
                            .tooltip("Next match (Enter)")
                            .disabled(disabled)
                            .on_click(
                                cx.listener(|view, _, _, cx| view.step_search_match(false, cx)),
                            ),
                    )
                    .child(
                        Button::new("session-search-close")
                            .small()
                            .outline()
                            .icon(IconName::Close)
                            .tooltip("Close search (Esc)")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.close_search(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.search.status()),
            )
            .into_any_element()
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
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("find-in-session")
                                    .small()
                                    .outline()
                                    .icon(IconName::Search)
                                    .label("Find")
                                    .tooltip("Find in session (⌘F)")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.focus_search(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("toggle-markdown")
                                    .small()
                                    .outline()
                                    .icon(IconName::File)
                                    .label(markdown_label)
                                    .on_click(
                                        cx.listener(|view, _, _, cx| view.toggle_markdown(cx)),
                                    ),
                            ),
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
        self.rendered_range = Some(match self.rendered_range.take() {
            Some(range) => range.start.min(index)..range.end.max(index + 1),
            None => index..index + 1,
        });
        if !self.process_lines.is_empty() && index == 0 {
            return self.render_process_summary(window, cx).into_any_element();
        }
        let conversation_index = index - usize::from(!self.process_lines.is_empty());
        if self.bodies.contains_key(&conversation_index) {
            self.touch_body(conversation_index);
        }
        let active_range = self
            .search
            .active
            .and_then(|active| self.search.results.matches.get(active))
            .filter(|found| found.conversation_index == conversation_index)
            .map(|found| found.range.clone());
        let highlighted = if self.render_markdown && active_range.is_some() {
            if let Some(CachedBody::Loaded(text)) = self.bodies.get(&conversation_index) {
                self.highlighted_message(conversation_index, text.clone(), cx)
            } else {
                None
            }
        } else {
            None
        };
        let Some(line) = self.conversation_lines.get(conversation_index) else {
            return div().into_any_element();
        };
        let kind = line
            .conversation_kind
            .unwrap_or(ConversationKind::Assistant);
        let accent = match kind {
            ConversationKind::User => cx.theme().blue,
            ConversationKind::Assistant => cx.theme().green,
        };
        let role = match kind {
            ConversationKind::User => "USER",
            ConversationKind::Assistant => "ASSISTANT",
        };
        let raw_line = line.clone();
        let content = match self.bodies.get(&conversation_index) {
            Some(CachedBody::Loaded(text)) if self.render_markdown => highlighted
                .as_ref()
                .map(TextView::new)
                .unwrap_or_else(|| {
                    TextView::markdown(
                        SharedString::from(format!("message-{}-{}", self.file.id, line.ordinal)),
                        text.clone(),
                    )
                })
                .style(TextViewStyle::default().inline_code(gpui::HighlightStyle {
                    color: Some(cx.theme().foreground),
                    background_color: Some(cx.theme().muted),
                    ..Default::default()
                }))
                .selectable(true)
                .w_full()
                .into_any_element(),
            Some(CachedBody::Loaded(text)) => div()
                .text_sm()
                .line_height(px(22.))
                .child(
                    StyledText::new(text.clone()).with_highlights(
                        active_range
                            .clone()
                            .into_iter()
                            .filter(|range| text.get(range.clone()).is_some())
                            .map(|range| {
                                (
                                    range,
                                    gpui::HighlightStyle {
                                        background_color: Some(cx.theme().yellow.opacity(0.4)),
                                        ..Default::default()
                                    },
                                )
                            }),
                    ),
                )
                .into_any_element(),
            Some(CachedBody::Oversized { source_bytes }) => v_flex()
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
            Some(CachedBody::Unavailable(error)) => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("Unable to load message: {error}")),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "retry-message-{}-{}",
                        self.file.id, line.ordinal
                    )))
                    .small()
                    .outline()
                    .label("Retry")
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.retry_body(conversation_index, cx);
                    })),
                )
                .into_any_element(),
            None => Skeleton::new().h_16().rounded_md().into_any_element(),
        };

        v_flex()
            .id(SharedString::from(format!(
                "message-row-{}-{}",
                self.file.id, line.ordinal
            )))
            .relative()
            .w_full()
            .gap_2()
            .px_5()
            .py_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(active_range.is_some(), |row| {
                row.bg(cx.theme().muted.opacity(0.3))
            })
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top_4()
                    .bottom_4()
                    .w(px(2.))
                    .bg(accent),
            )
            .child(
                h_flex()
                    .debug_selector(|| "session-message-header".to_string())
                    .justify_between()
                    .text_xs()
                    .child(div().font_semibold().text_color(accent).child(role))
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(line.timestamp.clone().unwrap_or_default()),
                    ),
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
        self.rendered_range = None;
        let weak = cx.weak_entity();
        // Layout can expose rows without a scroll event (restored anchors,
        // resizing, Markdown toggles, or async row-height changes).
        window.defer(cx, move |_, cx| {
            let _ = weak.update(cx, |view, cx| {
                if let Some(range) = view.rendered_range.take() {
                    view.visible_range = range;
                    view.evict_cache();
                    view.load_visible_bodies(cx);
                }
            });
        });
        v_flex()
            .size_full()
            .min_h_0()
            .child(self.render_header(cx))
            .when(self.search.open, |view| {
                view.child(self.render_search_bar(cx))
            })
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
        cell::RefCell,
        collections::{HashMap, HashSet, VecDeque},
        fs,
        io::{BufWriter, Write as _},
        path::PathBuf,
        rc::Rc,
        time::Instant,
    };

    use agentoria_core::{
        index::{ConversationKind, IndexedFile, IndexedLine},
        SessionSource,
    };
    use gpui::{
        point, px, AppContext as _, Entity, FollowMode, ListAlignment, ListOffset, ListState,
        ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext,
    };
    #[cfg(debug_assertions)]
    use gpui::{Modifiers, MouseButton};
    use tempfile::tempdir;

    use super::{CachedBody, SessionView, MAX_CACHED_BODIES, MAX_LOAD_BATCH};

    #[gpui::test]
    fn session_view_starts_at_tail_and_toggles_markdown(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
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
    fn saved_anchor_loads_middle_without_scrolling(cx: &mut TestAppContext) {
        let view = cx.update(|cx| {
            cx.new(|cx| {
                SessionView::new(
                    test_file(1_000),
                    test_lines(1_000),
                    Vec::new(),
                    Some(crate::preferences::ScrollAnchor {
                        item_index: 500,
                        offset_in_item: 37.,
                        is_following_tail: false,
                    }),
                    |_, _, _| {},
                    cx,
                )
            })
        });
        view.read_with(cx, |view, _| {
            assert!(view.loading.contains(&500));
            assert!(!view.loading.contains(&999));
        });
    }

    #[gpui::test]
    fn rapid_scroll_keeps_pending_body_loads_bounded(cx: &mut TestAppContext) {
        let view = cx.update(|cx| {
            cx.new(|cx| {
                SessionView::new(
                    test_file(1_000),
                    test_lines(1_000),
                    Vec::new(),
                    None,
                    |_, _, _| {},
                    cx,
                )
            })
        });
        view.update(cx, |view, cx| {
            for start in (0..900).step_by(30) {
                view.visible_range = start..start + 16;
                view.load_visible_bodies(cx);
            }
            assert!(
                view.loading.len() <= MAX_LOAD_BATCH,
                "{} pending loads",
                view.loading.len()
            );
        });
    }

    #[gpui::test]
    fn oversized_records_do_not_evict_resident_text(cx: &mut TestAppContext) {
        let view = cx.update(|cx| {
            cx.new(|cx| {
                SessionView::new(test_file(0), Vec::new(), Vec::new(), None, |_, _, _| {}, cx)
            })
        });
        view.update(cx, |view, _| {
            let text = "Keep this text";
            let oversized_bytes = 96 * 1024 * 1024;
            view.insert_body(0, CachedBody::Loaded(text.into()));
            view.insert_body(
                1,
                CachedBody::Oversized {
                    source_bytes: oversized_bytes,
                },
            );
            view.visible_range = 1..2;
            view.evict_cache();
            assert!(view.bodies.contains_key(&0));
            assert_eq!(view.cached_text_bytes, text.len());
        });
    }

    #[gpui::test]
    fn cache_evicts_least_recently_used_bodies(cx: &mut TestAppContext) {
        let view = cx.update(|cx| {
            cx.new(|cx| {
                SessionView::new(test_file(0), Vec::new(), Vec::new(), None, |_, _, _| {}, cx)
            })
        });
        view.update(cx, |view, _| {
            for index in 0..=MAX_CACHED_BODIES {
                view.insert_body(index, CachedBody::Loaded("cached text".into()));
            }
            view.visible_range = MAX_CACHED_BODIES..MAX_CACHED_BODIES + 1;
            view.touch_body(0);
            view.evict_cache();
            assert!(view.bodies.contains_key(&0));
            assert!(!view.bodies.contains_key(&1));
            assert!(view.bodies.contains_key(&MAX_CACHED_BODIES));
            assert_eq!(view.bodies.len(), MAX_CACHED_BODIES);
            assert_eq!(view.cache_order.len(), MAX_CACHED_BODIES);
            assert_eq!(
                view.cached_text_bytes,
                "cached text".len() * MAX_CACHED_BODIES
            );
        });
    }

    #[gpui::test]
    fn failed_body_is_cached_until_explicit_retry(cx: &mut TestAppContext) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let record = r#"{"type":"event_msg","payload":{"type":"agent_message","message":"Recovered message"}}"#;
        let mut lines = test_lines(1);
        lines[0].path = path.clone();
        lines[0].byte_len = record.len() as u64;
        let view = cx.update(|cx| {
            cx.new(|cx| SessionView::new(test_file(1), lines, Vec::new(), None, |_, _, _| {}, cx))
        });
        wait_for_load(&view, cx);
        view.update(cx, |view, cx| {
            assert!(matches!(
                view.bodies.get(&0),
                Some(CachedBody::Unavailable(_))
            ));
            view.load_visible_bodies(cx);
            assert!(
                view.loading.is_empty(),
                "errors must not trigger an automatic retry loop"
            );
        });
        fs::write(&path, record).unwrap();
        view.update(cx, |view, cx| view.retry_body(0, cx));
        wait_for_load(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(matches!(view.bodies.get(&0), Some(CachedBody::Loaded(text)) if text.as_ref() == "Recovered message"));
            assert_eq!(view.cached_text_bytes, "Recovered message".len());
        });
    }

    #[gpui::test]
    fn completed_batch_discards_old_viewport_and_loads_latest(cx: &mut TestAppContext) {
        let mut lines = test_lines(1_000);
        // These records return metadata-only placeholders without any file I/O.
        for line in &mut lines {
            line.byte_len = super::MAX_INLINE_SOURCE_BYTES + 1;
        }
        let view = cx.update(|cx| {
            cx.new(|cx| {
                SessionView::new(test_file(1_000), lines, Vec::new(), None, |_, _, _| {}, cx)
            })
        });
        view.update(cx, |view, cx| {
            view.visible_range = 0..4;
            view.load_visible_bodies(cx);
        });
        wait_for_load(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.bodies.contains_key(&0));
            assert!(view.bodies.contains_key(&3));
            assert!(!view.bodies.contains_key(&999));
            assert!(view.bodies.len() <= MAX_CACHED_BODIES);
        });
    }

    fn wait_for_load(view: &Entity<SessionView>, cx: &mut TestAppContext) {
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.loading.is_empty()));
    }

    fn test_lines(count: usize) -> Vec<IndexedLine> {
        (0..count)
            .map(|index| IndexedLine {
                file_id: 1,
                source: SessionSource::Codex,
                path: PathBuf::from("/tmp/agentoria-test-missing.jsonl"),
                ordinal: index as u64,
                line_number: index as u64 + 1,
                byte_offset: 0,
                byte_len: 64,
                event_type: "event_msg.agent_message".to_string(),
                timestamp: None,
                conversation_kind: Some(ConversationKind::Assistant),
                process_count: 0,
            })
            .collect()
    }

    fn search_fixture() -> (tempfile::TempDir, Vec<IndexedLine>) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("search.jsonl");
        let texts = [
            "First **Rust** and RUST",
            "Unrelated content",
            "Last rust 中文",
        ];
        let mut source = String::new();
        let mut lines = test_lines(texts.len());
        for (index, (line, text)) in lines.iter_mut().zip(texts).enumerate() {
            let record = format!(
                "{}\n",
                serde_json::json!({
                    "type": "event_msg", "payload": { "type": if index == 0 { "user_message" } else { "agent_message" }, "message": text }
                })
            );
            line.path = path.clone();
            line.byte_offset = source.len() as u64;
            line.byte_len = record.len() as u64;
            line.conversation_kind = Some(if index == 0 {
                ConversationKind::User
            } else {
                ConversationKind::Assistant
            });
            source.push_str(&record);
        }
        fs::write(path, source).unwrap();
        (directory, lines)
    }

    #[gpui::test]
    fn search_latest_query_wins_and_navigation_wraps_with_process_row(cx: &mut TestAppContext) {
        let (_directory, lines) = search_fixture();
        let view = cx.update(|cx| {
            cx.new(|cx| {
                SessionView::new(test_file(3), lines, test_lines(1), None, |_, _, _| {}, cx)
            })
        });
        view.update(cx, |view, cx| {
            view.set_search_query("unrelated".to_string(), cx);
            view.set_search_query("RUST".to_string(), cx);
        });
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(250));
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert_eq!(view.search.query, "RUST");
            assert_eq!(view.search.results.total_matches, 3);
            assert_eq!(view.search.active, Some(0));
            assert_eq!(view.list_state.logical_scroll_top().item_ix, 1);
            view.step_search_match(true, cx);
            assert_eq!(view.search.active, Some(2));
            assert_eq!(view.list_state.logical_scroll_top().item_ix, 3);
            view.step_search_match(false, cx);
            assert_eq!(view.search.active, Some(0));
            view.set_search_query("".to_string(), cx);
            assert!(view.search.results.matches.is_empty());
            assert!(!view.search.searching);
        });
    }

    #[gpui::test]
    fn session_search_keyboard_input_and_markdown_highlight_draw(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_directory, lines) = search_fixture();
        let slot = Rc::new(RefCell::new(None::<Entity<SessionView>>));
        let view_slot = slot.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                SessionView::new(test_file(3), lines, Vec::new(), None, |_, _, _| {}, cx)
            });
            *view_slot.borrow_mut() = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let view = slot.borrow().as_ref().unwrap().clone();
        let cx: &mut VisualTestContext = cx;
        settle_visual_load(&view, cx);
        let key = |key: &str| gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
            is_held: false,
            prefer_character_input: false,
        };
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.handle_search_key_down(&key("cmd-f"), window, cx));
            })
        });
        draw(cx);
        cx.simulate_input("rust");
        draw(cx);
        let executor = cx.update(|_, cx| cx.background_executor().clone());
        executor.advance_clock(std::time::Duration::from_millis(250));
        for _ in 0..4 {
            draw(cx);
        }
        view.read_with(cx, |view, cx| {
            assert!(view.search.open);
            assert_eq!(view.search.results.total_matches, 3);
            let highlight = view
                .search
                .highlight
                .as_ref()
                .expect("active match should have a text view");
            let (snapshot, range, _) = highlight
                .stamp
                .as_ref()
                .expect("parse should apply the highlight");
            let rendered = snapshot.range_for_source(range.clone()).unwrap();
            assert_eq!(&snapshot.as_str()[rendered], "Rust");
            assert_eq!(highlight.view.read(cx).rendered_text(), *snapshot);
        });
        let first = view.read_with(cx, |view, _| {
            view.search.highlight.as_ref().unwrap().view.entity_id()
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.handle_search_key_down(&key("enter"), window, cx));
            })
        });
        for _ in 0..3 {
            draw(cx);
        }
        view.read_with(cx, |view, _| {
            assert_eq!(view.search.active, Some(1));
            assert_eq!(
                view.search.highlight.as_ref().unwrap().view.entity_id(),
                first
            );
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                window.blur(cx);
                assert!(!view.handle_search_key_down(&key("enter"), window, cx));
                assert!(view.handle_search_key_down(&key("cmd-g"), window, cx));
                assert_eq!(view.search.active, Some(2));
                assert!(view.handle_search_key_down(&key("cmd-shift-g"), window, cx));
                view.focus_search(window, cx);
                assert!(view.handle_search_key_down(&key("shift-enter"), window, cx));
                assert_eq!(view.search.active, Some(0));
                view.toggle_markdown(cx);
            })
        });
        draw(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.handle_search_key_down(&key("escape"), window, cx));
                assert!(!view.search.open);
                assert!(view.search.query.is_empty());
                assert!(view.search.highlight.is_none());
            })
        });
        draw(cx);
    }

    #[gpui::test]
    fn layout_loads_programmatic_scroll_and_saves_exact_offset(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let slot = Rc::new(RefCell::new(None::<Entity<SessionView>>));
        let view_slot = slot.clone();
        let saved_anchor = Rc::new(RefCell::new(None));
        let scroll_slot = saved_anchor.clone();
        let mut lines = test_lines(1_000);
        for line in &mut lines {
            line.byte_len = super::MAX_INLINE_SOURCE_BYTES + 1;
        }
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                SessionView::new(
                    test_file(1_000),
                    lines,
                    Vec::new(),
                    None,
                    move |anchor, _, _| *scroll_slot.borrow_mut() = Some(anchor),
                    cx,
                )
            });
            *view_slot.borrow_mut() = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let view = slot.borrow().as_ref().unwrap().clone();
        let cx: &mut VisualTestContext = cx;
        settle_visual_load(&view, cx);

        // Neither scroll_to nor async row-height changes emit a scroll event.
        view.update(cx, |view, cx| {
            view.list_state.set_follow_mode(FollowMode::Normal);
            view.list_state.scroll_to(ListOffset {
                item_ix: 500,
                offset_in_item: px(27.),
            });
            cx.notify();
        });
        settle_visual_load(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.bodies.contains_key(&500));
            assert!(view.visible_range.contains(&500));
        });
        assert!(saved_anchor.borrow().is_none());

        cx.simulate_event(ScrollWheelEvent {
            position: point(px(600.), px(400.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(37.))),
            ..Default::default()
        });
        draw(cx);
        let anchor = saved_anchor
            .borrow()
            .expect("scroll should save its anchor");
        view.read_with(cx, |view, _| {
            let actual = view.list_state.logical_scroll_top();
            assert_eq!(anchor.item_index, actual.item_ix);
            assert_eq!(anchor.offset_in_item, actual.offset_in_item.as_f32());
            assert!(anchor.offset_in_item > 0.);
            assert!(!anchor.is_following_tail);
        });
    }

    fn settle_visual_load(view: &Entity<SessionView>, cx: &mut VisualTestContext) {
        for _ in 0..16 {
            draw(cx);
            if view.read_with(cx, |view, _| view.loading.is_empty()) {
                return;
            }
        }
        panic!("visible body loading did not settle after 16 frames");
    }

    // GPUI's debug_selector is a no-op in optimized dependencies. This test
    // needs its recorded bounds to locate the text before simulating a drag.
    #[cfg(debug_assertions)]
    #[gpui::test]
    fn transcript_text_can_be_selected_by_dragging(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let text = "Alpha bravo charlie delta";
        let file = test_file(1);
        let line = IndexedLine {
            file_id: file.id,
            source: SessionSource::Codex,
            path: PathBuf::from("/tmp/selection.jsonl"),
            ordinal: 0,
            line_number: 1,
            byte_offset: 0,
            byte_len: text.len() as u64,
            event_type: "event_msg.agent_message".to_string(),
            timestamp: Some("2026-08-28T01:00:00.000Z".to_string()),
            conversation_kind: Some(ConversationKind::Assistant),
            process_count: 0,
        };
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| {
                let list_state = ListState::new(1, ListAlignment::Bottom, px(1200.))
                    .with_uniform_item_height(px(180.));
                list_state.set_follow_mode(FollowMode::Tail);
                SessionView {
                    file,
                    conversation_lines: vec![line].into(),
                    process_lines: Vec::new().into(),
                    list_state,
                    bodies: HashMap::from([(0, CachedBody::Loaded(text.into()))]),
                    loading: HashSet::new(),
                    load_task: None,
                    cache_order: VecDeque::new(),
                    cached_text_bytes: text.len(),
                    visible_range: 0..1,
                    rendered_range: None,
                    render_markdown: true,
                    show_jump_to_bottom: false,
                    on_scroll: Rc::new(|_, _, _| {}),
                    search: crate::session_search::SessionSearch::default(),
                }
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        let cx: &mut VisualTestContext = cx;
        draw(cx);

        let header = cx
            .debug_bounds("session-message-header")
            .expect("message header should be rendered");
        let from = point(header.left() + px(4.), header.bottom() + px(10.));
        let to = point(from.x + px(140.), from.y);
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        draw(cx);
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        draw(cx);
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        draw(cx);

        let selected = cx.update(gpui_kit::base::TextSelection::selected_text);
        assert!(
            !selected.trim().is_empty(),
            "dragging across transcript text should select content"
        );
    }

    #[gpui::test]
    fn transcript_scroll_p95_stays_within_one_frame(cx: &mut TestAppContext) {
        const ROWS: usize = 1_000;
        const SAMPLES: usize = 40;

        cx.update(gpui_kit::init);
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
                    conversation_lines: lines.into(),
                    process_lines: Vec::new().into(),
                    list_state,
                    bodies: HashMap::new(),
                    loading: HashSet::new(),
                    load_task: None,
                    cache_order: VecDeque::new(),
                    cached_text_bytes: 0,
                    visible_range: ROWS.saturating_sub(16)..ROWS,
                    rendered_range: None,
                    render_markdown: true,
                    show_jump_to_bottom: false,
                    on_scroll: Rc::new(|_, _, _| {}),
                    search: crate::session_search::SessionSearch::default(),
                };
                let markdown =
                    "## Result\n\n- first item\n- second item\n\n```rust\nfn main() {}\n```";
                for index in 0..ROWS {
                    view.insert_body(index, CachedBody::Loaded(markdown.into()));
                }
                view
            });
            gpui_kit::component::Root::new(view, window, cx)
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

    #[gpui::test]
    fn large_transcript_keeps_cache_bounded_across_viewports(cx: &mut TestAppContext) {
        const ROWS: usize = 10_000;
        const PROCESS_ROWS: usize = 100_000;
        let directory = tempdir().unwrap();
        let path = directory.path().join("large.jsonl");
        let markdown = format!(
            "## Result\n\n{}\n\n```rust\nfn main() {{}}\n```",
            "Transcript text. ".repeat(128)
        );
        let record = format!(
            "{}\n",
            serde_json::json!({
                "type": "event_msg", "payload": {"type": "agent_message", "message": markdown}
            })
        );
        let mut source = BufWriter::new(fs::File::create(&path).unwrap());
        let mut lines = test_lines(ROWS);
        for (index, line) in lines.iter_mut().enumerate() {
            source.write_all(record.as_bytes()).unwrap();
            line.path = path.clone();
            line.byte_offset = (index * record.len()) as u64;
            line.byte_len = record.len() as u64;
        }
        source.flush().unwrap();
        let process = test_lines(PROCESS_ROWS);
        cx.update(gpui_kit::init);
        let slot = Rc::new(RefCell::new(None::<Entity<SessionView>>));
        let view_slot = slot.clone();
        let started = Instant::now();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                SessionView::new(test_file(ROWS), lines, process, None, |_, _, _| {}, cx)
            });
            *view_slot.borrow_mut() = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let view = slot.borrow().as_ref().unwrap().clone();
        let cx: &mut VisualTestContext = cx;
        settle_visual_load(&view, cx);
        let first_screen_ms = started.elapsed().as_secs_f64() * 1_000.;
        eprintln!("large transcript rows={ROWS} process_rows={PROCESS_ROWS} indexed first screen={first_screen_ms:.2}ms");
        if !cfg!(debug_assertions) {
            assert!(
                first_screen_ms <= 500.,
                "indexed first screen exceeds 500ms"
            );
        }

        view.update(cx, |view, cx| {
            view.set_search_query("transcript".to_string(), cx)
        });
        draw(cx);
        let executor = cx.update(|_, cx| cx.background_executor().clone());
        let search_started = Instant::now();
        executor.advance_clock(std::time::Duration::from_millis(250));
        settle_visual_load(&view, cx);
        eprintln!(
            "large transcript search={:.2}ms",
            search_started.elapsed().as_secs_f64() * 1_000.
        );
        view.update(cx, |view, cx| {
            assert!(!view.search.searching);
            assert_eq!(view.search.results.total_matches, ROWS * 128);
            assert_eq!(
                view.search.results.matches.len(),
                crate::session_search::MAX_SEARCH_MATCHES
            );
            assert_eq!(
                view.search.results.oversized + view.search.results.unreadable,
                0
            );
            assert!(view.bodies.len() <= MAX_CACHED_BODIES);
            view.set_search_query(String::new(), cx);
        });

        for index in (0..ROWS).step_by(137) {
            view.update(cx, |view, cx| {
                view.list_state.set_follow_mode(FollowMode::Normal);
                view.list_state.scroll_to(ListOffset {
                    item_ix: index + 1,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
            settle_visual_load(&view, cx);
            view.read_with(cx, |view, _| {
                assert!(matches!(
                    view.bodies.get(&index),
                    Some(CachedBody::Loaded(_))
                ));
                assert!(view.bodies.len() <= MAX_CACHED_BODIES);
                assert!(view.cached_text_bytes <= super::MAX_CACHED_TEXT_BYTES);
                assert_eq!(
                    view.cached_text_bytes,
                    view.bodies
                        .values()
                        .map(CachedBody::text_bytes)
                        .sum::<usize>()
                );
                assert_eq!(view.cache_order.len(), view.bodies.len());
            });
        }
        // Exercise the process summary with all 100,000 metadata rows present.
        view.update(cx, |view, cx| {
            view.list_state.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        settle_visual_load(&view, cx);
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
