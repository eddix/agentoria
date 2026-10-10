use std::{collections::HashSet, path::PathBuf, sync::Arc};

use gpui::{
    div, img, list, prelude::FluentBuilder as _, px, AnyElement, AppContext as _, Context, Entity,
    ImageSource, InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState,
    ObjectFit, ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Subscription, Task, Window,
};
use gpui_kit::base::text::{TextView, TextViewState};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState, Input, InputEvent, InputState},
    resizable::{h_resizable, resizable_panel, ResizableState},
    skeleton::Skeleton,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
};

use crate::skill_catalog::{
    self, SkillAgent, SkillBundle, SkillFileKind, SkillLink, SkillRoot, SkillSummary,
};

pub struct SkillsView {
    agent: SkillAgent,
    workspace: Option<PathBuf>,
    home: Option<PathBuf>,
    roots: Vec<SkillRoot>,
    skills: Vec<SkillSummary>,
    visible_indices: Vec<usize>,
    query: String,
    search_input: Entity<InputState>,
    _search_subscription: Subscription,
    list_state: ListState,
    file_list_state: ListState,
    visible_file_indices: Arc<Vec<usize>>,
    selected_path: Option<PathBuf>,
    selected_file: Option<PathBuf>,
    bundle: Option<Arc<SkillBundle>>,
    collapsed_folders: HashSet<PathBuf>,
    detail: Option<SharedString>,
    markdown_view: Option<Entity<TextViewState>>,
    markdown_subscription: Option<Subscription>,
    code_editor: Option<Entity<EditorState>>,
    pending_anchor: Option<String>,
    detail_error: Option<String>,
    navigation_error: Option<String>,
    scan_errors: Vec<String>,
    loading: bool,
    detail_loading: bool,
    revision: u64,
    detail_revision: u64,
    scan_task: Option<Task<()>>,
    detail_task: Option<Task<()>>,
    resizable_state: Entity<ResizableState>,
    file_resizable_state: Entity<ResizableState>,
    file_panel_open: bool,
}

impl SkillsView {
    pub fn new(
        agent: SkillAgent,
        workspace: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::empty(agent, workspace, dirs::home_dir(), window, cx);
        view.refresh(cx);
        view
    }

