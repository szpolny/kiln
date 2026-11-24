use crate::theme;
use gpui::{
    div, prelude::*, px, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    PathPromptOptions, Render, SharedString, Window,
};
use kiln_agent::{claude, AgentEvent};
use kiln_core::policy::{capability_for_tool, Decision};
use kiln_core::persist::{self, Loaded};
use kiln_core::{Agent, Approval, Message, Role, Status, Store, NEW_THREAD_TITLE};

pub struct Workspace {
    store: Store,
    /// None disables saving (e.g. the existing file could not be read).
    state_path: Option<std::path::PathBuf>,
    selected: Option<u64>,
    draft: String,
    focus: FocusHandle,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let path = state_path();
        let (store, state_path) = match persist::load(&path) {
            Ok(Loaded::Ok(store)) => (store, Some(path)),
            Ok(Loaded::Corrupt(aside)) => {
                eprintln!("kiln: unreadable state moved to {}", aside.display());
                (Store::default(), Some(path))
            }
            Ok(_) => (Store::default(), Some(path)),
            Err(e) => {
                eprintln!("kiln: cannot read {}: {e}; saving disabled", path.display());
                (Store::default(), None)
            }
        };
        let selected = store.first_thread_id();
        Self {
            store,
            state_path,
            selected,
            draft: String::new(),
            focus: cx.focus_handle(),
        }
    }

    fn save(&self) {
        if let Some(path) = &self.state_path {
            if let Err(e) = persist::save(path, &self.store) {
                eprintln!("kiln: could not save state: {e}");
            }
        }
    }

    fn add_project(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                let p = this.store.add_project(&dir);
                if this.store.projects[p].threads.is_empty() {
                    this.new_thread(p, cx);
                } else {
                    this.save();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn new_thread(&mut self, p: usize, cx: &mut Context<Self>) {
        if let Some(id) = self.store.add_thread(p, Agent::default()) {
            self.selected = Some(id);
            self.save();
            cx.notify();
        }
    }

    fn delete_thread(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.store.delete_thread(id) {
            if self.selected == Some(id) {
                self.selected = self.store.first_thread_id();
            }
            self.save();
            cx.notify();
        }
    }

    fn remove_project(&mut self, p: usize, cx: &mut Context<Self>) {
        if self.store.remove_project(p) {
            if self.selected.is_some_and(|id| self.store.locate(id).is_none()) {
                self.selected = self.store.first_thread_id();
            }
            self.save();
            cx.notify();
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if self.selected.is_none() || ks.modifiers.platform || ks.modifiers.control {
            return;
        }
        match ks.key.as_str() {
            "enter" => self.submit(cx),
            "backspace" => {
                self.draft.pop();
            }
            _ => {
                if let Some(ch) = &ks.key_char {
                    self.draft.push_str(ch);
                }
            }
        }
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let body = self.draft.trim().to_string();
        let Some(id) = self.selected else { return };
        if body.is_empty() {
            return;
        }
        let Some(thread) = self.store.thread_mut(id) else {
            return;
        };
        if thread.status == Status::Running {
            return; // one turn at a time per thread
        }
        // A new message supersedes anything still waiting for approval.
        thread.pending.clear();
        if thread.title == NEW_THREAD_TITLE {
            thread.title = body.chars().take(40).collect();
        }
        thread.messages.push(Message { role: Role::User, body: body.clone() });
        self.draft.clear();
        self.save();
        self.start_turn(id, body, vec![], cx);
    }

    /// Run one agent turn and fold its events into the thread.
    fn start_turn(
        &mut self,
        id: u64,
        prompt: String,
        allowed_tools: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.store.project_dir_of(id) else {
            return;
        };
        let Some(thread) = self.store.thread_mut(id) else {
            return;
        };
        thread.status = Status::Running;
        let rx = claude::run(claude::Turn {
            dir,
            prompt,
            resume: thread.session_id.clone(),
            allowed_tools,
        });
        cx.notify();

        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                let done = matches!(ev, AgentEvent::Done { .. });
                let _ = this.update(cx, |this, cx| {
                    let finished = matches!(ev, AgentEvent::Done { .. });
                    let retry = this.apply(id, ev);
                    if finished {
                        this.save();
                    }
                    if let Some(rules) = retry {
                        let go = "Permission granted for the denied action. Continue.".to_string();
                        this.start_turn(id, go, rules, cx);
                    }
                    cx.notify();
                });
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    /// Returns rules to retry with when the policy pre-approved denied actions.
    fn apply(&mut self, id: u64, ev: AgentEvent) -> Option<Vec<String>> {
        let thread = self.store.thread_mut(id)?;
        let agent_msg = |body: String| Message { role: Role::Agent, body };
        match ev {
            AgentEvent::Session(id) => thread.session_id = Some(id),
            AgentEvent::Text(text) => thread.messages.push(agent_msg(text)),
            AgentEvent::Tool(name) => thread.messages.push(agent_msg(format!("⚙ {name}"))),
            AgentEvent::Denied { tool, summary, rule } => {
                match thread.policy.decide(capability_for_tool(&tool)) {
                    Decision::Allow => thread.auto_allow.push(rule),
                    Decision::Deny => thread
                        .messages
                        .push(agent_msg(format!("Blocked by policy: {summary}"))),
                    Decision::Ask => thread.pending.push(Approval { tool, summary, rule }),
                }
            }
            AgentEvent::Done { error } => {
                if let Some(e) = error {
                    thread.status = Status::NeedsInput;
                    thread.messages.push(agent_msg(format!("Error: {e}")));
                } else if !thread.auto_allow.is_empty() {
                    return Some(std::mem::take(&mut thread.auto_allow));
                } else {
                    thread.status =
                        if thread.pending.is_empty() { Status::Idle } else { Status::NeedsInput };
                }
            }
        }
        None
    }

    fn approve(&mut self, id: u64, idx: usize, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(id) else {
            return;
        };
        if idx >= thread.pending.len() {
            return;
        }
        let a = thread.pending.remove(idx);
        thread.messages.push(Message { role: Role::User, body: format!("✓ Allowed: {}", a.summary) });
        let go = "Permission granted for the denied action. Continue.".to_string();
        self.save();
        self.start_turn(id, go, vec![a.rule], cx);
    }

    fn deny(&mut self, id: u64, idx: usize, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(id) else {
            return;
        };
        if idx >= thread.pending.len() {
            return;
        }
        let a = thread.pending.remove(idx);
        thread.messages.push(Message { role: Role::User, body: format!("✗ Denied: {}", a.summary) });
        if thread.pending.is_empty() {
            thread.status = Status::Idle;
        }
        self.save();
        cx.notify();
    }

    fn approval_cards(&self, id: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap_2().px_4();
        let pending = self.store.thread(id).map(|th| th.pending.clone()).unwrap_or_default();
        for (i, a) in pending.into_iter().enumerate() {
            let button = |id: String, label: &'static str, primary: bool| {
                div().id(SharedString::from(id)).px_3().py_1().rounded_md().text_xs()
                    .cursor_pointer().border_1().border_color(theme::border())
                    .when(primary, |d| d.bg(theme::accent()).text_color(theme::bg()))
                    .when(!primary, |d| d.text_color(theme::text()).hover(|h| h.bg(theme::hover())))
                    .child(label)
            };
            col = col.child(
                div().flex().items_center().gap_3().px_3().py_2().rounded_lg()
                    .bg(theme::surface()).border_1().border_color(theme::warn())
                    .child(
                        div().flex().flex_col().flex_1().gap_0p5()
                            .child(div().text_xs().text_color(theme::warn())
                                .child(format!("{} needs approval", a.tool)))
                            .child(div().text_sm().text_color(theme::text()).child(a.summary)),
                    )
                    .child(button(format!("allow{i}"), "Allow", true)
                        .on_click(cx.listener(move |this, _, _, cx| this.approve(id, i, cx))))
                    .child(button(format!("deny{i}"), "Deny", false)
                        .on_click(cx.listener(move |this, _, _, cx| this.deny(id, i, cx)))),
            );
        }
        col
    }

    fn dot(status: Status) -> impl IntoElement {
        let c = match status {
            Status::Running => theme::accent(),
            Status::NeedsInput => theme::warn(),
            Status::Idle => theme::muted(),
        };
        div().size(px(7.)).rounded_full().bg(c)
    }

    fn small_button(id: impl Into<SharedString>, label: &'static str) -> gpui::Stateful<gpui::Div> {
        div().id(id.into()).px_1p5().rounded_md().text_xs().text_color(theme::muted())
            .cursor_pointer().hover(|h| h.bg(theme::hover()).text_color(theme::text()))
            .child(label)
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap_3().p_3().w(px(260.)).h_full()
            .bg(theme::sidebar()).border_r_1().border_color(theme::border())
            .child(
                div().flex().items_center().px_2().pt_6()
                    .child(div().flex_1().text_sm().text_color(theme::text()).child("Kiln"))
                    .child(Self::small_button("add-project", "+ Project")
                        .on_click(cx.listener(|this, _, _, cx| this.add_project(cx)))),
            );
        for (pi, p) in self.store.projects.iter().enumerate() {
            let mut group = div().flex().flex_col().gap_0p5().child(
                div().flex().items_center().px_2().py_1()
                    .child(div().flex_1().text_xs().text_color(theme::muted()).child(p.name.clone()))
                    .child(Self::small_button(format!("new-thread-{pi}"), "+")
                        .on_click(cx.listener(move |this, _, _, cx| this.new_thread(pi, cx))))
                    .child(Self::small_button(format!("rm-project-{pi}"), "×")
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_project(pi, cx)))),
            );
            for t in &p.threads {
                let id = t.id;
                let active = self.selected == Some(id);
                group = group.child(
                    div().id(SharedString::from(format!("t{id}")))
                        .flex().items_center().gap_2().px_2().py_1p5().rounded_md()
                        .text_sm().text_color(theme::text())
                        .when(active, |d| d.bg(theme::hover()))
                        .hover(|d| d.bg(theme::hover()))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.selected = Some(id);
                            window.focus(&this.focus);
                            cx.notify();
                        }))
                        .child(Self::dot(t.status))
                        .child(div().flex_1().overflow_hidden().child(t.title.clone()))
                        .child(div().text_xs().text_color(theme::muted()).child(t.agent.label()))
                        .child(Self::small_button(format!("del{id}"), "×")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.delete_thread(id, cx);
                            }))),
                );
            }
            col = col.child(group);
        }
        col
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (msg, action) = if self.store.projects.is_empty() {
            ("Add a project folder to start working with an agent.", "Add project")
        } else {
            ("Select a thread, or create one with + next to a project.", "")
        };
        div().flex().flex_col().flex_1().h_full().items_center().justify_center().gap_3()
            .child(div().text_sm().text_color(theme::muted()).child(msg))
            .when(!action.is_empty(), |d| {
                d.child(
                    div().id("empty-add").px_3().py_1p5().rounded_md().text_sm().cursor_pointer()
                        .bg(theme::accent()).text_color(theme::bg()).child(action)
                        .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                )
            })
    }

    fn thread(&self, id: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(thread) = self.store.thread(id) else {
            return self.empty_state(cx).into_any_element();
        };
        let mut msgs = div().flex().flex_col().gap_4().flex_1().p_6();
        for m in &thread.messages {
            let user = m.role == Role::User;
            msgs = msgs.child(
                div().flex().when(user, |d| d.justify_end()).child(
                    div().max_w(px(560.)).px_3().py_2().rounded_lg().text_sm()
                        .text_color(theme::text())
                        .when(user, |d| d.bg(theme::surface()))
                        .child(m.body.clone()),
                ),
            );
        }
        let composer_text = if self.draft.is_empty() {
            div().text_color(theme::muted()).child("Ask anything…")
        } else {
            div().text_color(theme::text()).child(format!("{}▏", self.draft))
        };
        let badge = if thread.policy.is_local_only() { "● local only" } else { "○ network allowed" };
        div().flex().flex_col().flex_1().h_full()
            .child(
                div().flex().items_center().gap_2().px_4().h(px(44.))
                    .border_b_1().border_color(theme::border())
                    .text_sm().text_color(theme::text()).child(thread.title.clone())
                    .child(div().text_xs().text_color(theme::ok()).child(badge)),
            )
            .child(msgs)
            .child(self.approval_cards(id, cx))
            .child(
                div().p_4().child(
                    div().id("composer").track_focus(&self.focus)
                        .on_key_down(cx.listener(|this, ev, _, cx| this.on_key(ev, cx)))
                        .on_click(cx.listener(|this, _, window, _| window.focus(&this.focus)))
                        .px_4().py_3().rounded_xl().bg(theme::surface())
                        .border_1().border_color(theme::border())
                        .text_sm().child(composer_text),
                ),
            )
            .into_any_element()
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus.is_focused(window) {
            window.focus(&self.focus);
        }
        div().flex().size_full().bg(theme::bg()).text_color(theme::text())
            .child(self.sidebar(cx))
            .child(match self.selected {
                Some(id) => self.thread(id, cx),
                None => self.empty_state(cx).into_any_element(),
            })
    }
}

/// `KILN_STATE_DIR` overrides the location so dev and test runs never touch real data.
fn state_path() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Some(dir) = std::env::var_os("KILN_STATE_DIR") {
        return PathBuf::from(dir).join("state.json");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Kiln")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(|d| PathBuf::from(d).join("kiln"))
            .unwrap_or_else(|| home.join(".local/share/kiln"))
    };
    base.join("state.json")
}
