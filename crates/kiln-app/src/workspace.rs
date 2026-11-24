use crate::theme;
use gpui::{
    div, prelude::*, px, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render,
    SharedString, Window,
};
use kiln_agent::{claude, AgentEvent};
use kiln_core::{Message, Role, Status, Store};

pub struct Workspace {
    store: Store,
    selected: (usize, usize),
    draft: String,
    focus: FocusHandle,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            store: Store::demo(),
            selected: (0, 0),
            draft: String::new(),
            focus: cx.focus_handle(),
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
        let Some(project_dir) = self.store.projects.get(p).map(|pr| pr.dir.clone()) else {
            return;
        };
        let Some(thread) = self.store.thread_mut(p, t) else {
            return;
        };
        if thread.status == Status::Running {
            return; // one turn at a time per thread
        }
        thread.messages.push(Message { role: Role::User, body: body.clone() });
        thread.status = Status::Running;
        let rx = claude::run(claude::Turn {
            dir: project_dir,
            prompt: body,
            resume: thread.session_id.clone(),
        });
        self.draft.clear();

        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                let done = matches!(ev, AgentEvent::Done { .. });
                let _ = this.update(cx, |this, cx| {
                    this.apply(p, t, ev);
                    cx.notify();
                });
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    fn apply(&mut self, p: usize, t: usize, ev: AgentEvent) {
        let Some(thread) = self.store.thread_mut(p, t) else {
            return;
        };
        let agent_msg = |body: String| Message { role: Role::Agent, body };
        match ev {
            AgentEvent::Session(id) => thread.session_id = Some(id),
            AgentEvent::Text(text) => thread.messages.push(agent_msg(text)),
            AgentEvent::Tool(name) => thread.messages.push(agent_msg(format!("⚙ {name}"))),
            AgentEvent::Done { error } => {
                thread.status = if error.is_some() { Status::NeedsInput } else { Status::Idle };
                if let Some(e) = error {
                    thread.messages.push(agent_msg(format!("Error: {e}")));
                }
            }
        }
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