    fn empty(
        agent: SkillAgent,
        workspace: Option<PathBuf>,
        home: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter skills"));
        let search_subscription =
            cx.subscribe(&search_input, |view, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.set_query(input.read(cx).value().to_string(), cx);
                }
            });
        Self {
            agent,
            workspace,
            home,
            roots: Vec::new(),
            skills: Vec::new(),
            visible_indices: Vec::new(),
            query: String::new(),
            search_input,
            _search_subscription: search_subscription,
            list_state: ListState::new(0, ListAlignment::Top, px(500.))
                .with_uniform_item_height(px(78.)),
            file_list_state: ListState::new(0, ListAlignment::Top, px(700.))
                .with_uniform_item_height(px(30.)),
            visible_file_indices: Arc::new(Vec::new()),
            selected_path: None,
            selected_file: None,
            bundle: None,
            collapsed_folders: HashSet::new(),
            detail: None,
            markdown_view: None,
            markdown_subscription: None,
            code_editor: None,
            pending_anchor: None,
            detail_error: None,
            navigation_error: None,
            scan_errors: Vec::new(),
            loading: false,
            detail_loading: false,
            revision: 0,
            detail_revision: 0,
            scan_task: None,
            detail_task: None,
            resizable_state: cx.new(|_| ResizableState::default()),
            file_resizable_state: cx.new(|_| ResizableState::default()),
            file_panel_open: true,
        }
    }

    pub fn agent(&self) -> SkillAgent {
        self.agent
    }

    pub fn status(&self) -> (String, usize, bool) {
        let status = if self.loading {
            format!("Scanning {} skills", self.agent.label())
        } else if !self.query.trim().is_empty() {
            format!(
                "{} of {} skills match",
                self.visible_indices.len(),
                self.skills.len()
            )
        } else if self.scan_errors.is_empty() {
            format!("{} skills ready", self.skills.len())
        } else {
            format!(
                "{} skills · {} read errors",
                self.skills.len(),
                self.scan_errors.len()
            )
        };
        (status, self.visible_indices.len(), self.loading)
    }

    pub fn set_agent(&mut self, agent: SkillAgent, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent == agent {
            return;
        }
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.query.clear();
        self.agent = agent;
        self.skills.clear();
        self.visible_indices.clear();
        self.list_state.reset(0);
        self.selected_path = None;
        self.file_list_state.reset(0);
        self.visible_file_indices = Arc::new(Vec::new());
        self.selected_file = None;
        self.bundle = None;
        self.collapsed_folders.clear();
        self.detail = None;
        self.markdown_view = None;
        self.markdown_subscription = None;
        self.code_editor = None;
        self.pending_anchor = None;
        self.detail_error = None;
        self.navigation_error = None;
        self.detail_loading = false;
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.detail_task = None;
        self.refresh(cx);
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.query.is_empty() {
            return false;
        }
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.set_query(String::new(), cx);
        true
    }

    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if self.query == query {
            return;
        }
        self.query = query;
        self.rebuild_filtered(false, cx);
    }

    fn rebuild_filtered(&mut self, reload_detail: bool, cx: &mut Context<Self>) {
        self.visible_indices = skill_catalog::filter_skills(&self.skills, &self.query);
        self.list_state.reset(self.visible_indices.len());
        self.list_state.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        let selected = self
            .selected_path
            .as_ref()
            .filter(|path| {
                self.visible_indices
                    .iter()
                    .any(|index| &self.skills[*index].path == *path)
            })
            .cloned()
            .or_else(|| {
                self.visible_indices
                    .first()
                    .map(|index| self.skills[*index].path.clone())
            });
        match selected {
            Some(path) if reload_detail || self.selected_path.as_ref() != Some(&path) => {
                self.select_skill(path, cx);
            }
            None => {
                self.selected_path = None;
                self.file_list_state.reset(0);
                self.visible_file_indices = Arc::new(Vec::new());
                self.selected_file = None;
                self.bundle = None;
                self.collapsed_folders.clear();
                self.detail = None;
                self.markdown_view = None;
                self.markdown_subscription = None;
                self.code_editor = None;
                self.pending_anchor = None;
                self.detail_error = None;
                self.navigation_error = None;
                self.detail_loading = false;
                self.detail_revision = self.detail_revision.wrapping_add(1);
                self.detail_task = None;
            }
            _ => {}
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let Some(home) = self.home.as_deref() else {
            self.loading = false;
            self.scan_errors = vec!["Could not locate the home directory".to_string()];
            cx.notify();
            return;
        };
        self.roots = skill_catalog::roots(self.agent, home, self.workspace.as_deref());
        self.loading = true;
        let roots = self.roots.clone();
        self.scan_task = Some(cx.spawn(async move |weak, cx| {
            let scan = smol::unblock(move || skill_catalog::discover(&roots)).await;
            let _ = weak.update(cx, |view, cx| {
                if view.revision != revision {
                    return;
                }
                view.loading = false;
                view.scan_errors = scan.errors;
                view.skills = scan.skills;
                view.rebuild_filtered(true, cx);
            });
        }));
        cx.notify();
    }

    fn select_skill(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self
            .visible_indices
            .iter()
            .any(|index| self.skills[*index].path == path)
        {
            return;
        }
        self.selected_path = Some(path.clone());
        self.file_list_state.reset(0);
        self.visible_file_indices = Arc::new(Vec::new());
        self.selected_file = None;
        self.bundle = None;
        self.collapsed_folders.clear();
        self.detail = None;
        self.markdown_view = None;
        self.markdown_subscription = None;
        self.code_editor = None;
        self.pending_anchor = None;
        self.detail_error = None;
        self.navigation_error = None;
        self.detail_loading = true;
        self.detail_revision = self.detail_revision.wrapping_add(1);
        let revision = self.detail_revision;
        self.detail_task = Some(cx.spawn(async move |weak, cx| {
            let result = smol::unblock({
                let path = path.clone();
                move || {
                    let bundle = skill_catalog::read_bundle(&path)?;
                    let content = skill_catalog::read_detail(&bundle.entry_file)?;
                    Ok::<_, anyhow::Error>((bundle, content))
                }
            })
            .await;
            let _ = weak.update(cx, |view, cx| {
                if view.detail_revision != revision || view.selected_path.as_ref() != Some(&path) {
                    return;
                }
                view.detail_loading = false;
                match result {
                    Ok((bundle, content)) => {
                        view.selected_file = Some(bundle.entry_file.clone());
                        view.collapsed_folders = bundle
                            .files
                            .iter()
                            .filter(|entry| entry.kind.is_none())
                            .map(|entry| entry.relative.clone())
                            .collect();
                        view.bundle = Some(Arc::new(bundle));
                        view.rebuild_file_list();
                        view.install_markdown(content, None, cx);
                    }
                    Err(error) => view.detail_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn install_markdown(
        &mut self,
        content: String,
        anchor: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let state = cx.new(|cx| TextViewState::markdown(&content, cx));
        self.markdown_subscription = Some(cx.observe(&state, |view, _, cx| {
            view.try_reveal_anchor(cx);
        }));
        self.markdown_view = Some(state);
        self.code_editor = None;
        self.detail = Some(content.into());
        self.pending_anchor = anchor;
        self.try_reveal_anchor(cx);
    }

    fn try_reveal_anchor(&mut self, cx: &mut Context<Self>) {
        let (Some(anchor), Some(content), Some(state)) = (
            self.pending_anchor.as_deref(),
            self.detail.as_ref(),
            self.markdown_view.as_ref(),
        ) else {
            return;
        };
        let Some(heading) = skill_catalog::heading_for_anchor(content.as_ref(), anchor) else {
            self.pending_anchor = None;
            return;
        };
        let rendered = state.read(cx).rendered_text();
        let Some(offset) = rendered.as_str().find(&heading) else {
            return;
        };
        self.pending_anchor = None;
        state.update(cx, |state, cx| {
            let _ = state.reveal_range(offset..offset, cx);
        });
    }

    fn open_file(
        &mut self,
        path: PathBuf,
        anchor: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bundle) = self.bundle.clone() else {
            return;
        };
        let Some(entry) = bundle.file(&path) else {
            self.navigation_error = Some("File is not in this skill".to_string());
            cx.notify();
            return;
        };
        let Some(kind) = entry.kind else {
            return;
        };
        let mut ancestor = entry.relative.parent();
        let mut expanded = false;
        while let Some(folder) = ancestor.filter(|folder| !folder.as_os_str().is_empty()) {
            expanded |= self.collapsed_folders.remove(folder);
            ancestor = folder.parent();
        }
        if expanded {
            self.rebuild_file_list();
        }
        if self.selected_file.as_ref() == Some(&path) && !self.detail_loading {
            self.pending_anchor = anchor;
            self.try_reveal_anchor(cx);
            return;
        }
        self.selected_file = Some(path.clone());
        self.detail = None;
        self.markdown_view = None;
        self.markdown_subscription = None;
        self.code_editor = None;
        self.pending_anchor = anchor;
        self.detail_error = None;
        self.navigation_error = None;
        self.detail_loading = true;
        self.detail_revision = self.detail_revision.wrapping_add(1);
        let revision = self.detail_revision;
        self.detail_task = Some(cx.spawn_in(window, async move |weak, cx| {
            let result = smol::unblock({
                let bundle = bundle.clone();
                let path = path.clone();
                move || skill_catalog::read_skill_file(&bundle, &path)
            })
            .await;
            let _ = weak.update_in(cx, |view, window, cx| {
                if view.detail_revision != revision || view.selected_file.as_ref() != Some(&path) {
                    return;
                }
                view.detail_loading = false;
                match result {
                    Ok(Some(content)) => match kind {
                        SkillFileKind::Markdown => {
                            let anchor = view.pending_anchor.take();
                            view.install_markdown(content, anchor, cx);
                        }
                        SkillFileKind::Code(language) => {
                            let editor = cx.new(|cx| {
                                let state = EditorState::new(window, cx);
                                if let Some(language) = language {
                                    state.language(language)
                                } else {
                                    state
                                }
                            });
                            editor.update(cx, |editor, cx| {
                                editor.set_value(content, window, cx);
                            });
                            view.code_editor = Some(editor);
                            view.pending_anchor = None;
                        }
                        SkillFileKind::Image => {}
                    },
                    Ok(None) => view.pending_anchor = None,
                    Err(error) => view.detail_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.collapsed_folders.insert(path.clone()) {
            self.collapsed_folders.remove(&path);
        }
        self.rebuild_file_list();
        cx.notify();
    }

    fn rebuild_file_list(&mut self) {
        let indices = self
            .bundle
            .as_ref()
            .map(|bundle| bundle.visible_indices(&self.collapsed_folders))
            .unwrap_or_default();
        self.file_list_state.reset(indices.len());
        self.visible_file_indices = Arc::new(indices);
    }

    fn render_list_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(skill) = self
            .visible_indices
            .get(index)
            .and_then(|index| self.skills.get(*index))
            .cloned()
        else {
            return div().into_any_element();
        };
        let selected = self.selected_path.as_ref() == Some(&skill.path);
        let path = skill.path.clone();
        v_flex()
            .id(SharedString::from(format!(
                "skill-row-{}",
                skill.path.display()
            )))
            .w_full()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .cursor_pointer()
            .when(selected, |this| this.bg(cx.theme().sidebar_accent))
            .hover(|this| this.bg(cx.theme().sidebar_accent.opacity(0.65)))
            .on_click(cx.listener(move |view, _, _, cx| view.select_skill(path.clone(), cx)))
            .child(div().truncate().text_sm().font_medium().child(skill.name))
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(skill.description),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} · {}", skill.scope.label(), skill.origin)),
            )
            .into_any_element()
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("SKILLS"),
                            )
                            .child(div().font_semibold().child(self.agent.label()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.query.trim().is_empty() {
                                        format!("{} found", self.skills.len())
                                    } else {
                                        format!(
                                            "{} of {} found",
                                            self.visible_indices.len(),
                                            self.skills.len()
                                        )
                                    }),
                            ),
                    )
                    .child(
                        Button::new("refresh-skills")
                            .xsmall()
                            .outline()
                            .label("Refresh")
                            .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        Input::new(&self.search_input)
                            .id("skills-search")
                            .aria_label("Filter skills")
                            .cleanable(true)
                            .w_full(),
                    ),
            )
            .child(
                list(
                    self.list_state.clone(),
                    cx.processor(|view, index: usize, _, cx| view.render_list_row(index, cx)),
                )
                .size_full(),
            )
            .into_any_element()
    }

    fn activate_file_entry(
        &mut self,
        path: PathBuf,
        relative: PathBuf,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if folder {
            self.toggle_folder(relative, cx);
        } else {
            self.open_file(path, None, window, cx);
        }
    }

    fn render_file_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.bundle.as_ref().and_then(|bundle| {
            self.visible_file_indices
                .get(index)
                .and_then(|index| bundle.files.get(*index))
        }) else {
            return div().into_any_element();
        };
        let path = entry.path.clone();
        let relative = entry.relative.clone();
        let folder = entry.kind.is_none();
        let expanded = folder && !self.collapsed_folders.contains(&relative);
        let selected = self.selected_file.as_ref() == Some(&path);
        let label = relative
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let icon = if folder {
            if expanded {
                IconName::FolderOpen
            } else {
                IconName::Folder
            }
        } else {
            IconName::FileText
        };
        let click_path = path.clone();
        let click_relative = relative.clone();
        h_flex()
            .id(SharedString::from(format!(
                "skill-file-{}",
                relative.display()
            )))
            .w_full()
            .min_w_0()
            .h(px(30.))
            .gap_1()
            .pl(px(8. + entry.depth as f32 * 14.))
            .pr_2()
            .role(Role::TreeItem)
            .aria_label(relative.display().to_string())
            .aria_selected(selected)
            .when(folder, |this| this.aria_expanded(expanded))
            .tab_index(0)
            .when(selected, |this| this.bg(cx.theme().sidebar_accent))
            .when(!selected, |this| {
                this.hover(|this| this.bg(cx.theme().sidebar_accent.opacity(0.65)))
            })
            .focus_visible(|style| style.border_1().border_color(cx.theme().ring))
            .on_click(cx.listener(move |view, _, window, cx| {
                view.activate_file_entry(
                    click_path.clone(),
                    click_relative.clone(),
                    folder,
                    window,
                    cx,
                );
            }))
            .on_key_down(
                cx.listener(move |view, event: &gpui::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "return" | "space") {
                        view.activate_file_entry(
                            path.clone(),
                            relative.clone(),
                            folder,
                            window,
                            cx,
                        );
                        cx.stop_propagation();
                    }
                }),
            )
            .child(div().w(px(16.)).flex_none().when(folder, |this| {
                this.child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall(),
                )
            }))
            .child(Icon::new(icon).small())
            .child(div().min_w_0().truncate().text_sm().child(label))
            .into_any_element()
    }

    fn render_file_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (count, truncated) = self
            .bundle
            .as_ref()
            .map(|bundle| (bundle.files.len(), bundle.truncated))
            .unwrap_or((0, false));
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .justify_between()
                    .h(px(44.))
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_xs().font_semibold().child("FILES"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if truncated {
                                        format!("{count}+")
                                    } else {
                                        count.to_string()
                                    }),
                            ),
                    )
                    .child(
                        Button::new("hide-skill-files")
                            .xsmall()
                            .ghost()
                            .icon(IconName::PanelRightClose)
                            .accessibility_label("Hide Files")
                            .tooltip("Hide Files")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.file_panel_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(if self.bundle.is_some() {
                div()
                    .id("skill-file-tree")
                    .role(Role::Tree)
                    .min_h_0()
                    .flex_1()
                    .child(
                        list(
                            self.file_list_state.clone(),
                            cx.processor(|view, index: usize, _, cx| {
                                view.render_file_row(index, cx)
                            }),
                        )
                        .size_full(),
                    )
                    .into_any_element()
            } else {
                div()
                    .p_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.detail_loading {
                        "Loading files…"
                    } else {
                        "Select a skill"
                    })
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(skill) = self
            .skills
            .iter()
            .find(|skill| self.selected_path.as_ref() == Some(&skill.path))
        else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .p_5()
                .child(div().font_semibold().child(if self.loading {
                    "Scanning skills"
                } else if self.query.trim().is_empty() {
                    "No skills found"
                } else {
                    "No matching skills"
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if self.loading {
                            "Reading skill metadata…"
                        } else if self.query.trim().is_empty() {
                            "Searched these local directories:"
                        } else {
                            "Try another name or description."
                        }),
                )
                .when(!self.loading && self.query.trim().is_empty(), |this| {
                    this.children(self.roots.iter().map(|root| {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.display_path(&root.path))
                    }))
                })
                .into_any_element();
        };

        let body: AnyElement = if self.detail_loading {
            v_flex()
                .gap_3()
                .p_5()
                .children((0..6).map(|_| Skeleton::new().h_5()))
                .into_any_element()
        } else if let Some(error) = &self.detail_error {
            div()
                .p_5()
                .text_color(cx.theme().danger)
                .child(error.clone())
                .into_any_element()
        } else if let (Some(bundle), Some(file), Some(markdown)) = (
            self.bundle.as_ref(),
            self.selected_file.as_ref(),
            self.markdown_view.as_ref(),
        ) {
            let image_bundle = bundle.clone();
            let image_file = file.clone();
            let link_bundle = bundle.clone();
            let link_file = file.clone();
            let weak = cx.weak_entity();
            TextView::new(markdown)
                .selectable(true)
                .scrollable(true)
                .image_source(move |uri| {
                    if uri.as_ref().starts_with("data:image/") {
                        return ImageSource::from(uri.clone());
                    }
                    match skill_catalog::resolve_skill_link(
                        &image_bundle,
                        &image_file,
                        uri.as_ref(),
                    ) {
                        SkillLink::Local { path, .. }
                            if image_bundle
                                .file(&path)
                                .is_some_and(|entry| entry.kind == Some(SkillFileKind::Image)) =>
                        {
                            ImageSource::from(path)
                        }
                        _ => ImageSource::Custom(Arc::new(|_, _| None)),
                    }
                })
                .on_link_click(move |href, _, window, cx| {
                    match skill_catalog::resolve_skill_link(&link_bundle, &link_file, href.as_ref())
                    {
                        SkillLink::Local { path, anchor } => {
                            let _ = weak
                                .update(cx, |view, cx| view.open_file(path, anchor, window, cx));
                        }
                        SkillLink::External(url) => cx.open_url(&url),
                        SkillLink::Unsupported => {
                            let _ = weak.update(cx, |view, cx| {
                                view.navigation_error =
                                    Some(format!("Cannot open link within this skill: {href}"));
                                cx.notify();
                            });
                        }
                    }
                })
                .size_full()
                .into_any_element()
        } else if let Some(editor) = &self.code_editor {
            Editor::new(editor)
                .readonly(true)
                .bordered(false)
                .aria_label("Skill source file")
                .h_full()
                .w_full()
                .into_any_element()
        } else if let (Some(bundle), Some(file)) = (&self.bundle, &self.selected_file) {
            if bundle
                .file(file)
                .is_some_and(|entry| entry.kind == Some(SkillFileKind::Image))
            {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_5()
                    .child(
                        img(file.clone())
                            .object_fit(ObjectFit::Contain)
                            .max_w_full()
                            .max_h_full(),
                    )
                    .into_any_element()
            } else {
                div().into_any_element()
            }
        } else {
            div().into_any_element()
        };

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .p_5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("SKILL DETAIL"),
                            )
                            .when(!self.file_panel_open, |this| {
                                this.child(
                                    Button::new("show-skill-files")
                                        .small()
                                        .outline()
                                        .icon(IconName::PanelRightOpen)
                                        .label("Files")
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.file_panel_open = true;
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(div().text_lg().font_semibold().child(skill.name.clone()))
                    .child(div().text_sm().child(skill.description.clone()))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {} · {}",
                                skill.scope.label(),
                                skill.origin,
                                self.display_path(&skill.path)
                            )),
                    )
                    .when_some(self.selected_file.as_ref(), |this, file| {
                        this.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    self.bundle
                                        .as_ref()
                                        .and_then(|bundle| bundle.file(file))
                                        .map(|entry| entry.relative.display().to_string())
                                        .unwrap_or_default(),
                                ),
                        )
                    })
                    .when_some(self.navigation_error.as_ref(), |this, error| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(error.clone()),
                        )
                    }),
            )
            .child(div().min_h_0().flex_1().p_5().child(body))
            .into_any_element()
    }

    fn display_path(&self, path: &std::path::Path) -> String {
        self.home
            .as_deref()
            .and_then(|home| path.strip_prefix(home).ok())
            .map(|relative| format!("~/{}", relative.display()))
            .unwrap_or_else(|| path.display().to_string())
    }
}

