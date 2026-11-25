use crate::assets;
use crate::markdown;
use crate::sidebar::{now, Row};
use crate::text_input::{self, InputEvent, TextInput};
use crate::theme::{ui_font, Theme, MONO};
use gpui::{
    anchored, deferred, div, img, list, point, prelude::*, px, relative, rgb, svg, Anchor, AnchoredPositionMode, Context,
    FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, ListAlignment,
    ListState, MouseButton, Entity, PathPromptOptions, Render, SharedString, Window,
};
use kiln_agent::{claude, process::Process, project, AgentEvent};
use kiln_core::policy::{capability_for_tool, Capability, Decision, Place};
use kiln_core::persist::{self, Loaded};
use kiln_core::{
    Agent, Approval, Effort, Message, Role, Status, Store, Thread, NEW_THREAD_TITLE,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Widest the conversation and composer grow; they stay centered beyond it.
const COLUMN: f32 = 760.;
/// Height of the title bar row shared by the sidebar and the thread.
pub(crate) const TOOLBAR: f32 = 52.;
/// How often the sidebar's ages and branches are brought up to date.
const REFRESH: Duration = Duration::from_secs(30);

pub struct Workspace {
    pub(crate) store: Store,
    /// None disables saving (e.g. the existing file could not be read).
    state_path: Option<std::path::PathBuf>,
    pub(crate) selected: Option<u64>,
    /// Process of each running turn, by thread id.
    turns: HashMap<u64, Process>,
    pub(crate) input: Entity<TextInput>,
    focus: FocusHandle,
    /// Palette for the current system appearance, refreshed every render.
    pub(crate) theme: Theme,
    /// The sidebar's entries, in the order shown.
    pub(crate) rows: Vec<Row>,
    /// Virtualized sidebar list over `rows`.
    pub(crate) thread_list: ListState,
    /// Checked-out branch of each project folder that is in a repository.
    pub(crate) branches: HashMap<PathBuf, String>,
    /// Virtualized message list. It follows new output until the user scrolls up.
    transcript: ListState,
    /// Thread `transcript` was built for; `None` forces a jump to the latest message.
    transcript_thread: Option<u64>,
    /// Parsed replies of that thread by message index, so frames do not reparse.
    parsed: HashMap<usize, Rc<[markdown::Block]>>,
    /// The dropdown menu that is open, if any.
    pub(crate) menu: Option<Menu>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Menu {
    NewThread,
    Model,
    Effort,
    Web,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_appearance(window, |_, _, cx| cx.notify()).detach();
        // Branches may have been switched while the window was in the background.
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh(cx);
            }
        })
        .detach();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH).await;
            if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                break;
            }
        })
        .detach();
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
        let input = cx.new(|cx| TextInput::new("Ask the agent to do something", window, cx));
        cx.subscribe(&input, |this, _, ev, cx| match ev {
            InputEvent::Submit => this.submit(cx),
            InputEvent::Changed => cx.notify(),
        })
        .detach();
        let mut this = Self {
            store,
            state_path,
            selected,
            turns: HashMap::new(),
            input,
            focus: cx.focus_handle(),
            theme: Theme::new(window.appearance(), font),
            rows: Vec::new(),
            thread_list: ListState::new(0, ListAlignment::Top, px(200.)),
            branches: HashMap::new(),
            transcript: ListState::new(0, ListAlignment::Bottom, px(400.)),
            transcript_thread: None,
            parsed: HashMap::new(),
            menu: None,
        };
        this.sync_rows();
        this.refresh(cx);
        this
    }

    /// Reread the branches and redraw, which also brings the ages up to date.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.branches = self
            .store
            .projects
            .iter()
            .filter_map(|p| Some((p.dir.clone(), project::branch(&p.dir)?)))
            .collect();
        cx.notify();
    }

    /// Write the state to disk. Every change to the thread list ends here, so
    /// the sidebar is rebuilt too.
    fn save(&mut self) {
        self.sync_rows();
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
            self.parsed.clear();
            self.transcript_thread = self.selected;
        } else if count > known {
            self.transcript.splice(known..known, count - known);
        }
    }

    pub(crate) fn add_project(&mut self, cx: &mut Context<Self>) {
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
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    pub(crate) fn new_thread(&mut self, p: usize, cx: &mut Context<Self>) {
        if let Some(id) = self.store.add_thread(p, Agent::default(), now()) {
            self.selected = Some(id);
            self.save();
            cx.notify();
        }
    }

    pub(crate) fn set_settled(&mut self, id: u64, settled: bool, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(id) else {
            return;
        };
        if settled {
            if !thread.settle() {
                return;
            }
        } else {
            thread.settled = false;
        }
        self.save();
        cx.notify();
    }

    pub(crate) fn delete_thread(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.store.delete_thread(id) {
            if self.selected == Some(id) {
                self.selected = self.store.first_thread_id();
            }
            self.save();
            cx.notify();
        }
    }

    pub(crate) fn remove_project(&mut self, p: usize, cx: &mut Context<Self>) {
        if self.store.remove_project(p) {
            if self.selected.is_some_and(|id| self.store.locate(id).is_none()) {
                self.selected = self.store.first_thread_id();
            }
            self.save();
            cx.notify();
        }
    }

    /// Escape closes an open menu, or else stops the shown thread's turn. Text
    /// editing is the composer's.
    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        // A held key must not close a menu and then go on to stop the turn.
        if ev.keystroke.key != "escape" || ev.is_held {
            return;
        }
        if self.menu.take().is_some() {
            cx.notify();
        } else if let Some(id) = self.selected {
            self.stop(id, cx);
        }
    }

    /// Stop the thread's turn. It stays running until the process has exited.
    fn stop(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(process) = self.turns.get(&id) {
            process.stop();
            cx.notify();
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let body = self.input.read(cx).text().trim().to_string();
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
        thread.touch(now());
        self.input.update(cx, |input, cx| input.clear(cx));
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
        let (web_allowed, web_disallowed) = thread.policy.web_tool_rules();
        let mut granted = allowed_tools;
        granted.extend(web_allowed.iter().map(|t| t.to_string()));
        let started = claude::run(claude::Turn {
            dir,
            prompt,
            resume: thread.session_id.clone(),
            model: Some(thread.model().to_string()),
            effort: Some(thread.effort().id().to_string()),
            allowed_tools: granted.clone(),
            disallowed_tools: web_disallowed.iter().map(|t| t.to_string()).collect(),
        });
        cx.notify();
        let (process, rx) = match started {
            Ok(started) => started,
            Err(e) => {
                let error = Some(format!("could not start claude: {e}"));
                self.apply(id, AgentEvent::Done { error }, &granted);
                self.save();
                return;
            }
        };
        self.turns.insert(id, process);

        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                let done = matches!(ev, AgentEvent::Done { .. } | AgentEvent::Stopped);
                let _ = this.update(cx, |this, cx| {
                    if done {
                        this.turns.remove(&id);
                    }
                    let retry = this.apply(id, ev, &granted);
                    if done {
                        this.save();
                        // The turn may have switched branches.
                        this.refresh(cx);
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
    /// `granted` holds the allow rules the turn was started with.
    fn apply(&mut self, id: u64, ev: AgentEvent, granted: &[String]) -> Option<Vec<String>> {
        let dir = self.store.project_dir_of(id)?;
        let thread = self.store.thread_mut(id)?;
        match ev {
            AgentEvent::Session(id) => thread.session_id = Some(id),
            AgentEvent::Model(model) => thread.reported_model = Some(model),
            AgentEvent::Text(text) => thread.messages.push(Message::agent(text)),
            AgentEvent::Tool { name, input } => {
                thread.messages.push(Message::tool_call(name, input))
            }
            // Already granted, so something outside Kiln refused it and a
            // retry would only be refused again.
            AgentEvent::Denied { tool, summary, rule, .. } if granted.contains(&rule) => {
                thread.messages.push(Message::notice(format!(
                    "Blocked by the agent's own settings: {tool}\n{summary}"
                )))
            }
            AgentEvent::Denied { tool, summary, rule, path } => {
                let place = path.map_or(Place::Outside, |path| {
                    let resolved = project::inside(&dir, &path);
                    Place::of(resolved.as_deref(), path.strip_prefix(&dir).unwrap_or(&path))
                });
                let capability = capability_for_tool(&tool, place);
                match thread.policy.decide(capability) {
                    Decision::Allow => thread.auto_allow.push(rule),
                    Decision::Deny => {
                        let why = match capability {
                            Capability::OutsideFiles => "Blocked outside the project",
                            _ => "Blocked by policy",
                        };
                        let note = format!("{why}: {tool}\n{summary}");
                        thread.messages.push(Message::notice(note));
                    }
                    Decision::Ask => thread.pending.push(Approval { tool, summary, rule }),
                }
            }
            AgentEvent::Done { error } => {
                if let Some(e) = error {
                    thread.touch(now());
                    thread.status = Status::NeedsInput;
                    thread.messages.push(Message::notice(format!("Error: {e}")));
                } else if !thread.auto_allow.is_empty() {
                    return Some(std::mem::take(&mut thread.auto_allow));
                } else {
                    thread.touch(now());
                    thread.status =
                        if thread.pending.is_empty() { Status::Idle } else { Status::NeedsInput };
                }
            }
            AgentEvent::Stopped => {
                thread.touch(now());
                thread.stop_turn();
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
        thread.touch(now());
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
        thread.touch(now());
        if thread.pending.is_empty() {
            thread.status = Status::Idle;
        }
        self.save();
        cx.notify();
    }

    fn approval_cards(&self, id: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let mut col = div().id("approvals").flex().flex_col().flex_shrink_0()
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
            col = col.child(centered(
                div().flex().items_start().gap_2().w_full()
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
            ).flex_shrink_0());
        }
        col
    }

    fn set_model(&mut self, id: u64, model: &str, cx: &mut Context<Self>) {
        if let Some(thread) = self.store.thread_mut(id) {
            thread.model = Some(model.to_string());
            self.save();
            cx.notify();
        }
    }

    fn set_effort(&mut self, id: u64, effort: Effort, cx: &mut Context<Self>) {
        if let Some(thread) = self.store.thread_mut(id) {
            thread.effort = Some(effort);
            self.save();
            cx.notify();
        }
    }

    /// Compact picker in the composer footer; the choice applies from the next turn.
    fn model_picker(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let button = self.footer_button("model-picker", Menu::Model, cx)
            .child(agent_icon(thread.agent, 13.))
            .child(div().font_weight(FontWeight::MEDIUM)
                .child(thread.agent.model_name(thread.model()).to_string()));
        if self.menu != Some(Menu::Model) {
            return self.footer_picker(button, None, window, cx);
        }
        let id = thread.id;
        let mut menu = self.menu_panel("model-menu", capitalized(thread.agent.label()));
        for model in thread.agent.models() {
            let (model_id, active) = (model.id, thread.model() == model.id);
            let key = SharedString::from(format!("model-{model_id}"));
            menu = menu.child(
                self.menu_item(key, Some(thread.agent), model.name, None, active)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.set_model(id, model_id, cx);
                    })),
            );
        }
        self.footer_picker(button, Some(menu), window, cx)
    }

    /// Reasoning level picker beside the model; applies from the next turn.
    fn effort_picker(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let button = self.footer_button("effort-picker", Menu::Effort, cx)
            .child(assets::icon(assets::Icon::Brain, 13.).text_color(self.theme.muted))
            .child(div().font_weight(FontWeight::MEDIUM).child(thread.effort().name()));
        if self.menu != Some(Menu::Effort) {
            return self.footer_picker(button, None, window, cx);
        }
        let id = thread.id;
        let mut menu = self.menu_panel("effort-menu", "Reasoning");
        for &effort in thread.agent.efforts() {
            let key = SharedString::from(format!("effort-{}", effort.id()));
            menu = menu.child(
                self.menu_item(key, None, effort.name(), None, thread.effort() == effort)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.set_effort(id, effort, cx);
                    })),
            );
        }
        self.footer_picker(button, Some(menu), window, cx)
    }

    /// A dropdown button in the composer footer. The caller adds its label.
    fn footer_button(
        &self,
        id: &'static str,
        menu: Menu,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        let open = self.menu == Some(menu);
        div().id(id).flex().flex_shrink_0().items_center().gap_1p5().h(px(26.)).px_2()
            .rounded_md().text_xs().cursor_pointer()
            .when(open, |d| d.bg(theme.selected).text_color(theme.text))
            .when(!open, |d| d.text_color(theme.muted).hover(|h| h.bg(theme.hover).text_color(theme.text)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_menu(menu, cx)))
    }

    /// Finish a footer dropdown, showing `menu` above its button when open.
    fn footer_picker(
        &self,
        button: gpui::Stateful<gpui::Div>,
        menu: Option<gpui::Stateful<gpui::Div>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let button = button.child(div().text_color(self.theme.muted).child("▾"));
        let picker = div().relative().child(button);
        let Some(menu) = menu else {
            return picker;
        };
        // Anchored at the button's top-left corner, so the menu opens above it.
        picker
            .child(self.backdrop(window, cx))
            .child(div().absolute().top_0().left_0().child(
                deferred(anchored().anchor(Anchor::BottomLeft).snap_to_window().child(menu.mb_2()))
                    .with_priority(2),
            ))
    }

    pub(crate) fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    /// A see-through layer under an open menu. It catches clicks outside the
    /// menu, including on its button, so one click only closes the menu.
    pub(crate) fn backdrop(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let layer = div().id("menu-backdrop").w(viewport.width).h(viewport.height).occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }));
        deferred(
            anchored().position_mode(AnchoredPositionMode::Window).position(point(px(0.), px(0.)))
                .child(layer),
        )
        .with_priority(1)
    }

    pub(crate) fn menu_panel(&self, id: &'static str, title: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div().id(id).flex().flex_col().w(px(220.)).p_1().rounded(px(10.)).bg(theme.card)
            .border_1().border_color(theme.border).shadow_lg().occlude()
            .child(div().px_2().pt_1().pb_1p5().text_xs().font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted).child(title.into()))
    }

    fn menu_item(
        &self,
        id: impl Into<SharedString>,
        icon: Option<Agent>,
        label: impl Into<SharedString>,
        hint: Option<&'static str>,
        active: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div().id(id.into()).flex().items_center().gap_2().h(px(28.)).px_2().rounded_md().text_sm()
            .text_color(theme.text).cursor_pointer().hover(|h| h.bg(theme.hover))
            .when_some(icon, |d, agent| d.child(agent_icon(agent, 14.)))
            .child(div().flex_1().child(label.into()))
            .when_some(hint, |d, hint| {
                d.child(div().text_xs().text_color(theme.muted).child(hint))
            })
            .child(div().w(px(12.)).text_xs().text_color(theme.accent)
                .child(if active { "✓" } else { "" }))
    }

    fn set_web_access(&mut self, id: u64, access: Decision, cx: &mut Context<Self>) {
        if let Some(thread) = self.store.thread_mut(id) {
            thread.policy.network = access;
            self.save();
            cx.notify();
        }
    }

    /// Toolbar button for the thread's web tools; the choice applies from the
    /// next turn.
    fn web_picker(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let open = self.menu == Some(Menu::Web);
        let current = thread.policy.network;
        let (label, color) = match current {
            Decision::Ask => ("Web: ask first", theme.ok),
            Decision::Allow => ("Web: allowed", theme.warn),
            Decision::Deny => ("Web: blocked", theme.ok),
        };
        let button = div().id("web-picker").flex().items_center().gap_1p5().px_2p5().py_1()
            .rounded_full().text_xs().text_color(theme.muted).cursor_pointer()
            .when(open, |d| d.bg(theme.selected).text_color(theme.text))
            .when(!open, |d| d.bg(theme.fill).hover(|h| h.bg(theme.hover).text_color(theme.text)))
            .child(div().size(px(6.)).rounded_full().bg(color))
            .child(label)
            .child("▾")
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Web, cx)));
        let picker = div().relative().child(button);
        if !open {
            return picker;
        }
        let id = thread.id;
        let mut menu = self.menu_panel("web-menu", "Web search and fetch").mt_2();
        for (access, label) in [
            (Decision::Ask, "Ask first"),
            (Decision::Allow, "Allow"),
            (Decision::Deny, "Block"),
        ] {
            let key = SharedString::from(format!("web-{label}"));
            menu = menu.child(
                self.menu_item(key, None, label, None, current == access)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.set_web_access(id, access, cx);
                    })),
            );
        }
        menu = menu.child(
            div().px_2().pt_1p5().pb_1().text_xs().text_color(theme.muted)
                .child("Applies from the next turn. Commands and the model connection are not covered."),
        );
        // Anchored at the button's bottom-right corner, so the menu opens below it.
        picker
            .child(self.backdrop(window, cx))
            .child(div().absolute().bottom_0().right_0().child(
                deferred(anchored().anchor(Anchor::TopRight).snap_to_window().child(menu))
                    .with_priority(2),
            ))
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let (msg, action) = if self.store.projects.is_empty() {
            ("Add a project folder to start working with an agent.", "Add Project")
        } else {
            ("Select a thread, or start one with the new thread button.", "")
        };
        div().flex().flex_col().flex_1().h_full().items_center().justify_center().gap_3()
            .bg(theme.window)
            .child(img(theme.logo).flex_shrink_0().size(px(assets::LOGO_SIZE)))
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

    fn message(&mut self, id: u64, ix: usize) -> gpui::AnyElement {
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
            (None, Role::Agent) => {
                let blocks = self.parsed.entry(ix).or_insert_with(|| markdown::parse(&m.body).into());
                body.child(markdown::render(blocks, theme, ix)).into_any_element()
            }
            (None, Role::Notice) => body.text_xs().text_color(theme.muted)
                .child(m.body.clone())
                .into_any_element(),
        };
        centered(content).px_6().py_1p5().into_any_element()
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

    /// The round button at the composer's corner. The caller adds its mark.
    fn round_button(&self, id: &'static str, enabled: bool) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div().id(id).flex().flex_shrink_0().items_center().justify_center()
            .size(px(28.)).rounded_full().text_sm().font_weight(FontWeight::BOLD)
            .when(enabled, |d| {
                d.bg(theme.accent).text_color(theme.on_accent).cursor_pointer()
                    .hover(|h| h.opacity(0.85))
            })
            .when(!enabled, |d| d.bg(theme.selected).text_color(theme.muted))
    }

    fn send_button(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let ready = !self.input.read(cx).text().trim().is_empty();
        self.round_button("send", ready)
            .child("↑")
            .on_click(cx.listener(|this, _, _, cx| this.submit(cx)))
    }

    /// Takes the send button's place while a turn runs.
    fn stop_button(&self, id: u64, enabled: bool, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let mark = if enabled { self.theme.on_accent } else { self.theme.muted };
        self.round_button("stop", enabled)
            .child(div().size(px(10.)).rounded(px(2.)).bg(mark))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.stop(id, cx);
                // The click took focus; hand it back for the next message.
                window.focus(&this.input.focus_handle(cx), cx);
            }))
    }

    fn composer(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let running = thread.status == Status::Running;
        let stopping = self.turns.get(&thread.id).is_some_and(Process::stop_requested);
        let note = match (running, stopping) {
            (_, true) => Some("Stopping…".to_string()),
            (true, false) => Some("Working… Esc to stop".to_string()),
            (false, false) => thread.reported_model.clone(),
        };
        let button = if running {
            self.stop_button(thread.id, !stopping, cx)
        } else {
            self.send_button(cx)
        };
        centered(
            div().flex().flex_col().gap_2().w_full().p_2().rounded(px(16.))
                .bg(theme.card).border_1().border_color(theme.border).shadow_md()
                .child(
                    div().id("composer")
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.focus(&this.input.focus_handle(cx), cx)
                        }))
                        .px_2().py_1p5().text_sm().child(self.input.clone()),
                )
                .child(
                    div().flex().items_center().gap_3().pl_0p5()
                        .child(div().flex().items_center().gap_0p5()
                            .child(self.model_picker(thread, window, cx))
                            .child(self.effort_picker(thread, window, cx)))
                        .child(div().flex_1().min_w_0().truncate().text_xs()
                            .text_color(theme.muted).text_right()
                            .when_some(note, |d, note| d.child(note)))
                        .child(button),
                ),
        )
        .flex_shrink_0()
        .px_6()
        .pt_1()
        .pb_4()
    }

    fn toolbar(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let project = self.store.locate(thread.id).map(|(p, _)| self.store.projects[p].name.clone());
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
            .child(self.web_picker(thread, window, cx))
    }

    fn thread(&self, id: u64, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(thread) = self.store.thread(id) else {
            return self.empty_state(cx).into_any_element();
        };
        let this = cx.entity();
        let messages = list(self.transcript.clone(), move |ix, _, cx| {
            this.update(cx, |this, _| this.message(id, ix))
        })
        .flex_1()
        .min_h_0()
        .py_3();
        div().flex().flex_col().flex_1().min_w_0().h_full().bg(self.theme.window)
            .child(self.toolbar(thread, window, cx))
            .child(messages)
            .child(self.approval_cards(id, cx))
            .child(self.composer(thread, window, cx))
            .into_any_element()
    }
}

