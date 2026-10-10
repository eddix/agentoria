use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use agentoria_core::{
    index::{CancellationToken, IndexDatabase, IndexedFile},
    SessionSource,
};
use gpui::{
    div, list, prelude::FluentBuilder as _, px, Anchor, AnyElement, App, AppContext, Context,
    Entity, InteractiveElement, IntoElement, KeyDownEvent, ListAlignment, ListState, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window,
};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress,
    resizable::{h_resizable, resizable_panel, ResizableState},
    skeleton::Skeleton,
    tab::TabBar,
    v_flex, ActiveTheme, Icon, IconName, Root, Sizable as _, StyledExt as _, ThemeMode, TitleBar,
};

use crate::{
    indexer::{index_path, run_source_load, SourceLoadEvent},
    preferences::{Preferences, ViewMode},
    session_view::SessionView,
    skill_catalog::SkillAgent,
    skills_view::SkillsView,
    themes,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContentMode {
    Sessions,
    Skills,
}

#[derive(Debug, Clone)]
enum SessionListItem {
    Group {
        key: String,
        label: String,
        count: usize,
    },
    Session(i64),
}

pub struct WorkspaceView {
    mode: ContentMode,
    source: SessionSource,
    sessions: Vec<IndexedFile>,
    session_items: Vec<SessionListItem>,
    session_list_state: ListState,
    selected_file_id: Option<i64>,
    active_session: Option<Entity<SessionView>>,
    skills_view: Option<Entity<SkillsView>>,
    skills_subscription: Option<Subscription>,
    source_loading: bool,
    session_loading: bool,
    discovered_files: usize,
    indexed_files: usize,
    current_file_progress: f32,
    status_text: String,
    file_errors: usize,
    index_path: PathBuf,
    source_revision: u64,
    session_revision: u64,
    cancellation: CancellationToken,
    preferences: Preferences,
    collapsed_groups: HashSet<String>,
    resizable_state: Entity<ResizableState>,
    source_task: Option<Task<()>>,
    source_events_task: Option<Task<()>>,
    session_task: Option<Task<()>>,
    preferences_task: Option<Task<()>>,
}

impl WorkspaceView {
    pub fn new(preferences: Preferences, cx: &mut Context<Self>) -> Self {
        let index_path = index_path().unwrap_or_else(|_| PathBuf::from("index.sqlite"));
        let source = preferences.source();
        let mut view = Self {
            mode: ContentMode::Sessions,
            source,
            sessions: Vec::new(),
            session_items: Vec::new(),
            session_list_state: ListState::new(0, ListAlignment::Top, px(500.))
                .with_uniform_item_height(px(86.)),
            selected_file_id: None,
            active_session: None,
            skills_view: None,
            skills_subscription: None,
            source_loading: false,
            session_loading: false,
            discovered_files: 0,
            indexed_files: 0,
            current_file_progress: 0.,
            status_text: "Starting native index".to_string(),
            file_errors: 0,
            index_path,
            source_revision: 0,
            session_revision: 0,
            cancellation: CancellationToken::default(),
            preferences,
            collapsed_groups: HashSet::new(),
            resizable_state: cx.new(|_| ResizableState::default()),
            source_task: None,
            source_events_task: None,
            session_task: None,
            preferences_task: None,
        };
        view.load_source(source, cx);
        view
    }

    fn load_source(&mut self, source: SessionSource, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.cancellation = CancellationToken::default();
        self.source_revision = self.source_revision.wrapping_add(1);
        self.session_revision = self.session_revision.wrapping_add(1);
        self.source = source;
        self.preferences.set_source(source);
        self.schedule_preferences_save(cx);
        self.sessions.clear();
        self.session_items.clear();
        self.session_list_state.reset(0);
        self.selected_file_id = None;
        self.active_session = None;
        self.source_loading = true;
        self.session_loading = false;
        self.discovered_files = 0;
        self.indexed_files = 0;
        self.current_file_progress = 0.;
        self.file_errors = 0;
        self.status_text = format!("Loading {} sessions", source_label(source));

        let revision = self.source_revision;
        let cancellation = self.cancellation.clone();
        let index_path = self.index_path.clone();
        let (sender, receiver) = async_channel::unbounded();
        let worker_sender = sender.clone();
        self.source_task = Some(cx.background_spawn(async move {
            let result = smol::unblock(move || {
                run_source_load(source, &index_path, &cancellation, &worker_sender)
            })
            .await;
            if let Err(error) = result {
                sender
                    .send(SourceLoadEvent::Failed(format!("{error:#}")))
                    .await
                    .ok();
            }
        }));
        self.source_events_task = Some(cx.spawn(async move |weak, cx| {
            while let Ok(event) = receiver.recv().await {
                let keep_receiving = weak
                    .update(cx, |view, cx| {
                        if view.source_revision != revision {
                            return false;
                        }
                        view.apply_source_event(event, cx);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_receiving {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn apply_source_event(&mut self, event: SourceLoadEvent, cx: &mut Context<Self>) {
        match event {
            SourceLoadEvent::Cached(files) => {
                self.sessions = files;
                self.sort_sessions();
                self.rebuild_session_items();
                self.status_text = format!("{} cached sessions ready", self.sessions.len());
            }
            SourceLoadEvent::Discovered { total } => {
                self.discovered_files = total;
                self.status_text = format!("Discovered {total} session files");
            }
            SourceLoadEvent::Progress {
                path,
                file_index,
                total_files,
                scan,
            } => {
                self.indexed_files = file_index;
                self.discovered_files = total_files;
                self.current_file_progress = if scan.total_bytes == 0 {
                    100.
                } else {
                    scan.bytes_scanned as f32 / scan.total_bytes as f32 * 100.
                };
                self.status_text = format!(
                    "Indexing {} · {}/{}",
                    short_path(&path),
                    file_index.saturating_add(1),
                    total_files
                );
            }
            SourceLoadEvent::Indexed(file) => {
                let file_id = file.id;
                let refresh_active = self.selected_file_id == Some(file_id)
                    && self.active_session.is_some()
                    && !self.session_loading;
                self.upsert_session(*file);
                self.indexed_files = self.indexed_files.saturating_add(1);
                if refresh_active {
                    self.select_session(file_id, cx);
                }
            }
            SourceLoadEvent::FileError { path, message } => {
                self.file_errors += 1;
                self.status_text = format!("Skipped {} · {message}", short_path(&path));
            }
            SourceLoadEvent::Finished => {
                self.source_loading = false;
                self.current_file_progress = 100.;
                self.status_text = if self.file_errors == 0 {
                    format!("{} sessions indexed", self.sessions.len())
                } else {
                    format!(
                        "{} sessions indexed · {} errors",
                        self.sessions.len(),
                        self.file_errors
                    )
                };
            }
            SourceLoadEvent::Failed(message) => {
                self.source_loading = false;
                self.status_text = format!("Index failed · {message}");
            }
        }
    }

    fn upsert_session(&mut self, file: IndexedFile) {
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|existing| existing.id == file.id)
        {
            *existing = file;
        } else {
            self.sessions.push(file);
        }
        self.sort_sessions();
        self.rebuild_session_items();
    }

    fn sort_sessions(&mut self) {
        self.sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.path.cmp(&right.path))
        });
    }

    fn rebuild_session_items(&mut self) {
        let visible = self
            .sessions
            .iter()
            .filter(|file| !self.preferences.hide_subagents || !file.is_sidechain)
            .collect::<Vec<_>>();
        self.session_items = match self.preferences.view_mode {
            ViewMode::Recent => visible
                .into_iter()
                .map(|file| SessionListItem::Session(file.id))
                .collect(),
            ViewMode::Grouped => {
                let mut groups: Vec<(String, String, Vec<i64>)> = Vec::new();
                for file in visible {
                    let key = workspace_group_key(file);
                    if let Some((_, _, files)) =
                        groups.iter_mut().find(|(existing, _, _)| *existing == key)
                    {
                        files.push(file.id);
                    } else {
                        groups.push((key.clone(), workspace_group_label(file), vec![file.id]));
                    }
                }
                groups
                    .into_iter()
                    .flat_map(|(key, label, files)| {
                        let count = files.len();
                        let collapsed = self.collapsed_groups.contains(&key);
                        std::iter::once(SessionListItem::Group { key, label, count }).chain(
                            (!collapsed)
                                .then_some(files)
                                .into_iter()
                                .flatten()
                                .map(SessionListItem::Session),
                        )
                    })
                    .collect()
            }
        };
        self.session_list_state.reset(self.session_items.len());
    }

    fn visible_session_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|file| !self.preferences.hide_subagents || !file.is_sidechain)
            .count()
    }

    fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        self.preferences.view_mode = mode;
        self.rebuild_session_items();
        self.schedule_preferences_save(cx);
        cx.notify();
    }

    fn toggle_subagents(&mut self, cx: &mut Context<Self>) {
        self.preferences.hide_subagents = !self.preferences.hide_subagents;
        self.rebuild_session_items();
        self.schedule_preferences_save(cx);
        cx.notify();
    }

    fn toggle_group(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed_groups.insert(key.to_string()) {
            self.collapsed_groups.remove(key);
        }
        self.rebuild_session_items();
        cx.notify();
    }

    fn schedule_preferences_save(&mut self, cx: &mut Context<Self>) {
        let preferences = self.preferences.clone();
        self.preferences_task = Some(cx.background_spawn(async move {
            smol::Timer::after(Duration::from_millis(250)).await;
            if let Err(error) = smol::unblock(move || preferences.save()).await {
                eprintln!("failed to save Agentoria preferences: {error:#}");
            }
        }));
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = if cx.theme().is_dark() {
            self.preferences.light_theme.clone()
        } else {
            self.preferences.dark_theme.clone()
        };
        self.select_theme(&theme, window, cx);
    }

    fn select_theme(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mode) = themes::apply_theme(name, Some(window), cx) else {
            return;
        };
        self.preferences.set_theme(name, mode.is_dark());
        self.schedule_preferences_save(cx);
        cx.notify();
    }

    fn set_mode(&mut self, mode: ContentMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == ContentMode::Skills && self.skills_view.is_none() {
            let agent = match self.source {
                SessionSource::Claude => SkillAgent::Claude,
                SessionSource::Codex => SkillAgent::Codex,
            };
            let workspace = self
                .selected_file_id
                .and_then(|id| self.sessions.iter().find(|file| file.id == id))
                .and_then(|file| file.working_dir.as_ref())
                .map(PathBuf::from)
                .filter(|path| path.is_dir());
            let skills = cx.new(|cx| SkillsView::new(agent, workspace, window, cx));
            self.skills_subscription = Some(cx.observe(&skills, |_, _, cx| cx.notify()));
            self.skills_view = Some(skills);
        }
        cx.notify();
    }

    fn set_skill_agent(&mut self, agent: SkillAgent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(skills) = &self.skills_view {
            skills.update(cx, |view, cx| view.set_agent(agent, window, cx));
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != ContentMode::Skills {
            return;
        }
        let Some(skills) = &self.skills_view else {
            return;
        };
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.shift
            && event.keystroke.key.eq_ignore_ascii_case("f")
        {
            skills.update(cx, |view, cx| view.focus_search(window, cx));
            window.prevent_default();
            cx.stop_propagation();
        } else if !modifiers.modified()
            && event.keystroke.key.eq_ignore_ascii_case("escape")
            && skills.update(cx, |view, cx| view.clear_search(window, cx))
        {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    fn select_session(&mut self, file_id: i64, cx: &mut Context<Self>) {
        let Some(file) = self
            .sessions
            .iter()
            .find(|file| file.id == file_id)
            .cloned()
        else {
            return;
        };
        self.selected_file_id = Some(file_id);
        self.active_session = None;
        self.session_loading = true;
        self.session_revision = self.session_revision.wrapping_add(1);
        let revision = self.session_revision;
        let index_path = self.index_path.clone();
        let session_path = file.path.clone();
        let initial_anchor = self.preferences.scroll_anchor(&session_path);
        let load = cx.background_spawn(async move {
            smol::unblock(move || {
                let database =
                    IndexDatabase::open(index_path).map_err(|error| format!("{error:#}"))?;
                let conversations = database
                    .conversation_lines(file.id)
                    .map_err(|error| format!("{error:#}"))?;
                let process = database
                    .process_lines(file.id)
                    .map_err(|error| format!("{error:#}"))?;
                Ok::<_, String>((file, conversations, process))
            })
            .await
        });
        self.session_task = Some(cx.spawn(async move |weak, cx| {
            let result = load.await;
            let scroll_workspace = weak.clone();
            let _ = weak.update(cx, |view, cx| {
                if view.session_revision != revision {
                    return;
                }
                view.session_loading = false;
                match result {
                    Ok((file, conversations, process)) => {
                        view.status_text =
                            format!("Loaded {} conversation rows", conversations.len());
                        let anchor_path = session_path.clone();
                        let scroll_workspace = scroll_workspace.clone();
                        view.active_session = Some(cx.new(|cx| {
                            SessionView::new(
                                file,
                                conversations,
                                process,
                                initial_anchor,
                                move |anchor, _, cx| {
                                    let _ = scroll_workspace.update(cx, |view, cx| {
                                        view.preferences.set_scroll_anchor(&anchor_path, anchor);
                                        view.schedule_preferences_save(cx);
                                    });
                                },
                                cx,
                            )
                        }));
                    }
                    Err(message) => {
                        view.status_text = format!("Session load failed · {message}");
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_title_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self.source;
        let source_weak = cx.weak_entity();
        let skills_weak = cx.weak_entity();
        let theme_weak = cx.weak_entity();
        let skill_agent = self
            .skills_view
            .as_ref()
            .map(|view| view.read(cx).agent())
            .unwrap_or(match source {
                SessionSource::Claude => SkillAgent::Claude,
                SessionSource::Codex => SkillAgent::Codex,
            });
        let source_button: AnyElement = if self.mode == ContentMode::Sessions {
            Button::new("source-menu")
                .small()
                .outline()
                .label(source_label(source))
                .dropdown_caret(true)
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    let claude_weak = source_weak.clone();
                    let codex_weak = source_weak.clone();
                    menu.label("Session Source")
                        .item(
                            PopupMenuItem::new("Claude")
                                .checked(source == SessionSource::Claude)
                                .on_click(move |_, _, cx| {
                                    let _ = claude_weak.update(cx, |view, cx| {
                                        view.load_source(SessionSource::Claude, cx)
                                    });
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Codex")
                                .checked(source == SessionSource::Codex)
                                .on_click(move |_, _, cx| {
                                    let _ = codex_weak.update(cx, |view, cx| {
                                        view.load_source(SessionSource::Codex, cx)
                                    });
                                }),
                        )
                })
                .into_any_element()
        } else {
            Button::new("skill-agent-menu")
                .small()
                .outline()
                .label(skill_agent.label())
                .dropdown_caret(true)
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    let mut menu = menu.label("Skill Agent");
                    for agent in SkillAgent::ALL {
                        let weak = skills_weak.clone();
                        menu = menu.item(
                            PopupMenuItem::new(agent.label())
                                .checked(agent == skill_agent)
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |view, cx| {
                                        view.set_skill_agent(agent, window, cx)
                                    });
                                }),
                        );
                    }
                    menu
                })
                .into_any_element()
        };
        let active_theme = cx.theme().theme_name().to_string();
        let available_themes = themes::available_themes(cx);
        TitleBar::new().child(
            h_flex()
                .size_full()
                .justify_between()
                .pr_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(logo_mark(cx))
                        .child(div().font_semibold().child("Agentoria"))
                        .when(crate::IS_PREVIEW, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("GPUI PREVIEW"),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            TabBar::new("content-mode")
                                .segmented()
                                .small()
                                .selected_index(match self.mode {
                                    ContentMode::Sessions => 0,
                                    ContentMode::Skills => 1,
                                })
                                .on_click(cx.listener(|view, index: &usize, window, cx| {
                                    let mode = match index {
                                        0 => ContentMode::Sessions,
                                        1 => ContentMode::Skills,
                                        _ => return,
                                    };
                                    view.set_mode(mode, window, cx);
                                }))
                                .child("Sessions")
                                .child("Skills"),
                        )
                        .child(source_button)
                        .child(
                            Button::new("theme-menu")
                                .small()
                                .outline()
                                .icon(IconName::Palette)
                                .label(active_theme.clone())
                                .dropdown_caret(true)
                                .tooltip("Select color theme")
                                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                    let mut menu =
                                        menu.max_h(px(620.)).scrollable(true).label("Light themes");
                                    for (name, _) in available_themes
                                        .iter()
                                        .filter(|(_, mode)| *mode == ThemeMode::Light)
                                    {
                                        let weak = theme_weak.clone();
                                        let selected_name = name.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(name.clone())
                                                .icon(IconName::Sun)
                                                .checked(*name == active_theme)
                                                .on_click(move |_, window, cx| {
                                                    let _ = weak.update(cx, |view, cx| {
                                                        view.select_theme(
                                                            &selected_name,
                                                            window,
                                                            cx,
                                                        )
                                                    });
                                                }),
                                        );
                                    }
                                    menu = menu.separator().label("Dark themes");
                                    for (name, _) in available_themes
                                        .iter()
                                        .filter(|(_, mode)| *mode == ThemeMode::Dark)
                                    {
                                        let weak = theme_weak.clone();
                                        let selected_name = name.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(name.clone())
                                                .icon(IconName::Moon)
                                                .checked(*name == active_theme)
                                                .on_click(move |_, window, cx| {
                                                    let _ = weak.update(cx, |view, cx| {
                                                        view.select_theme(
                                                            &selected_name,
                                                            window,
                                                            cx,
                                                        )
                                                    });
                                                }),
                                        );
                                    }
                                    menu
                                }),
                        )
                        .child(
                            Button::new("theme-toggle")
                                .small()
                                .ghost()
                                .icon(if cx.theme().is_dark() {
                                    IconName::Sun
                                } else {
                                    IconName::Moon
                                })
                                .tooltip("Toggle light and dark mode")
                                .on_click(
                                    cx.listener(|view, _, window, cx| {
                                        view.toggle_theme(window, cx)
                                    }),
                                ),
                        ),
                ),
        )
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.visible_session_count();
        let view_mode = self.preferences.view_mode;
        let hide_subagents = self.preferences.hide_subagents;
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("SESSIONS"),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child(div().font_semibold().child(source_label(self.source)))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{count} shown")),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .pt_2()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("view-recent")
                                            .xsmall()
                                            .when(view_mode == ViewMode::Recent, |button| {
                                                button.primary()
                                            })
                                            .label("Recent")
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.set_view_mode(ViewMode::Recent, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("view-grouped")
                                            .xsmall()
                                            .when(view_mode == ViewMode::Grouped, |button| {
                                                button.primary()
                                            })
                                            .label("Grouped")
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.set_view_mode(ViewMode::Grouped, cx)
                                            })),
                                    ),
                            )
                            .child(
                                Button::new("toggle-subagents")
                                    .xsmall()
                                    .outline()
                                    .icon(IconName::EyeOff)
                                    .label(if hide_subagents { "Hidden" } else { "Shown" })
                                    .tooltip("Show or hide subagent sessions")
                                    .on_click(
                                        cx.listener(|view, _, _, cx| view.toggle_subagents(cx)),
                                    ),
                            ),
                    ),
            )
            .child(
                list(
                    self.session_list_state.clone(),
                    cx.processor(|view, index: usize, _, cx| view.render_session_row(index, cx)),
                )
                .size_full(),
            )
            .into_any_element()
    }

    fn render_session_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.session_items.get(index).cloned() else {
            return div().into_any_element();
        };
        let SessionListItem::Session(file_id) = item else {
            let SessionListItem::Group { key, label, count } = item else {
                unreachable!();
            };
            let collapsed = self.collapsed_groups.contains(&key);
            return h_flex()
                .id(SharedString::from(format!("session-group-{key}")))
                .relative()
                .w_full()
                .min_h(px(42.))
                .justify_between()
                .gap_2()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().sidebar_border)
                .bg(cx.theme().sidebar_accent)
                .text_color(cx.theme().sidebar_accent_foreground)
                .cursor_pointer()
                .hover(|this| this.bg(cx.theme().sidebar_primary.opacity(0.14)))
                .on_click(cx.listener(move |view, _, _, cx| view.toggle_group(&key, cx)))
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .child(
                            Icon::new(if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .small(),
                        )
                        .child(Icon::new(IconName::Folder).small())
                        .child(div().truncate().text_sm().font_semibold().child(label)),
                )
                .child(
                    div()
                        .min_w(px(24.))
                        .px_1()
                        .py_0p5()
                        .rounded_full()
                        .bg(cx.theme().sidebar_primary.opacity(0.12))
                        .text_xs()
                        .text_color(cx.theme().sidebar_primary)
                        .text_center()
                        .child(count.to_string()),
                )
                .into_any_element();
        };
        let Some(file) = self.sessions.iter().find(|file| file.id == file_id) else {
            return div().into_any_element();
        };
        let selected = self.selected_file_id == Some(file_id);
        let title = file_title(file);
        let workspace = workspace_label(file);
        v_flex()
            .id(SharedString::from(format!("session-row-{file_id}")))
            .w_full()
            .gap_1()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .cursor_pointer()
            .when(selected, |this| this.bg(cx.theme().sidebar_accent))
            .hover(|this| this.bg(cx.theme().sidebar_accent.opacity(0.65)))
            .on_click(cx.listener(move |view, _, _, cx| view.select_session(file_id, cx)))
            .child(div().truncate().font_medium().text_sm().child(title))
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(workspace),
            )
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} messages · {} process",
                        file.user_count + file.assistant_count,
                        file.process_count
                    ))
                    .child(short_timestamp(&file.updated_at)),
            )
            .into_any_element()
    }

    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(session) = &self.active_session {
            return session.clone().into_any_element();
        }
        if self.session_loading {
            return v_flex()
                .size_full()
                .gap_4()
                .p_5()
                .children((0..6).map(|_| Skeleton::new().h_20().rounded_md()))
                .into_any_element();
        }
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().font_semibold().child("Select a session"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("The transcript is loaded lazily from its JSONL byte offsets."),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let overall = if self.discovered_files == 0 {
            if self.source_loading {
                5.
            } else {
                100.
            }
        } else {
            ((self.indexed_files as f32 + self.current_file_progress / 100.)
                / self.discovered_files as f32
                * 100.)
                .clamp(0., 100.)
        };
        let (status, source_summary, show_progress) = if self.mode == ContentMode::Skills {
            if let Some(skills) = &self.skills_view {
                let skills = skills.read(cx);
                let (status, count, _) = skills.status();
                (
                    status,
                    format!("{} · {} skills", skills.agent().label(), count),
                    false,
                )
            } else {
                ("Opening skills".to_string(), "Skills".to_string(), false)
            }
        } else {
            (
                self.status_text.clone(),
                format!(
                    "{} · {} indexed",
                    source_label(self.source),
                    self.sessions.len()
                ),
                self.source_loading,
            )
        };
        h_flex()
            .h(px(28.))
            .flex_shrink_0()
            .justify_between()
            .gap_4()
            .px_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .min_w_0()
                    .gap_3()
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(status),
                    )
                    .when(show_progress, |this| {
                        this.child(
                            Progress::new("index-progress")
                                .value(overall)
                                .w(px(120.))
                                .h(px(5.)),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(source_summary),
            )
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = if self.mode == ContentMode::Skills {
            self.skills_view
                .as_ref()
                .map(|skills| skills.clone().into_any_element())
                .unwrap_or_else(|| div().into_any_element())
        } else {
            h_resizable("workspace-split")
                .with_state(&self.resizable_state)
                .child(
                    resizable_panel()
                        .size(px(340.))
                        .size_range(px(280.)..px(520.))
                        .child(self.render_sidebar(cx)),
                )
                .child(resizable_panel().child(self.render_detail(cx)))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .capture_key_down(
                cx.listener(|view, event, window, cx| view.handle_key_down(event, window, cx)),
            )
            .child(self.render_title_bar(cx))
            .child(content)
            .child(self.render_status_bar(cx))
    }
}

pub fn root_view(
    workspace: Entity<WorkspaceView>,
    window: &mut Window,
    cx: &mut Context<Root>,
) -> Root {
    Root::new(workspace, window, cx)
}

fn logo_mark(cx: &App) -> impl IntoElement {
    h_flex()
        .size(px(22.))
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(cx.theme().primary.opacity(0.1))
        .child(
            div()
                .w(px(3.))
                .h(px(14.))
                .rounded_full()
                .bg(cx.theme().primary),
        )
        .child(
            v_flex()
                .gap(px(2.))
                .ml_1()
                .child(div().w(px(8.)).h(px(2.)).bg(cx.theme().primary))
                .child(
                    div()
                        .w(px(6.))
                        .h(px(2.))
                        .bg(cx.theme().foreground.opacity(0.7)),
                )
                .child(
                    div()
                        .w(px(4.))
                        .h(px(2.))
                        .bg(cx.theme().foreground.opacity(0.35)),
                ),
        )
}

fn source_label(source: SessionSource) -> &'static str {
    match source {
        SessionSource::Claude => "Claude",
        SessionSource::Codex => "Codex",
    }
}

fn file_title(file: &IndexedFile) -> String {
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

fn workspace_label(file: &IndexedFile) -> String {
    file.working_dir
        .as_deref()
        .map(Path::new)
        .map(short_path)
        .or_else(|| file.path.parent().map(short_path))
        .unwrap_or_else(|| "Unknown workspace".to_string())
}

fn workspace_group_key(file: &IndexedFile) -> String {
    file.working_dir
        .clone()
        .or_else(|| {
            file.path
                .parent()
                .map(|path| path.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown-workspace".to_string())
}

fn workspace_group_label(file: &IndexedFile) -> String {
    file.working_dir
        .as_deref()
        .map(Path::new)
        .map(short_path)
        .or_else(|| file.path.parent().map(short_path))
        .unwrap_or_else(|| "Unknown workspace".to_string())
}

fn short_path(path: &Path) -> String {
    let display = path.to_string_lossy();
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy();
        if let Some(rest) = display.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    display.into_owned()
}

fn short_timestamp(timestamp: &str) -> String {
    timestamp.get(0..16).unwrap_or(timestamp).replace('T', " ")
}
