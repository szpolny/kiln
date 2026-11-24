use crate::theme;
use gpui::{div, prelude::*, px, Context, IntoElement, Render, SharedString, Window};

#[derive(Clone, Copy, PartialEq)]
enum Status { Running, NeedsInput, Idle }

struct Thread { title: &'static str, agent: &'static str, status: Status }
struct Project { name: &'static str, threads: Vec<Thread> }
struct Message { from_user: bool, body: &'static str }

pub struct Workspace {
    projects: Vec<Project>,
    selected: (usize, usize),
    messages: Vec<Message>,
}

impl Workspace {
    /// Mock data until the control-plane core exists.
    pub fn demo() -> Self {
        Self {
            projects: vec![
                Project { name: "kiln", threads: vec![
                    Thread { title: "Scaffold GPUI shell", agent: "claude", status: Status::Running },
                    Thread { title: "Sandbox policy draft", agent: "codex", status: Status::NeedsInput },
                ]},
                Project { name: "nix", threads: vec![
                    Thread { title: "Tidy home-manager", agent: "claude", status: Status::Idle },
                ]},
            ],
            selected: (0, 0),
            messages: vec![
                Message { from_user: true, body: "Set up the app shell." },
                Message { from_user: false, body: "Done. Sidebar, thread view and composer are in place. All data stays on this machine." },
            ],
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
        for (pi, p) in self.projects.iter().enumerate() {
            let mut group = div().flex().flex_col().gap_0p5()
                .child(div().px_2().py_1().text_xs().text_color(theme::muted()).child(p.name));
            for (ti, t) in p.threads.iter().enumerate() {
                let active = self.selected == (pi, ti);
                group = group.child(
                    div().id(SharedString::from(format!("t{pi}-{ti}")))
                        .flex().items_center().gap_2().px_2().py_1p5().rounded_md()
                        .text_sm().text_color(theme::text())
                        .when(active, |d| d.bg(theme::hover()))
                        .hover(|d| d.bg(theme::hover()))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected = (pi, ti);
                            cx.notify();
                        }))
                        .child(Self::dot(t.status))
                        .child(div().flex_1().child(t.title))
                        .child(div().text_xs().text_color(theme::muted()).child(t.agent)),
                );
            }
            col = col.child(group);
        }
        col
    }

    fn thread(&self) -> impl IntoElement {
        let t = &self.projects[self.selected.0].threads[self.selected.1];
        let mut msgs = div().flex().flex_col().gap_4().flex_1().p_6();
        for m in &self.messages {
            msgs = msgs.child(
                div().flex().when(m.from_user, |d| d.justify_end()).child(
                    div().max_w(px(560.)).px_3().py_2().rounded_lg().text_sm()
                        .text_color(theme::text())
                        .when(m.from_user, |d| d.bg(theme::surface()))
                        .child(m.body),
                ),
            );
        }
        div().flex().flex_col().flex_1().h_full()
            .child(
                div().flex().items_center().gap_2().px_4().h(px(44.))
                    .border_b_1().border_color(theme::border())
                    .text_sm().text_color(theme::text()).child(t.title)
                    .child(div().text_xs().text_color(theme::ok()).child("● local only")),
            )
            .child(msgs)
            .child(
                div().p_4().child(
                    div().px_4().py_3().rounded_xl().bg(theme::surface())
                        .border_1().border_color(theme::border())
                        .text_sm().text_color(theme::muted())
                        .child("Ask anything, or @mention a file…"),
                ),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().size_full().bg(theme::bg()).text_color(theme::text())
            .child(self.sidebar(cx))
            .child(self.thread())
    }
}
