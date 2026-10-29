use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, list, px, AnyElement, AppContext as _, Context, Entity, InteractiveElement as _,
    IntoElement, ListAlignment, ListOffset, ListState, ParentElement as _, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState, Input, InputEvent, InputState},
    resizable::{h_resizable, resizable_panel, ResizableState},
    v_flex, ActiveTheme as _, Sizable as _, StyledExt as _,
};

use crate::config_catalog::{self, ConfigAgent, ConfigKind, ConfigRoots, ConfigScan};

pub struct ConfigView {
    kind: ConfigKind,
    agent: ConfigAgent,
    roots: Option<ConfigRoots>,
    project: Option<PathBuf>,
    scan: ConfigScan,
    visible: Vec<usize>,
    selected_id: Option<String>,
    query: String,
    search_input: Entity<InputState>,
    _search_subscription: Subscription,
    editor: Option<Entity<EditorState>>,
    list_state: ListState,
    resizable: Entity<ResizableState>,
    loading: bool,
    show_errors: bool,
    revision: u64,
    task: Option<Task<()>>,
}

impl ConfigView {
    pub fn new(
        kind: ConfigKind,
        agent: ConfigAgent,
        project: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let roots = dirs::home_dir().map(ConfigRoots::from_environment);
        let mut view = Self::empty(kind, agent, project, roots, window, cx);
        view.refresh(window, cx);
        view
    }