/// Center `child` in the conversation column.
///
/// It has to be a row: that settles the capped width before the height is
/// measured. Centering from a column measures wrapped text at the uncapped
/// width, so it comes out lines short and the next element overlaps it.
fn centered(child: impl IntoElement) -> gpui::Div {
    div().flex().justify_center().w_full()
        .child(div().flex_1().min_w_0().max_w(px(COLUMN)).child(child))
}

/// The provider's mark, in its brand color.
pub(crate) fn agent_icon(agent: Agent, size: f32) -> impl IntoElement {
    let (path, color) = match agent {
        Agent::Claude => (assets::CLAUDE, rgb(0xd97757)),
    };
    svg().path(path).flex_shrink_0().size(px(size)).text_color(color)
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
        if window.focused(cx).is_none() {
            window.focus(&self.input.focus_handle(cx), cx);
        }
        self.theme = Theme::new(window.appearance(), self.theme.font);
        let theme = self.theme;
        let selection = gpui::Rgba { a: 0.3, ..theme.accent };
        self.input.update(cx, |input, _| {
            input.colors = text_input::Colors {
                text: theme.text.into(),
                placeholder: theme.muted.into(),
                caret: theme.accent.into(),
                selection: selection.into(),
            }
        });
        self.sync_transcript();
        // No root background: the macOS sidebar lets the blur through.
        div().flex().size_full().font_family(self.theme.font).text_color(self.theme.text)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev, _, cx| this.on_key(ev, cx)))
            .child(self.sidebar(window, cx))
            .child(match self.selected {
                Some(id) => self.thread(id, window, cx),
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
