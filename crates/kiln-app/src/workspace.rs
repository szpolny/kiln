use crate::theme;
use gpui::{
    div, list, prelude::*, px, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    ListAlignment, ListState, PathPromptOptions, Render, SharedString, Window,
};
use kiln_agent::{claude, AgentEvent};
use kiln_core::policy::{capability_for_tool, Decision};
use kiln_core::persist::{self, Loaded};
use kiln_core::{Agent, Approval, Message, Role, Status, Store, Thread, NEW_THREAD_TITLE};

const MONO: &str = if cfg!(target_os = "macos") { "Menlo" } else { "DejaVu Sans Mono" };

pub struct Workspace {
    store: Store,
    /// None disables saving (e.g. the existing file could not be read).
    state_path: Option<std::path::PathBuf>,
    selected: Option<u64>,
    draft: String,
    focus: FocusHandle,
    /// Virtualized message list. It follows new output until the user scrolls up.
    transcript: ListState,
    /// Thread `transcript` was built for; `None` forces a jump to the latest message.
    transcript_thread: Option<u64>,
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
            transcript: ListState::new(0, ListAlignment::Bottom, px(400.)),
            transcript_thread: None,
        }
    }

    fn save(&self) {
        if let Some(path) = &self.state_path {
            if let Err(e) = persist::save(path, &self.store) {
                eprintln!("kiln: could not save state: {e}");
            }
        }
    }

    /// Tell the list about messages added since the last frame.
    fn sync_transcript(&mut self) {
        let count = self
            .selected
            .and_then(|id| self.store.thread(id))
            .map_or(0, |t| t.messages.len());
        let known = self.transcript.item_count();
        if self.transcript_thread != self.selected || count < known {
            self.transcript.reset(count);
            self.transcript_thread = self.selected;
        } else if count > known {
            self.transcript.splice(known..known, count - known);
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
        thread.messages.push(Message::user(body.clone()));
        self.draft.clear();
        self.transcript_thread = None; // show what was just sent
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
            model: thread.model.clone(),
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
        match ev {
            AgentEvent::Session(id) => thread.session_id = Some(id),
            AgentEvent::Model(model) => thread.reported_model = Some(model),
            AgentEvent::Text(text) => thread.messages.push(Message::agent(text)),
            AgentEvent::Tool { name, input } => {
                thread.messages.push(Message::tool_call(name, input))
            }
            AgentEvent::Denied { tool, summary, rule } => {
                match thread.policy.decide(capability_for_tool(&tool)) {
                    Decision::Allow => thread.auto_allow.push(rule),
                    Decision::Deny => thread.messages.push(Message::agent(format!(
                        "Blocked by policy: {tool}\n{summary}"
                    ))),
                    Decision::Ask => thread.pending.push(Approval { tool, summary, rule }),
                }
            }
            AgentEvent::Done { error } => {
                if let Some(e) = error {
                    thread.status = Status::NeedsInput;
                    thread.messages.push(Message::agent(format!("Error: {e}")));
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
        let note = format!("✓ Allowed: {}\n{}", a.tool, a.summary);
        thread.messages.push(Message::user(note));
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
        let note = format!("✗ Denied: {}\n{}", a.tool, a.summary);
        thread.messages.push(Message::user(note));
        if thread.pending.is_empty() {
            thread.status = Status::Idle;
        }
        self.save();
        cx.notify();
    }

    fn approval_cards(&self, id: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().id("approvals").flex().flex_col().flex_shrink_0().gap_2().px_4()
            .max_h(px(240.)).overflow_y_scroll();
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
                div().flex().flex_shrink_0().items_start().gap_3().px_3().py_2().rounded_lg()
                    .bg(theme::surface()).border_1().border_color(theme::warn())
                    .child(
                        div().flex().flex_col().flex_1().min_w_0().gap_0p5()
                            .child(div().text_xs().text_color(theme::warn())
                                .child(format!("{} needs approval", a.tool)))
                            .child(div().font_family(MONO).text_xs().text_color(theme::text())
                                .child(a.summary)),
                    )
                    .child(button(format!("allow{i}"), "Allow", true)
                        .on_click(cx.listener(move |this, _, _, cx| this.approve(id, i, cx))))
                    .child(button(format!("deny{i}"), "Deny", false)
                        .on_click(cx.listener(move |this, _, _, cx| this.deny(id, i, cx)))),
            );
        }
        col
    }

    fn set_model(&mut self, id: u64, model: Option<&str>, cx: &mut Context<Self>) {
        if let Some(thread) = self.store.thread_mut(id) {
            thread.model = model.map(str::to_string);
            self.save();
            cx.notify();
        }
    }

    /// One button per model; the choice applies from the next turn.
    fn model_chooser(&self, thread: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let id = thread.id;
        let mut row = div().flex().flex_wrap().items_center().gap_1().px_1().pt_2()
            .child(div().pr_1().text_xs().text_color(theme::muted()).child("Model"));
        let choices = std::iter::once(None).chain(thread.agent.models().iter().copied().map(Some));
        for choice in choices {
            let label = choice.unwrap_or("default");
            let active = thread.model.as_deref() == choice;
            row = row.child(
                div().id(SharedString::from(format!("model-{label}")))
                    .px_2().py_0p5().rounded_md().text_xs().cursor_pointer()
                    .border_1()
                    .border_color(if active { theme::accent() } else { theme::border() })
                    .text_color(if active { theme::text() } else { theme::muted() })
                    .hover(|h| h.bg(theme::hover()))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_model(id, choice, cx))),
            );
        }
        row.when_some(thread.reported_model.clone(), |row, model| {
            row.child(div().flex_1()).child(
                div().text_xs().text_color(theme::muted()).child(format!("last turn ran {model}")),
            )
        })
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
        let header = div().flex().flex_shrink_0().items_center().px_2().pt_6()
            .child(div().flex_1().text_sm().text_color(theme::text()).child("Kiln"))
            .child(Self::small_button("add-project", "+ Project")
                .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))));
        let mut projects = div().id("projects").flex().flex_col().flex_1().min_h_0().gap_3()
            .overflow_y_scroll();
        for (pi, p) in self.store.projects.iter().enumerate() {
            let mut group = div().flex().flex_col().flex_shrink_0().gap_0p5().child(
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
            projects = projects.child(group);
        }
        div().flex().flex_col().flex_shrink_0().gap_3().p_3().w(px(260.)).h_full()
            .bg(theme::sidebar()).border_r_1().border_color(theme::border())
            .child(header)
            .child(projects)
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

    fn message(&self, id: u64, ix: usize) -> gpui::AnyElement {
        let Some(m) = self.store.thread(id).and_then(|t| t.messages.get(ix)) else {
            return div().into_any_element();
        };
        if let Some(name) = &m.tool {
            return Self::tool_call(name, &m.body).into_any_element();
        }
        let user = m.role == Role::User;
        div().flex().w_full().px_6().py_2().when(user, |d| d.justify_end())
            .child(
                div().max_w(px(560.)).min_w_0().px_3().py_2().rounded_lg().text_sm()
                    .text_color(theme::text())
                    .when(user, |d| d.bg(theme::surface()))
                    .child(m.body.clone()),
            )
            .into_any_element()
    }

    /// The exact tool name and its input, as the agent sent them.
    fn tool_call(name: &str, input: &str) -> impl IntoElement {
        div().w_full().px_6().py_1().child(
            div().flex().flex_col().w_full().min_w_0().gap_1().px_3().py_2().rounded_md()
                .border_1().border_color(theme::border())
                .child(div().font_family(MONO).text_xs().text_color(theme::accent())
                    .child(name.to_string()))
                .when(!input.is_empty(), |d| {
                    d.child(div().font_family(MONO).text_xs().text_color(theme::text())
                        .child(input.to_string()))
                }),
        )
    }

    fn thread(&self, id: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(thread) = self.store.thread(id) else {
            return self.empty_state(cx).into_any_element();
        };
        let composer_text = if self.draft.is_empty() {
            div().text_color(theme::muted()).child("Ask anything…")
        } else {
            div().text_color(theme::text()).child(format!("{}▏", self.draft))
        };
        let badge = if thread.policy.is_local_only() { "● local only" } else { "○ network allowed" };
        let this = cx.entity();
        let messages = list(self.transcript.clone(), move |ix, _, cx| {
            this.read(cx).message(id, ix)
        })
        .flex_1()
        .min_h_0()
        .py_3();
        div().flex().flex_col().flex_1().min_w_0().h_full()
            .child(
                div().flex().flex_shrink_0().items_center().gap_2().px_4().h(px(44.))
                    .border_b_1().border_color(theme::border())
                    .text_sm().text_color(theme::text()).child(thread.title.clone())
                    .child(div().text_xs().text_color(theme::ok()).child(badge)),
            )
            .child(messages)
            .child(self.approval_cards(id, cx))
            .child(
                div().flex_shrink_0().p_4().child(
                    div().id("composer").track_focus(&self.focus)
                        .on_key_down(cx.listener(|this, ev, _, cx| this.on_key(ev, cx)))
                        .on_click(cx.listener(|this, _, window, _| window.focus(&this.focus)))
                        .px_4().py_3().rounded_xl().bg(theme::surface())
                        .border_1().border_color(theme::border())
                        .text_sm().child(composer_text),
                )
                .child(self.model_chooser(thread, cx)),
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
        self.sync_transcript();
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