    fn empty(
        kind: ConfigKind,
        agent: ConfigAgent,
        project: Option<PathBuf>,
        roots: Option<ConfigRoots>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(format!("Filter {}", kind.noun())));
        let subscription = cx.subscribe_in(
            &search_input,
            window,
            |view, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    view.query = input.read(cx).value().to_string();
                    view.filter(window, cx);
                }
            },
        );
        Self {
            kind,
            agent,
            roots,
            project,
            scan: ConfigScan::default(),
            visible: Vec::new(),
            selected_id: None,
            query: String::new(),
            search_input,
            _search_subscription: subscription,
            editor: None,
            list_state: ListState::new(0, ListAlignment::Top, px(500.))
                .with_uniform_item_height(px(78.)),
            resizable: cx.new(|_| ResizableState::default()),
            loading: false,
            show_errors: false,
            revision: 0,
            task: None,
        }
    }

    pub fn agent(&self) -> ConfigAgent {
        self.agent
    }

    pub fn status(&self) -> (String, usize) {
        let status = if self.loading {
            format!("Scanning {} {}", self.agent.label(), self.kind.noun())
        } else {
            format!(
                "{} of {} {} · {} read errors{}",
                self.visible.len(),
                self.scan.entries.len(),
                self.kind.noun(),
                self.scan.errors.len(),
                if self.scan.truncated {
                    " · catalog limit reached"
                } else {
                    ""
                }
            )
        };
        (status, self.visible.len())
    }

    pub fn set_agent(&mut self, agent: ConfigAgent, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent == agent {
            return;
        }
        self.agent = agent;
        self.query.clear();
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.scan = ConfigScan::default();
        self.visible.clear();
        self.list_state.reset(0);
        self.selected_id = None;
        self.editor = None;
        self.refresh(window, cx);
    }

    pub fn set_project(
        &mut self,
        project: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project != project {
            self.project = project;
            self.scan = ConfigScan::default();
            self.visible.clear();
            self.list_state.reset(0);
            self.selected_id = None;
            self.editor = None;
            self.refresh(window, cx);
        }
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.query.is_empty() {
            return false;
        }
        self.query.clear();
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.filter(window, cx);
        true
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let Some(roots) = self.roots.clone() else {
            self.scan.errors = vec!["Could not locate the home directory".into()];
            self.loading = false;
            cx.notify();
            return;
        };
        let kind = self.kind;
        let agent = self.agent;
        let project = self.project.clone();
        self.loading = true;
        let scan = cx.background_spawn(async move {
            config_catalog::discover(kind, agent, &roots, project.as_deref())
        });
        self.task = Some(cx.spawn_in(window, async move |weak, cx| {
            let scan = scan.await;
            let _ = weak.update_in(cx, |view, window, cx| {
                if view.revision != revision {
                    return;
                }
                view.loading = false;
                view.scan = scan;
                // A refresh may retain the same identity with changed content.
                view.editor = None;
                view.filter(window, cx);
            });
        }));
        cx.notify();
    }

    fn filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = self
            .scan
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.matches(&self.query))
            .map(|(index, _)| index)
            .collect();
        self.list_state.reset(self.visible.len());
        self.list_state.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        let selected = self
            .selected_id
            .as_ref()
            .and_then(|id| {
                self.visible
                    .iter()
                    .copied()
                    .find(|index| self.scan.entries[*index].id == *id)
            })
            .or_else(|| self.visible.first().copied());
        if let Some(index) = selected {
            self.select(index, window, cx);
        } else {
            self.selected_id = None;
            self.editor = None;
        }
        cx.notify();
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.scan.entries.get(index) else {
            return;
        };
        if self.selected_id.as_ref() == Some(&entry.id) && self.editor.is_some() {
            return;
        }
        self.selected_id = Some(entry.id.clone());
        let content = entry.detail.clone();
        self.editor = Some(cx.new(|cx| {
            let mut editor = EditorState::new(window, cx).language("json");
            editor.set_value(content.as_ref(), window, cx);
            editor
        }));
        cx.notify();
    }

    fn display_path(&self, path: &Path) -> String {
        self.roots
            .as_ref()
            .and_then(|roots| path.strip_prefix(&roots.home).ok())
            .map(|path| format!("~/{}", path.display()))
            .unwrap_or_else(|| path.display().to_string())
    }

    fn render_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(&entry_index) = self.visible.get(index) else {
            return div().into_any_element();
        };
        let entry = &self.scan.entries[entry_index];
        let selected = self.selected_id.as_ref() == Some(&entry.id);
        v_flex()
            .id(SharedString::from(entry.id.clone()))
            .w_full()
            .h(px(78.))
            .min_w_0()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .cursor_pointer()
            .role(Role::ListItem)
            .aria_label(format!(
                "{} · {} · {}",
                entry.name, entry.scope, entry.origin
            ))
            .aria_selected(selected)
            .tab_index(0)
            .when(selected, |this| this.bg(cx.theme().sidebar_accent))
            .when(!selected, |this| {
                this.hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.65)))
            })
            .focus_visible(|style| style.border_1().border_color(cx.theme().ring))
            .on_click(cx.listener(move |view, _, window, cx| view.select(entry_index, window, cx)))
            .on_key_down(
                cx.listener(move |view, event: &gpui::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "return" | "space") {
                        view.select(entry_index, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .font_medium()
                    .child(entry.name.clone()),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(entry.summary.clone()),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} · {}", entry.scope, entry.origin)),
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
                                    .child(self.kind.label().to_uppercase()),
                            )
                            .child(div().font_semibold().child(self.agent.label()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{} of {} found",
                                        self.visible.len(),
                                        self.scan.entries.len()
                                    )),
                            ),
                    )
                    .child(
                        Button::new("refresh-config")
                            .xsmall()
                            .outline()
                            .label("Refresh")
                            .on_click(cx.listener(|view, _, window, cx| view.refresh(window, cx))),
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
                            .id("config-search")
                            .aria_label(format!("Filter {}", self.kind.noun()))
                            .cleanable(true)
                            .w_full(),
                    ),
            )
            .child(
                list(
                    self.list_state.clone(),
                    cx.processor(|view, index: usize, _, cx| view.render_row(index, cx)),
                )
                .size_full(),
            )
            .into_any_element()
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let entry = self
            .scan
            .entries
            .iter()
            .find(|entry| self.selected_id.as_ref() == Some(&entry.id));
        let header = v_flex()
            .w_full()
            .flex_shrink_0()
            .min_w_0()
            .gap_2()
            .p_5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("CONFIGURATION · READ ONLY"),
            )
            .child(
                div().text_lg().font_semibold().child(
                    entry
                        .map(|entry| entry.name.clone())
                        .unwrap_or_else(|| self.kind.label().into()),
                ),
            )
            .when_some(entry, |this, entry| {
                this.child(div().text_sm().child(entry.summary.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {} · {}",
                                entry.scope,
                                entry.origin,
                                self.display_path(&entry.path)
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(entry.location.clone()),
                    )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        self.project
                            .as_ref()
                            .map(|path| format!("Project: {}", self.display_path(path)))
                            .unwrap_or_else(|| {
                                "User configuration · select a session to include its project"
                                    .into()
                            }),
                    ),
            );
        let errors = v_flex()
            .flex_shrink_0()
            .px_5()
            .py_2()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("config-read-errors")
                    .xsmall()
                    .ghost()
                    .label(format!(
                        "{} read errors{}",
                        self.scan.errors.len(),
                        if self.scan.truncated {
                            " · catalog limit reached"
                        } else {
                            ""
                        }
                    ))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.show_errors = !view.show_errors;
                        cx.notify();
                    })),
            )
            .when(self.show_errors, |this| {
                this.child(
                    v_flex()
                        .id("config-errors")
                        .max_h(px(160.))
                        .overflow_y_scroll()
                        .children(self.scan.errors.iter().take(20).map(|error| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(error.clone())
                        })),
                )
            });
        let body = if let Some(editor) = &self.editor {
            Editor::new(editor)
                .readonly(true)
                .bordered(false)
                .aria_label("Configuration definition")
                .size_full()
                .into_any_element()
        } else {
            v_flex()
                .id("config-empty")
                .size_full()
                .min_h_0()
                .overflow_y_scroll()
                .p_5()
                .gap_3()
                .child(div().font_semibold().child(if self.loading {
                    "Scanning configuration"
                } else if self.query.trim().is_empty() {
                    "No definitions found"
                } else {
                    "No matching definitions"
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if self.query.trim().is_empty() {
                            "Searched these local paths:"
                        } else {
                            "Try another name, event, matcher, or source."
                        }),
                )
                .when(self.query.trim().is_empty(), |this| {
                    this.children(self.scan.paths.iter().take(24).map(|path| {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.display_path(path))
                    }))
                })
                .into_any_element()
        };
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(header)
            .when(
                !self.scan.errors.is_empty() || self.scan.truncated,
                |this| this.child(errors),
            )
            .child(body)
            .into_any_element()
    }
}