impl Render for SkillsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let detail: AnyElement = if self.file_panel_open {
            h_resizable("skill-file-split")
                .with_state(&self.file_resizable_state)
                .child(resizable_panel().child(self.render_detail(cx)))
                .child(
                    resizable_panel()
                        .size(px(280.))
                        .size_range(px(220.)..px(420.))
                        .child(self.render_file_panel(cx)),
                )
                .into_any_element()
        } else {
            self.render_detail(cx)
        };
        h_resizable("skills-split")
            .with_state(&self.resizable_state)
            .child(
                resizable_panel()
                    .size(px(340.))
                    .size_range(px(280.)..px(520.))
                    .child(self.render_sidebar(cx)),
            )
            .child(resizable_panel().child(detail))
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, fs, rc::Rc};

    use gpui::{px, AppContext as _, Entity, ListOffset, TestAppContext, VisualTestContext};
    use tempfile::tempdir;

    use super::{skill_catalog, SkillAgent, SkillsView};

    #[gpui::test]
    fn switching_between_populated_and_empty_agent_draws(cx: &mut TestAppContext) {
        let home = tempdir().unwrap();
        let skill = home.path().join(".claude/skills/sample");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: Sample\n---\n# Body").unwrap();

        cx.update(gpui_kit::init);
        let catalog =
            skill_catalog::discover(&skill_catalog::roots(SkillAgent::Claude, home.path(), None));
        assert_eq!(catalog.skills.len(), 1);
        let view = Rc::new(RefCell::new(None::<Entity<SkillsView>>));
        let slot = view.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let skills = cx.new(|cx| {
                let mut skills = SkillsView::empty(
                    SkillAgent::Claude,
                    None,
                    Some(home.path().to_path_buf()),
                    window,
                    cx,
                );
                skills.skills = catalog.skills;
                skills.visible_indices = vec![0];
                skills.list_state.reset(1);
                skills.selected_path = Some(skill.join("SKILL.md"));
                skills.detail = Some("# Body".into());
                skills
            });
            *slot.borrow_mut() = Some(skills.clone());
            gpui_kit::component::Root::new(skills, window, cx)
        });
        let view = view.borrow().as_ref().unwrap().clone();
        let cx: &mut VisualTestContext = cx;
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(view.read_with(cx, |view, _| view.skills.len()), 1);

        view.update(cx, |view, cx| view.set_query("sample".to_string(), cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.visible_indices, [0]);
            assert_eq!(view.selected_path.as_ref(), Some(&skill.join("SKILL.md")));
        });
        view.update(cx, |view, cx| view.set_query("missing".to_string(), cx));
        view.read_with(cx, |view, _| {
            assert!(view.visible_indices.is_empty());
            assert!(view.selected_path.is_none());
            assert_eq!(view.list_state.item_count(), 0);
        });

        view.update(cx, |view, cx| {
            view.agent = SkillAgent::ZCode;
            view.skills.clear();
            view.visible_indices.clear();
            view.list_state.reset(0);
            view.list_state.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            view.selected_path = None;
            view.detail = None;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.agent, SkillAgent::ZCode);
            assert!(view.skills.is_empty());
        });

        view.update(cx, |view, cx| {
            view.agent = SkillAgent::Claude;
            view.skills = skill_catalog::discover(&skill_catalog::roots(
                SkillAgent::Claude,
                home.path(),
                None,
            ))
            .skills;
            view.visible_indices = vec![0];
            view.list_state.reset(1);
            view.selected_path = Some(skill.join("SKILL.md"));
            view.detail = Some("# Body".into());
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(view.read_with(cx, |view, _| view.skills.len()), 1);
    }
}
