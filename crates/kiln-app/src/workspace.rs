use crate::theme::{ui_font, Theme, MONO};
use gpui::{
    div, list, prelude::*, px, relative, Context, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyDownEvent, ListAlignment, ListState, PathPromptOptions, Render, SharedString, Window,
};
use kiln_agent::{claude, AgentEvent};
use kiln_core::policy::{capability_for_tool, Decision};
use kiln_core::persist::{self, Loaded};
use kiln_core::{Agent, Approval, Message, Role, Status, Store, Thread, NEW_THREAD_TITLE};

/// Widest the conversation and composer grow; they stay centered beyond it.
const COLUMN: f32 = 760.;
/// Height of the title bar row shared by the sidebar and the thread.
const TOOLBAR: f32 = 52.;

pub struct Workspace {
    store: Store,
    /// None disables saving (e.g. the existing file could not be read).
    state_path: Option<std::path::PathBuf>,
    selected: Option<u64>,
    draft: String,
    focus: FocusHandle,
    /// Palette for the current system appearance, refreshed every render.
    theme: Theme,
    /// Virtualized message list. It follows new output until the user scrolls up.
    transcript: ListState,
    /// Thread `transcript` was built for; `None` forces a jump to the latest message.
    transcript_thread: Option<u64>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_appearance(window, |_, _, cx| cx.notify()).detach();
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
        let font = ui_font(&cx.text_system().all_font_names());
        Self {
            store,
            state_path,
            selected,
            draft: String::new(),
            focus: cx.focus_handle(),
            theme: Theme::new(window.appearance(), font),
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
                    Decision::Deny => thread.messages.push(Message::notice(format!(
                        "Blocked by policy: {tool}\n{summary}"
                    ))),
                    Decision::Ask => thread.pending.push(Approval { tool, summary, rule }),
                }
            }
            AgentEvent::Done { error } => {
                if let Some(e) = error {
                    thread.status = Status::NeedsInput;
                    thread.messages.push(Message::notice(format!("Error: {e}")));
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
        thread.messages.push(Message::notice(note));
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
        thread.messages.push(Message::notice(note));
        if thread.pending.is_empty() {
            thread.status = Status::Idle;
        }
        self.save();
        cx.notify();
    }

    fn approval_cards(&self, id: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let mut col = div().id("approvals").flex().flex_col().items_center().flex_shrink_0()
            .gap_2().px_6().max_h(px(240.)).overflow_y_scroll();
        let pending = self.store.thread(id).map(|th| th.pending.clone()).unwrap_or_default();
        for (i, a) in pending.into_iter().enumerate() {
            let button = |id: String, label: &'static str, primary: bool| {
                div().id(SharedString::from(id)).px_3().py_1().rounded_md().text_xs()
                    .font_weight(FontWeight::MEDIUM).cursor_pointer()
                    .when(primary, |d| d.bg(theme.accent).text_color(theme.on_accent))
                    .when(!primary, |d| d.bg(theme.selected).text_color(theme.text))
                    .hover(|h| h.opacity(0.85))
                    .child(label)
            };
            col = col.child(
                div().flex().flex_shrink_0().items_start().gap_2().w_full().max_w(px(COLUMN))
                    .p_3().mb_2().rounded(px(12.)).bg(theme.card)
                    .border_1().border_color(theme.warn).shadow_sm()
                    .child(
                        div().flex().flex_col().flex_1().min_w_0().gap_1()
                            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.warn)
                                .child(format!("{} needs approval", a.tool)))
                            .child(div().font_family(MONO).text_xs().text_color(theme.text)
                                .child(a.summary)),
                    )
                    .child(button(format!("deny{i}"), "Deny", false)
                        .on_click(cx.listener(move |this, _, _, cx| this.deny(id, i, cx))))
                    .child(button(format!("allow{i}"), "Allow", true)
                        .on_click(cx.listener(move |this, _, _, cx| this.approve(id, i, cx)))),
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

    /// Segmented control; the choice applies from the next turn.
    fn model_chooser(&self, thread: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let id = thread.id;
        let mut track = div().flex().gap_0p5().p_0p5().rounded_lg().bg(theme.fill);
        let choices = std::iter::once(None).chain(thread.agent.models().iter().copied().map(Some));
        for choice in choices {
            let label = choice.map_or_else(|| "Default".to_string(), capitalized);
            let active = thread.model.as_deref() == choice;
            track = track.child(
                div().id(SharedString::from(format!("model-{label}")))
                    .px_2p5().py_0p5().rounded_md().text_xs().cursor_pointer()
                    .when(active, |d| {
                        d.bg(theme.segment).shadow_sm().text_color(theme.text)
                            .font_weight(FontWeight::MEDIUM)
                    })
                    .when(!active, |d| {
                        d.text_color(theme.muted).hover(|h| h.text_color(theme.text))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_model(id, choice, cx))),
            );
        }
        track
    }

    fn status_mark(&self, status: Status) -> impl IntoElement {
        let color = match status {
            Status::Running => Some(self.theme.accent),
            Status::NeedsInput => Some(self.theme.warn),
            Status::Idle => None,
        };
        div().flex_shrink_0().size(px(7.)).rounded_full().when_some(color, |d, c| d.bg(c))
    }

    /// A borderless toolbar-style button.
    fn quiet_button(
        &self,
        id: impl Into<SharedString>,
        label: &'static str,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div().id(id.into()).flex().items_center().justify_center().min_w(px(20.)).h(px(20.))
            .px_1().rounded_md().text_sm().text_color(theme.muted).cursor_pointer()
            .hover(|h| h.bg(theme.hover).text_color(theme.text))
            .child(label)
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        // macOS draws its window buttons over the top of the sidebar.
        let top = if cfg!(target_os = "macos") { TOOLBAR } else { 12. };
        let mut projects = div().id("projects").flex().flex_col().flex_1().min_h_0().gap_4()
            .px_2p5().overflow_y_scroll();
        for (pi, p) in self.store.projects.iter().enumerate() {
            let mut group = div().flex().flex_col().flex_shrink_0().gap_0p5().child(
                div().flex().items_center().gap_1().pl_2().pr_1()
                    .child(div().flex_1().min_w_0().truncate().text_xs()
                        .font_weight(FontWeight::SEMIBOLD).text_color(theme.muted)
                        .child(p.name.clone()))
                    .child(self.quiet_button(format!("new-thread-{pi}"), "+")
                        .on_click(cx.listener(move |this, _, _, cx| this.new_thread(pi, cx))))
                    .child(self.quiet_button(format!("rm-project-{pi}"), "×")
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_project(pi, cx)))),
            );
            for t in &p.threads {
                let id = t.id;
                let active = self.selected == Some(id);
                group = group.child(
                    div().id(SharedString::from(format!("t{id}")))
                        .flex().items_center().gap_2().h(px(30.)).pl_2().pr_1().rounded_md()
                        .text_sm().text_color(theme.text).cursor_pointer()
                        .when(active, |d| d.bg(theme.selected).font_weight(FontWeight::MEDIUM))
                        .when(!active, |d| d.hover(|h| h.bg(theme.hover)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.selected = Some(id);
                            window.focus(&this.focus);
                            cx.notify();
                        }))
                        .child(self.status_mark(t.status))
                        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().line_clamp(1)
                            .child(t.title.clone()))
                        .child(div().text_xs().text_color(theme.muted).child(t.agent.label()))
                        .child(self.quiet_button(format!("del{id}"), "×")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.delete_thread(id, cx);
                            }))),
                );
            }
            projects = projects.child(group);
        }
        let add = div().id("add-project").flex().flex_shrink_0().items_center().gap_2()
            .mx_2p5().mb_2p5().px_2().h(px(30.)).rounded_md().text_sm().text_color(theme.muted)
            .cursor_pointer().hover(|h| h.bg(theme.hover).text_color(theme.text))
            .child("+").child("Add Project")
            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx)));
        div().flex().flex_col().flex_shrink_0().gap_2().w(px(260.)).h_full().pt(px(top))
            .bg(theme.sidebar).border_r_1().border_color(theme.border)
            .child(projects)
            .child(add)
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let (msg, action) = if self.store.projects.is_empty() {
            ("Add a project folder to start working with an agent.", "Add Project")
        } else {
            ("Select a thread, or create one with + next to a project.", "")
        };
        div().flex().flex_col().flex_1().h_full().items_center().justify_center().gap_3()
            .bg(theme.window)
            .child(div().text_sm().text_color(theme.muted).child(msg))
            .when(!action.is_empty(), |d| {
                d.child(
                    div().id("empty-add").px_4().py_1p5().rounded_lg().text_sm()
                        .font_weight(FontWeight::MEDIUM).cursor_pointer()
                        .bg(theme.accent).text_color(theme.on_accent).child(action)
                        .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                )
            })
    }

    fn message(&self, id: u64, ix: usize) -> gpui::AnyElement {
        let theme = self.theme;
        let Some(m) = self.store.thread(id).and_then(|t| t.messages.get(ix)) else {
            return div().into_any_element();
        };
        let body = div().min_w_0().text_sm().line_height(relative(1.55));
        let content = match (&m.tool, m.role) {
            (Some(name), _) => self.tool_call(name, &m.body).into_any_element(),
            (None, Role::User) => div().flex().justify_end()
                .child(body.max_w(px(560.)).px_3p5().py_2().rounded(px(18.))
                    .bg(theme.accent).text_color(theme.on_accent)
                    .child(m.body.clone()))
                .into_any_element(),
            (None, Role::Agent) => body.child(m.body.clone()).into_any_element(),
            (None, Role::Notice) => body.text_xs().text_color(theme.muted)
                .child(m.body.clone())
                .into_any_element(),
        };
        div().flex().flex_col().items_center().w_full().px_6().py_1p5()
            .child(div().w_full().max_w(px(COLUMN)).min_w_0().child(content))
            .into_any_element()
    }

    /// The exact tool name and its input, as the agent sent them.
    fn tool_call(&self, name: &str, input: &str) -> impl IntoElement {
        let theme = self.theme;
        div().flex().flex_col().w_full().min_w_0().rounded(px(10.)).bg(theme.fill)
            .border_1().border_color(theme.border)
            .child(div().px_3().py_1p5().text_xs().font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted).child(name.to_string()))
            .when(!input.is_empty(), |d| {
                d.child(div().px_3().py_2().border_t_1().border_color(theme.border)
                    .font_family(MONO).text_xs().line_height(relative(1.6))
                    .text_color(theme.text).child(input.to_string()))
            })
    }

    fn composer(&self, thread: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let running = thread.status == Status::Running;
        let ready = !running && !self.draft.trim().is_empty();
        let text = if self.draft.is_empty() {
            div().text_color(theme.muted).child("Ask the agent to do something")
        } else {
            div().text_color(theme.text).child(format!("{}▏", self.draft))
        };
        let note = if running {
            Some("Working…".to_string())
        } else {
            thread.reported_model.clone()
        };
        let send = div().id("send").flex().flex_shrink_0().items_center().justify_center()
            .size(px(28.)).rounded_full().text_sm().font_weight(FontWeight::BOLD)
            .when(ready, |d| {
                d.bg(theme.accent).text_color(theme.on_accent).cursor_pointer()
                    .hover(|h| h.opacity(0.85))
            })
            .when(!ready, |d| d.bg(theme.selected).text_color(theme.muted))
            .child("↑")
            .on_click(cx.listener(|this, _, _, cx| this.submit(cx)));
        // A column, so the card's height follows its wrapped draft.
        div().flex().flex_col().items_center().flex_shrink_0().px_6().pt_1().pb_4().child(
            div().flex().flex_col().gap_2().w_full().max_w(px(COLUMN)).p_2().rounded(px(16.))
                .bg(theme.card).border_1().border_color(theme.border).shadow_md()
                .child(
                    div().id("composer").track_focus(&self.focus)
                        .on_key_down(cx.listener(|this, ev, _, cx| this.on_key(ev, cx)))
                        .on_click(cx.listener(|this, _, window, _| window.focus(&this.focus)))
                        .px_2().py_1p5().text_sm().child(text),
                )
                .child(
                    div().flex().items_center().gap_3()
                        .child(self.model_chooser(thread, cx))
                        .child(div().flex_1().min_w_0().truncate().text_xs()
                            .text_color(theme.muted).text_right()
                            .when_some(note, |d, note| d.child(note)))
                        .child(send),
                ),
        )
    }

    fn toolbar(&self, id: u64, thread: &Thread) -> impl IntoElement {
        let theme = self.theme;
        let project = self.store.locate(id).map(|(p, _)| self.store.projects[p].name.clone());
        let (badge, color) = if thread.policy.is_local_only() {
            ("Local only", theme.ok)
        } else {
            ("Network allowed", theme.warn)
        };
        div().flex().flex_shrink_0().items_center().gap_3().px_5().h(px(TOOLBAR))
            .border_b_1().border_color(theme.border)
            .child(
                div().flex().flex_col().flex_1().min_w_0()
                    .child(div().truncate().text_sm().font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text).child(thread.title.clone()))
                    .when_some(project, |d, name| {
                        d.child(div().truncate().text_xs().text_color(theme.muted).child(name))
                    }),
            )
            .child(
                div().flex().items_center().gap_1p5().px_2p5().py_1().rounded_full()
                    .bg(theme.fill).text_xs().text_color(theme.muted)
                    .child(div().size(px(6.)).rounded_full().bg(color))
                    .child(badge),
            )
    }

    fn thread(&self, id: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(thread) = self.store.thread(id) else {
            return self.empty_state(cx).into_any_element();
        };
        let this = cx.entity();
        let messages = list(self.transcript.clone(), move |ix, _, cx| {
            this.read(cx).message(id, ix)
        })
        .flex_1()
        .min_h_0()
        .py_3();
        div().flex().flex_col().flex_1().min_w_0().h_full().bg(self.theme.window)
            .child(self.toolbar(id, thread))
            .child(messages)
            .child(self.approval_cards(id, cx))
            .child(self.composer(thread, cx))
            .into_any_element()
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
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
        self.theme = Theme::new(window.appearance(), self.theme.font);
        self.sync_transcript();
        // No root background: the macOS sidebar lets the blur through.
        div().flex().size_full().font_family(self.theme.font).text_color(self.theme.text)
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