impl Render for ConfigView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let split = h_resizable("configuration-split")
            .with_state(&self.resizable)
            .child(
                resizable_panel()
                    .size(px(340.))
                    .size_range(px(280.)..px(520.))
                    .child(self.render_sidebar(cx)),
            )
            .child(resizable_panel().child(self.render_detail(cx)));
        div()
            .size_full()
            .capture_action(cx.listener(
                |view, _: &gpui_kit::component::input::Search, window, cx| {
                    view.focus_search(window, cx);
                    cx.stop_propagation();
                },
            ))
            .capture_action(cx.listener(
                |view, _: &gpui_kit::component::input::Escape, window, cx| {
                    if view.clear_search(window, cx) {
                        cx.stop_propagation();
                    }
                },
            ))
            .child(split)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use std::{cell::RefCell, fs, rc::Rc};
    use tempfile::tempdir;

    #[gpui::test]
    fn filtering_refreshing_and_switching_agents_clear_stale_details(cx: &mut TestAppContext) {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        fs::create_dir_all(&roots.codex).unwrap();
        let path = roots.codex.join("config.toml");
        fs::write(&path, "[mcp_servers.docs]\ncommand='first'\n").unwrap();
        cx.update(gpui_kit::init);
        let slot = Rc::new(RefCell::new(None::<Entity<ConfigView>>));
        let target = slot.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                ConfigView::empty(
                    ConfigKind::Mcp,
                    ConfigAgent::Codex,
                    None,
                    Some(roots),
                    window,
                    cx,
                )
            });
            *target.borrow_mut() = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let cx: &mut VisualTestContext = cx;
        let view = slot.borrow().as_ref().unwrap().clone();
        view.update_in(cx, |view, window, cx| view.refresh(window, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.loading);
            assert_eq!(view.visible.len(), 1);
            assert!(view.editor.is_some());
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let editor = view.read_with(cx, |view, _| view.editor.as_ref().unwrap().clone());
        let original = editor.read_with(cx, |editor, _| editor.value());
        editor.update_in(cx, |editor, window, cx| editor.focus(window, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_input("cannot-edit");
        assert_eq!(editor.read_with(cx, |editor, _| editor.value()), original);
        cx.simulate_keystrokes("cmd-f");
        cx.simulate_input("docs");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.query.clone()), "docs");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.query.is_empty()));
        view.update_in(cx, |view, window, cx| {
            view.query = "missing".into();
            view.filter(window, cx);
            assert!(view.editor.is_none());
            assert!(view.selected_id.is_none());
            assert_eq!(view.list_state.item_count(), 0);
            assert!(view.clear_search(window, cx));
        });
        fs::write(&path, "[mcp_servers.docs]\ncommand='changed'\n").unwrap();
        view.update_in(cx, |view, window, cx| view.refresh(window, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.scan.entries[0].detail.contains("changed"))
        });
        view.update_in(cx, |view, window, cx| {
            view.set_agent(ConfigAgent::Claude, window, cx);
            assert!(view.editor.is_none());
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        view.read_with(cx, |view, _| {
            assert!(view.scan.entries.is_empty());
            assert!(view.selected_id.is_none());
            assert!(view.query.is_empty());
        });
        assert!(fs::read_to_string(path).unwrap().contains("changed"));
    }
}
