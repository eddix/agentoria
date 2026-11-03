use agentoria_core::{
    index::CancellationToken,
    trae_cn::{
        self,
        credentials::{key_path, SessionKey},
    },
};
use gpui::{
    div, AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Task, Window,
};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputContentType, InputEvent, InputState},
    v_flex, ActiveTheme as _, Disableable as _, StyledExt as _,
};
use std::path::PathBuf;

pub struct KeySaved;

pub struct TraeKeyView {
    input: Entity<InputState>,
    _subscription: Subscription,
    paths: Option<(PathBuf, PathBuf)>,
    error: Option<String>,
    pending: bool,
    cancellation: CancellationToken,
    task: Option<Task<()>>,
}

impl EventEmitter<KeySaved> for TraeKeyView {}

impl TraeKeyView {
    pub fn new(message: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paths = key_path()
            .and_then(|path| trae_cn::root().map(|root| (path, root)))
            .ok();
        Self::with_paths(paths, message, window, cx)
    }

    fn with_paths(
        paths: Option<(PathBuf, PathBuf)>,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("64 hexadecimal characters"));
        input.update(cx, |input, cx| input.set_masked(true, window, cx));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |view, _, event: &InputEvent, window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false
                    }
                ) {
                    view.submit(window, cx);
                }
            });
        Self {
            input,
            _subscription: subscription,
            paths,
            error,
            pending: false,
            cancellation: CancellationToken::default(),
            task: None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let key = match SessionKey::parse(&self.input.read(cx).value()) {
            Ok(key) => key,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let Some((path, root)) = self.paths.clone() else {
            self.error = Some("Could not locate the Agentoria configuration directory".into());
            cx.notify();
            return;
        };
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.pending = true;
        self.error = None;
        let cancellation = self.cancellation.clone();
        let validate = cx.background_spawn(async move {
            let result = trae_cn::validate_key_at(&root, &key).and_then(|()| {
                anyhow::ensure!(!cancellation.is_cancelled(), "Key setup cancelled");
                key.save_at(&path)
            });
            result.map_err(|error| error.to_string())
        });
        self.task = Some(cx.spawn(async move |weak, cx| {
            let result = validate.await;
            let _ = weak.update(cx, |view, cx| {
                view.pending = false;
                match result {
                    Ok(()) => cx.emit(KeySaved),
                    Err(error) => view.error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Drop for TraeKeyView {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Render for TraeKeyView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path = self
            .paths
            .as_ref()
            .map(|(path, _)| path.display().to_string())
            .unwrap_or_else(|| "~/.config/agentoria/trae-cn.json".into());
        v_flex()
            .size_full()
            .min_h_0()
            .items_center()
            .justify_center()
            .p_5()
            .child(
                v_flex()
                    .w_full()
                    .max_w(gpui::px(620.))
                    .gap_4()
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .child("Unlock Trae CN sessions"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Enter this installation’s session key to read its local history.",
                            ),
                    )
                    .child(
                        Input::new(&self.input)
                            .id("trae-session-key")
                            .aria_label("Trae CN session key")
                            .content_type(InputContentType::Password)
                            .disabled(self.pending)
                            .w_full(),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Saved locally for next time: {path}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                            "The database is opened read-only. Each user supplies their own key.",
                        ),
                    )
                    .children(self.error.as_ref().map(|error| {
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error.clone())
                    }))
                    .child(
                        Button::new("save-trae-session-key")
                            .primary()
                            .label(if self.pending {
                                "Validating…"
                            } else {
                                "Unlock and save"
                            })
                            .disabled(self.pending)
                            .on_click(cx.listener(|view, _, window, cx| view.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use rusqlite::Connection;
    use std::{cell::RefCell, rc::Rc};

    #[gpui::test]
    fn setup_validates_before_saving_preserves_old_key_on_failure_and_accepts_enter(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        std::fs::create_dir_all(root.join("ai-agent")).unwrap();
        let valid = "1a".repeat(32);
        let database = Connection::open(root.join("ai-agent/database.db")).unwrap();
        database
            .pragma_update(None, "key", format!("x'{valid}'"))
            .unwrap();
        database
            .execute_batch("CREATE TABLE fixture(value TEXT);")
            .unwrap();
        drop(database);
        let path = directory.path().join("config/trae-cn.json");
        cx.update(gpui_kit::init);
        let slot = Rc::new(RefCell::new(None::<Entity<TraeKeyView>>));
        let target = slot.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view =
                cx.new(|cx| TraeKeyView::with_paths(Some((path.clone(), root)), None, window, cx));
            *target.borrow_mut() = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let cx: &mut VisualTestContext = cx;
        let view = slot.borrow().as_ref().unwrap().clone();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_input("not-a-key");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!path.exists());
        assert!(view.read_with(cx, |view, _| view.error.is_some()));
        SessionKey::parse(&"3c".repeat(32))
            .unwrap()
            .save_at(&path)
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        view.update_in(cx, |view, window, cx| {
            view.input
                .update(cx, |input, cx| input.set_value("2b".repeat(32), window, cx))
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(view.read_with(cx, |view, _| !view.pending && view.error.is_some()));
        view.update_in(cx, |view, window, cx| {
            view.input
                .update(cx, |input, cx| input.set_value(valid.clone(), window, cx))
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| !view.pending && view.error.is_none()));
        assert_ne!(std::fs::read(&path).unwrap(), before);
        assert!(SessionKey::load_at(&path).is_ok());
        view.read_with(cx, |view, cx| {
            assert!(view.input.read(cx).value().is_empty())
        });
    }
}
