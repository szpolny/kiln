use crate::theme;
use gpui::{
    div, prelude::*, px, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render,
    SharedString, Window,
};
use kiln_agent::{claude, AgentEvent};
use kiln_core::policy::{capability_for_tool, Decision};
use kiln_core::persist::{self, Loaded};
use kiln_core::{Approval, Message, Role, Status, Store};

pub struct Workspace {
    store: Store,
    /// None disables saving (e.g. the existing file could not be read).
    state_path: Option<std::path::PathBuf>,
    selected: (usize, usize),
    draft: String,
    focus: FocusHandle,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let path = state_path();
        let (store, state_path) = match persist::load(&path) {
            Ok(Loaded::Ok(store)) if !store.projects.is_empty() => (store, Some(path)),
            Ok(Loaded::Corrupt(aside)) => {
                eprintln!("kiln: unreadable state moved to {}", aside.display());
                (Store::demo(), Some(path))
            }
            Ok(_) => (Store::demo(), Some(path)),
            Err(e) => {
                eprintln!("kiln: cannot read {}: {e}; saving disabled", path.display());
                (Store::demo(), None)
            }
        };
        Self {
            store,
            state_path,
            selected: (0, 0),
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

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.platform || ks.modifiers.control {
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
        let (p, t) = self.selected;
        if body.is_empty() {
            return;
        }
        let Some(thread) = self.store.thread_mut(p, t) else {
            return;
        };
        if thread.status == Status::Running {
            return; // one turn at a time per thread
        }
        // A new message supersedes anything still waiting for approval.
        thread.pending.clear();
        thread.messages.push(Message { role: Role::User, body: body.clone() });
        self.draft.clear();
        self.save();
        self.start_turn(p, t, body, vec![], cx);
    }

    /// Run one agent turn and fold its events into the thread.
    fn start_turn(
        &mut self,
        p: usize,
        t: usize,
        prompt: String,
        allowed_tools: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.store.projects.get(p).map(|pr| pr.dir.clone()) else {
            return;
        };
        let Some(thread) = self.store.thread_mut(p, t) else {
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
                    let retry = this.apply(p, t, ev);
                    if finished {
                        this.save();
                    }
                    if let Some(rules) = retry {
                        let go = "Permission granted for the denied action. Continue.".to_string();
                        this.start_turn(p, t, go, rules, cx);
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
    fn apply(&mut self, p: usize, t: usize, ev: AgentEvent) -> Option<Vec<String>> {
        let thread = self.store.thread_mut(p, t)?;
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

    fn approve(&mut self, p: usize, t: usize, idx: usize, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(p, t) else {
            return;
        };
        if idx >= thread.pending.len() {
            return;
        }
        let a = thread.pending.remove(idx);
        thread.messages.push(Message { role: Role::User, body: format!("✓ Allowed: {}", a.summary) });
        let go = "Permission granted for the denied action. Continue.".to_string();
        self.save();
        self.start_turn(p, t, go, vec![a.rule], cx);
    }

    fn deny(&mut self, p: usize, t: usize, idx: usize, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(p, t) else {
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

    fn approval_cards(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (p, t) = self.selected;
        let mut col = div().flex().flex_col().gap_2().px_4();
        let pending = self.store.thread(p, t).map(|th| th.pending.clone()).unwrap_or_default();
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
                        .on_click(cx.listener(move |this, _, _, cx| this.approve(p, t, i, cx))))
                    .child(button(format!("deny{i}"), "Deny", false)
                        .on_click(cx.listener(move |this, _, _, cx| this.deny(p, t, i, cx)))),
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

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap_3().p_3().w(px(260.)).h_full()
            .bg(theme::sidebar()).border_r_1().border_color(theme::border())
            .child(div().px_2().pt_6().text_sm().text_color(theme::text()).child("Kiln"));
        for (pi, p) in self.store.projects.iter().enumerate() {
            let mut group = div().flex().flex_col().gap_0p5().child(
                div().px_2().py_1().text_xs().text_color(theme::muted()).child(p.name.clone()),
            );
            for (ti, t) in p.threads.iter().enumerate() {
                let active = self.selected == (pi, ti);
                group = group.child(
                    div().id(SharedString::from(format!("t{pi}-{ti}")))
                        .flex().items_center().gap_2().px_2().py_1p5().rounded_md()
                        .text_sm().text_color(theme::text())
                        .when(active, |d| d.bg(theme::hover()))
                        .hover(|d| d.bg(theme::hover()))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.selected = (pi, ti);
                            window.focus(&this.focus);
                            cx.notify();
                        }))
                        .child(Self::dot(t.status))
                        .child(div().flex_1().child(t.title.clone()))
                        .child(div().text_xs().text_color(theme::muted()).child(t.agent.clone())),
                );
            }
            col = col.child(group);
        }
        col
    }

    fn thread(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (p, t) = self.selected;
        let thread = self.store.thread(p, t).expect("selected thread exists");
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
            .child(self.approval_cards(cx))
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
            .child(self.thread(cx))
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
