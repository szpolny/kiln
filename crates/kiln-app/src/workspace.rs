use crate::assets;
use crate::markdown;
use crate::project_icon;
use crate::settings::{OpenSettings, SettingsModal};
use crate::sidebar::{now, Row};
use crate::text_input::{self, InputEvent, TextInput};
use crate::theme::{ui_font, Theme, MONO};
use crate::transcript::{self, Entry};
use chrono::{Local, TimeZone};
use gpui::{
    anchored, canvas, deferred, div, fill, img, list, point, prelude::*, px, relative, rgb, size,
    svg, Anchor, AnchoredPositionMode, Animation, AnimationExt, Bounds, Context, DispatchPhase,
    Entity, FocusHandle, Focusable, FontWeight, HitboxBehavior, IntoElement, KeyDownEvent,
    ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathPromptOptions, Pixels, Render, RenderImage, ScrollHandle, SharedString, Task, Window,
};
use kiln_agent::{claude, codex, process::Process, project, AgentEvent};
use kiln_core::policy::{capability_for_tool, Capability, Decision, PermissionMode, Place, WEB_TOOLS};
use kiln_core::persist::{self, Loaded};
use kiln_core::{
    Agent, Approval, Effort, Message, Role, Status, Store, Thread, NEW_THREAD_TITLE,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Widest the conversation and composer grow; they stay centered beyond it.
const COLUMN: f32 = 760.;
/// Space above and below the conversation.
const TRANSCRIPT_PADDING: f32 = 12.;
/// Height of the title bar row shared by the sidebar and the thread.
pub(crate) const TOOLBAR: f32 = 52.;
/// How often the sidebar's ages, branches, and icons are brought up to date.
const REFRESH: Duration = Duration::from_secs(30);
const MENU_ROW_HEIGHT: f32 = 28.;
const SCROLLBAR_GUTTER: f32 = 10.;

/// A turn in progress.
struct RunningTurn {
    process: Process,
    started: Instant,
    codex: Option<codex::Client>,
    approvals: Vec<codex::ApprovalRequest>,
}

impl RunningTurn {
    fn stop(&self) {
        if let Some(client) = &self.codex {
            client.stop(&self.process);
        } else {
            self.process.stop();
        }
    }
}

pub struct Workspace {
    pub(crate) store: Store,
    /// None disables saving (e.g. the existing file could not be read).
    state_path: Option<std::path::PathBuf>,
    pub(crate) selected: Option<u64>,
    /// Each running turn, by thread id.
    turns: HashMap<u64, RunningTurn>,
    pub(crate) input: Entity<TextInput>,
    focus: FocusHandle,
    /// Palette for the current appearance preference, refreshed every render.
    pub(crate) theme: Theme,
    pub(crate) settings_modal: Option<SettingsModal>,
    /// The sidebar's entries, in the order shown.
    pub(crate) rows: Vec<Row>,
    /// Virtualized sidebar list over `rows`.
    pub(crate) thread_list: ListState,
    /// Checked-out branch of each project folder that is in a repository.
    pub(crate) branches: HashMap<PathBuf, String>,
    /// Icon of each project folder that ships one.
    pub(crate) icons: HashMap<PathBuf, Arc<RenderImage>>,
    /// The icon lookup in progress; starting another cancels it.
    icon_load: Task<()>,
    /// Virtualized list over `entries`. It follows new output until the user
    /// scrolls up.
    transcript: ListState,
    /// The conversation's rows, in the order shown.
    entries: Vec<Entry>,
    /// Thread `transcript` was built for; `None` forces a jump to the latest message.
    transcript_thread: Option<u64>,
    /// Message count and whether a turn was running when `entries` was built;
    /// `None` forces a rebuild.
    transcript_built: Option<(usize, bool)>,
    /// Expanded work, by thread id and the index of its first message.
    open_work: HashSet<(u64, usize)>,
    /// Expanded groups of consecutive tool calls, by thread and first message.
    open_tool_groups: HashSet<(u64, usize)>,
    /// Expanded tool calls, by thread id and message index.
    open_tools: HashSet<(u64, usize)>,
    /// Parsed replies of that thread by message index, so frames do not reparse.
    parsed: HashMap<usize, Rc<[markdown::Block]>>,
    /// The dropdown menu that is open, if any.
    pub(crate) menu: Option<Menu>,
    model_scroll: ScrollHandle,
    model_scroll_grab: Rc<Cell<Option<Pixels>>>,
    /// The open project menu reports that the picked file is no icon.
    pub(crate) icon_rejected: bool,
}

impl Drop for Workspace {
    fn drop(&mut self) {
        for turn in self.turns.values() {
            turn.stop();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Menu {
    NewThread,
    Model { agent: Agent, expanded: bool, choosing_provider: bool },
    Effort,
    Permissions,
    Project,
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
        let theme = Theme::new(window.appearance(), store.settings.appearance, font);
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
            theme,
            settings_modal: None,
            rows: Vec::new(),
            thread_list: ListState::new(0, ListAlignment::Top, px(200.)),
            branches: HashMap::new(),
            icons: HashMap::new(),
            icon_load: Task::ready(()),
            transcript: ListState::new(0, ListAlignment::Bottom, px(400.)),
            entries: Vec::new(),
            transcript_thread: None,
            transcript_built: None,
            open_work: HashSet::new(),
            open_tool_groups: HashSet::new(),
            open_tools: HashSet::new(),
            parsed: HashMap::new(),
            menu: None,
            model_scroll: ScrollHandle::new(),
            model_scroll_grab: Rc::default(),
            icon_rejected: false,
        };
        this.sync_rows();
        this.refresh(cx);
        this
    }

    /// Reread the branches and redraw, which also brings the ages up to date.
    /// The icons follow once they are read.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.branches = self
            .store
            .projects
            .iter()
            .filter_map(|p| Some((p.dir.clone(), project::branch(&p.dir)?)))
            .collect();
        self.load_icons(cx);
        cx.notify();
    }

    /// Reread the project icons away from the UI thread.
    fn load_icons(&mut self, cx: &mut Context<Self>) {
        let projects: Vec<_> =
            self.store.projects.iter().map(|p| (p.dir.clone(), p.icon.clone())).collect();
        let load = cx.background_executor().spawn(async move {
            projects.into_iter()
                .filter_map(|(dir, chosen)| {
                    let icon = project_icon::load(&dir, chosen.as_deref())?;
                    Some((dir, icon))
                })
                .collect::<HashMap<_, _>>()
        });
        self.icon_load = cx.spawn(async move |this, cx| {
            let icons = load.await;
            let _ = this.update(cx, |this, cx| this.set_icons(icons, cx));
        });
    }

    fn set_icons(&mut self, mut icons: HashMap<PathBuf, Arc<RenderImage>>, cx: &mut Context<Self>) {
        let mut changed = icons.len() != self.icons.len();
        for (dir, old) in self.icons.drain() {
            match icons.get_mut(&dir) {
                // Keep the image already uploaded to the GPU.
                Some(new) if project_icon::same(&old, new) => *new = old,
                _ => {
                    changed = true;
                    cx.drop_image(old, None);
                }
            }
        }
        self.icons = icons;
        if changed {
            cx.notify();
        }
    }

    /// Write the state to disk. Every change to the thread list ends here, so
    /// the sidebar is rebuilt too.
    pub(crate) fn save(&mut self) {
        self.sync_rows();
        if let Some(path) = &self.state_path {
            if let Err(e) = persist::save(path, &self.store) {
                eprintln!("kiln: could not save state: {e}");
            }
        }
    }

    /// Bring the rows up to date with the shown thread, telling the list only
    /// about those that changed so it keeps its place.
    fn sync_transcript(&mut self) {
        let thread = self.selected.and_then(|id| self.store.thread(id));
        let built = thread.map_or((0, false), |t| (t.messages.len(), t.status == Status::Running));
        let switched = self.transcript_thread != self.selected;
        if !switched && self.transcript_built == Some(built) {
            return;
        }
        let entries = thread.map_or_else(Vec::new, |t| {
            transcript::entries(
                &t.messages,
                built.1,
                |at| self.open_work.contains(&(t.id, at)),
                |at| self.open_tool_groups.contains(&(t.id, at)),
            )
        });
        if switched {
            self.transcript.reset(entries.len());
            self.parsed.clear();
            self.transcript_thread = self.selected;
        } else {
            let same = |(a, b): &(&Entry, &Entry)| match (a, b) {
                // A running group can grow without moving the existing rows.
                (Entry::ToolGroup { start: a, .. }, Entry::ToolGroup { start: b, .. }) => a == b,
                _ => a == b,
            };
            let head = self.entries.iter().zip(&entries).take_while(same).count();
            let tail = self.entries[head..].iter().rev()
                .zip(entries[head..].iter().rev())
                .take_while(same)
                .count();
            let old = head..self.entries.len() - tail;
            let new = entries.len() - tail - head;
            if !old.is_empty() || new > 0 {
                self.transcript.splice(old, new);
            }
        }
        self.entries = entries;
        self.transcript_built = Some(built);
    }

    /// Expand or collapse the work starting at message `at`.
    fn toggle_work(&mut self, id: u64, at: usize, cx: &mut Context<Self>) {
        if !self.open_work.remove(&(id, at)) {
            self.open_work.insert((id, at));
        }
        self.anchor_transcript();
        self.transcript_built = None;
        cx.notify();
    }

    /// Keep a disclosure header in view when opening it in a bottom-aligned list.
    fn anchor_transcript(&self) {
        if self.transcript.logical_scroll_top().item_ix >= self.transcript.item_count() {
            let view = self.transcript.viewport_bounds().size.height;
            self.transcript.scroll_by(px(TRANSCRIPT_PADDING * 2.) - view);
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
                    this.new_thread(p, Agent::default(), cx);
                } else {
                    this.save();
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    pub(crate) fn new_thread(&mut self, p: usize, agent: Agent, cx: &mut Context<Self>) {
        if let Some(id) = self.store.add_thread(p, agent, now()) {
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
            self.open_tools.retain(|&(thread, _)| thread != id);
            self.open_tool_groups.retain(|&(thread, _)| thread != id);
            if self.selected == Some(id) {
                self.selected = self.store.first_thread_id();
            }
            self.save();
            cx.notify();
        }
    }

    pub(crate) fn remove_project(&mut self, p: usize, cx: &mut Context<Self>) {
        if self.store.remove_project(p) {
            self.open_tools.retain(|&(id, _)| self.store.locate(id).is_some());
            self.open_tool_groups.retain(|&(id, _)| self.store.locate(id).is_some());
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
        if let Some(Menu::Model { agent, expanded, choosing_provider: true }) = self.menu {
            self.menu = Some(Menu::Model { agent, expanded, choosing_provider: false });
            cx.notify();
        } else if self.menu.take().is_some() {
            cx.notify();
        } else if let Some(id) = self.selected {
            self.stop(id, cx);
        }
    }

    /// Stop the thread's turn. It stays running until the process has exited.
    fn stop(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(turn) = self.turns.get(&id) {
            turn.stop();
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
        thread.auto_allow.clear();
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

    fn resume(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(prompt) = self.store.thread(id).and_then(Thread::resume_prompt) {
            self.transcript_thread = None;
            self.start_turn(id, prompt, vec![], cx);
        }
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
        if self.menu == Some(Menu::Permissions) {
            self.menu = None;
        }
        thread.status = Status::Running;
        let mut granted = allowed_tools;
        let permission_mode = thread.permission_mode;
        let agent = thread.agent;
        let disallowed_tools = if agent == Agent::Claude && permission_mode == PermissionMode::Manual {
            let (web_allowed, web_disallowed) = thread.policy.web_tool_rules();
            granted.extend(web_allowed.iter().map(|t| t.to_string()));
            web_disallowed.iter().map(|t| t.to_string()).collect()
        } else {
            vec![]
        };
        let resume = thread.session_id.clone();
        let model = thread.model();
        let effort = thread.effort().id().to_string();
        let started = match agent {
            Agent::Claude => claude::run(claude::Turn {
                dir, prompt, resume, model: Some(model.to_string()), effort: Some(effort),
                permission_mode, allowed_tools: granted.clone(), disallowed_tools,
            }).map(|(process, rx)| (process, None, rx)),
            Agent::Codex => codex::run(codex::Turn {
                dir, prompt, resume, model: Some(model.to_string()),
                effort, permission_mode, policy: thread.policy.clone(),
            }).map(|(process, client, rx)| (process, Some(client), rx)),
        };
        cx.notify();
        let (process, codex, rx) = match started {
            Ok(started) => started,
            Err(e) => {
                let error = Some(format!("Could not start {}: {e}", agent.name()));
                self.apply(id, AgentEvent::Done { error }, permission_mode, &granted);
                self.save();
                return;
            }
        };
        self.turns.insert(id, RunningTurn {
            process, codex, started: Instant::now(), approvals: Vec::new(),
        });

        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                let done = matches!(ev, AgentEvent::Done { .. } | AgentEvent::Stopped);
                let _ = this.update(cx, |this, cx| {
                    if done {
                        let turn = this.turns.remove(&id);
                        if let (Some(turn), Some(thread)) = (turn, this.store.thread_mut(id)) {
                            thread.record_work(turn.started.elapsed());
                        }
                    }
                    let retry = this.apply(id, ev, permission_mode, &granted);
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
    fn apply(
        &mut self,
        id: u64,
        ev: AgentEvent,
        permission_mode: PermissionMode,
        granted: &[String],
    ) -> Option<Vec<String>> {
        let dir = self.store.project_dir_of(id)?;
        let thread = self.store.thread_mut(id)?;
        match ev {
            AgentEvent::Session(id) => thread.session_id = Some(id),
            AgentEvent::Model(model) => thread.reported_model = Some(model),
            AgentEvent::PermissionMode(mode) => {
                if mode != permission_mode {
                    thread.messages.push(Message::notice(format!(
                        "{} is using {} permissions instead of {}.",
                        thread.agent.name(), permission_label(mode), permission_label(permission_mode)
                    )));
                }
            }
            AgentEvent::Text(text) => thread.messages.push(Message::agent(text).at(now())),
            AgentEvent::Tool { name, input } => {
                thread.messages.push(Message::tool_call(name, input))
            }
            AgentEvent::Notice(text) => thread.messages.push(Message::notice(text)),
            AgentEvent::ApprovalRequested(request) => {
                if let Some(turn) = self.turns.get_mut(&id).filter(|turn| !turn.process.stop_requested()) {
                    turn.approvals.push(request);
                }
            }
            AgentEvent::ApprovalResolved(request) => {
                if let Some(turn) = self.turns.get_mut(&id) {
                    turn.approvals.retain(|approval| approval.id != request);
                }
            }
            AgentEvent::Denied { tool, summary, .. } if permission_mode != PermissionMode::Manual => {
                thread.messages.push(Message::notice(format!(
                    "Blocked by {}: {tool}\n{summary}", thread.agent.name()
                )));
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
                    Decision::Ask => {
                        let approval = Approval { tool, summary, rule };
                        if !thread.pending.contains(&approval) {
                            thread.pending.push(approval);
                        }
                    }
                }
            }
            AgentEvent::Done { error } => {
                if let Some(e) = error {
                    thread.auto_allow.clear();
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
        if thread.status == Status::Running || idx >= thread.pending.len() {
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
        if thread.status == Status::Running || idx >= thread.pending.len() {
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

    pub(crate) fn has_live_approval(&self, id: u64) -> bool {
        self.turns.get(&id).is_some_and(|turn| {
            !turn.approvals.is_empty() && !turn.process.stop_requested()
        })
    }

    fn decide_codex(
        &mut self,
        id: u64,
        started: Instant,
        request_id: &codex::ApprovalId,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(turn) = self.turns.get_mut(&id) else { return };
        if turn.started != started || turn.process.stop_requested() {
            return;
        }
        let Some(client) = &turn.codex else { return };
        let Some(index) = turn.approvals.iter().position(|request| &request.id == request_id) else { return };
        let request = turn.approvals.remove(index);
        let note = match client.respond(&request, allow) {
            Ok(()) => {
                let decision = if allow { "✓ Allowed" } else { "✗ Denied" };
                format!("{decision}: {}\n{}", request.tool, request.summary)
            }
            Err(error) => format!("Could not answer Codex approval: {error}"),
        };
        if let Some(thread) = self.store.thread_mut(id) {
            thread.touch(now());
            thread.messages.push(Message::notice(note));
        }
        self.save();
        cx.notify();
    }

    fn approval_cards(&self, id: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let mut col = div().id("approvals").flex().flex_col().flex_shrink_0()
            .gap_2().px_6().max_h(px(240.)).overflow_y_scroll();
        let mut pending: Vec<_> = self.store.thread(id)
            .filter(|thread| thread.status != Status::Running)
            .into_iter().flat_map(|thread| thread.pending.iter())
            .map(|approval| (approval.tool.clone(), approval.summary.clone(), None))
            .collect();
        if let Some(turn) = self.turns.get(&id).filter(|_| self.has_live_approval(id)) {
            pending.extend(turn.approvals.iter().map(|request| {
                (request.tool.clone(), request.summary.clone(), Some((turn.started, request.id.clone())))
            }));
        }
        for (i, (tool, summary, request)) in pending.into_iter().enumerate() {
            let button = |id: String, label: &'static str, primary: bool| {
                div().id(SharedString::from(id)).px_3().py_1().rounded_md().text_xs()
                    .font_weight(FontWeight::MEDIUM).cursor_pointer()
                    .when(primary, |d| d.bg(theme.accent).text_color(theme.on_accent))
                    .when(!primary, |d| d.bg(theme.selected).text_color(theme.text))
                    .hover(|h| h.opacity(0.85))
                    .child(label)
            };
            let deny_request = request.clone();
            col = col.child(centered(
                div().flex().items_start().gap_2().w_full()
                    .p_3().mb_2().rounded(px(12.)).bg(theme.card)
                    .border_1().border_color(theme.warn).shadow_sm()
                    .child(
                        div().flex().flex_col().flex_1().min_w_0().gap_1()
                            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.warn)
                                .child(format!("{tool} needs approval")))
                            .child(div().font_family(MONO).text_xs().text_color(theme.text)
                                .child(summary)),
                    )
                    .child(button(format!("deny{i}"), "Deny", false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some((started, request)) = &deny_request {
                                this.decide_codex(id, *started, request, false, cx);
                            } else {
                                this.deny(id, i, cx);
                            }
                        })))
                    .child(button(format!("allow{i}"), "Allow", true)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some((started, request)) = &request {
                                this.decide_codex(id, *started, request, true, cx);
                            } else {
                                this.approve(id, i, cx);
                            }
                        }))),
            ).flex_shrink_0());
        }
        col
    }

    fn set_model(&mut self, id: u64, agent: Agent, model: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.store.choose_model(id, agent, model, now()) {
            self.selected = Some(target);
            self.save();
        }
        cx.notify();
    }

    fn set_effort(&mut self, id: u64, effort: Effort, cx: &mut Context<Self>) {
        if self.store.select_effort(id, effort) {
            self.save();
            cx.notify();
        }
    }

    /// Compact picker in the composer footer; the choice applies from the next turn.
    fn model_picker(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        const PREVIEW_LEN: usize = 4;
        const PROVIDER_CHIP_HEIGHT: f32 = 26.;

        let theme = self.theme;
        let (agent, expanded, choosing_provider) = match self.menu {
            Some(Menu::Model { agent, expanded, choosing_provider }) => (agent, expanded, choosing_provider),
            _ => (thread.agent, false, false),
        };
        let state = Menu::Model { agent, expanded, choosing_provider };
        let button = self.footer_button("model-picker", state, cx)
            .child(agent_icon(thread.agent, 13., theme.text))
            .child(div().font_weight(FontWeight::MEDIUM)
                .child(thread.agent.model_name(thread.model()).to_string()));
        if self.menu != Some(state) {
            return self.footer_picker(button, None, window, cx);
        }

        let provider_chip = div().id("choose-model-provider").flex().items_center().gap_1p5()
            .h(px(PROVIDER_CHIP_HEIGHT)).px_2().rounded_md().bg(theme.fill).border_1().border_color(theme.border)
            .text_xs().font_weight(FontWeight::SEMIBOLD).cursor_pointer()
            .when(choosing_provider, |d| d.bg(theme.selected))
            .hover(|h| h.bg(theme.selected))
            .child(agent_icon(agent, 13., theme.text))
            .child(agent.name())
            .child(assets::icon(assets::Icon::ChevronDown, 12.).text_color(theme.muted))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu = Some(Menu::Model { agent, expanded, choosing_provider: !choosing_provider });
                cx.notify();
            }));
        let header = div().flex().items_center().justify_between().px_1().pb_1()
            .child(div().pl_1().text_xs().text_color(theme.muted).child("Models"))
            .child(provider_chip);

        let models = agent.models();
        let list_height = (window.viewport_size().height - px(164.)).clamp(px(56.), px(196.));
        let selected = (agent == thread.agent).then(|| thread.model());
        let preview = &models[..models.len().min(PREVIEW_LEN)];
        let selected_below_preview = models[preview.len()..].iter().find(|m| Some(m.id) == selected);
        let shown = if expanded { models } else { preview };
        let overflows = px(MENU_ROW_HEIGHT * shown.len() as f32) > list_height;
        let id = thread.id;
        let results = div().id(SharedString::from(format!("model-options-{}-{expanded}", agent.label())))
            .flex().flex_col().max_h(list_height).overflow_y_scroll()
            .track_scroll(&self.model_scroll)
            .when(overflows, |d| d.pr(px(SCROLLBAR_GUTTER)))
            .children(shown.iter().enumerate().map(|(index, model)| {
                let model = if !expanded && index == PREVIEW_LEN - 1 {
                    selected_below_preview.unwrap_or(model)
                } else {
                    model
                };
                let model_id = model.id;
                let active = Some(model_id) == selected;
                self.menu_item(model_id, None, model.name, None, active)
                    .flex_shrink_0()
                    .when(active, |d| d.bg(theme.selected).font_weight(FontWeight::MEDIUM))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.set_model(id, agent, model_id, cx);
                        window.focus(&this.input.focus_handle(cx), cx);
                    }))
            }));

        let menu = self.menu_surface("model-menu").relative().w(px(288.)).child(header)
            .when(agent != thread.agent && !thread.can_change_agent(), |d| {
                d.child(div().px_2().pb_1().text_xs().text_color(theme.muted)
                    .child("Selecting a model starts a new thread"))
            })
            .child(div().relative().child(results)
                .when(overflows, |d| d.child(self.model_scrollbar())))
            .when(models.len() > PREVIEW_LEN, |d| {
                let action_color = theme.accent.blend(theme.text.alpha(0.3));
                let (label, icon) = if expanded {
                    ("Show fewer models".to_string(), assets::Icon::Minus)
                } else {
                    (format!("Show all {} models", models.len()), assets::Icon::Plus)
                };
                d.child(div().flex().mt_1().child(div().id("toggle-model-list").flex().items_center().gap_1()
                        .h(px(28.)).px_2().rounded_md().text_xs().text_color(action_color)
                        .cursor_pointer().hover(|h| h.bg(theme.hover))
                        .child(assets::icon(icon, 12.))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reset_model_scroll();
                            this.menu = Some(Menu::Model { agent, expanded: !expanded, choosing_provider: false });
                            cx.notify();
                        }))))
            })
            .when(choosing_provider, |d| {
                // Paint above the model rows without changing the picker's size.
                d.child(self.menu_surface("model-providers").absolute()
                    .top(px(PROVIDER_CHIP_HEIGHT + 8.)).right_2().w(px(152.)).bg(theme.fill)
                    .children(Agent::ALL.map(|provider| {
                        self.menu_item(provider.label(), Some(provider), provider.name(), None, provider == agent)
                            .when(provider == agent, |d| d.bg(theme.selected))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if provider != agent {
                                    this.reset_model_scroll();
                                }
                                this.menu = Some(Menu::Model {
                                    agent: provider,
                                    expanded: provider == agent && expanded,
                                    choosing_provider: false,
                                });
                                cx.notify();
                            }))
                    })))
            });
        self.footer_picker(button, Some(menu), window, cx)
    }

    fn reset_model_scroll(&self) {
        self.model_scroll.set_offset(point(px(0.), px(0.)));
        self.model_scroll_grab.set(None);
    }

    fn model_scrollbar(&self) -> impl IntoElement {
        let scroll = self.model_scroll.clone();
        let grab = self.model_scroll_grab.clone();
        let theme = self.theme;
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                let max_offset = scroll.max_offset().y;
                if max_offset <= px(0.) {
                    return;
                }
                let track_height = bounds.size.height - px(4.);
                let thumb_height = (track_height * (bounds.size.height / (bounds.size.height + max_offset)))
                    .clamp(px(16.), track_height);
                let travel = track_height - thumb_height;
                let track_top = bounds.top() + px(2.);
                let thumb_top = track_top + travel * (-scroll.offset().y / max_offset);
                let track_left = bounds.left() + px(4.);
                let paint_bar = |top, height, color, window: &mut Window| {
                    let mut bar = fill(Bounds::new(point(track_left, top), size(px(3.), height)), color);
                    bar.corner_radii = px(1.5).into();
                    window.paint_quad(bar);
                };
                paint_bar(track_top, track_height, theme.border.opacity(0.65), window);
                paint_bar(thumb_top, thumb_height, theme.muted.alpha(0.56), window);

                let scroll_to = move |y: Pixels, anchor: Pixels, window: &mut Window| {
                    let progress = ((y - track_top - anchor) / travel).clamp(0., 1.);
                    scroll.set_offset(point(px(0.), -max_offset * progress));
                    window.refresh();
                };
                let drag_scroll = scroll_to.clone();
                let start_grab = grab.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || event.button != MouseButton::Left
                        || !hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let within_thumb = event.position.y - thumb_top;
                    let anchor = if (px(0.)..=thumb_height).contains(&within_thumb) {
                        within_thumb
                    } else {
                        thumb_height / 2.
                    };
                    start_grab.set(Some(anchor));
                    scroll_to(event.position.y, anchor, window);
                    cx.stop_propagation();
                });
                let moving_grab = grab.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && event.dragging() {
                        if let Some(anchor) = moving_grab.get() {
                            drag_scroll(event.position.y, anchor, window);
                            cx.stop_propagation();
                        }
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
                    if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                        grab.set(None);
                    }
                });
            },
        )
        .absolute().top_0().right_0().bottom_0().w(px(SCROLLBAR_GUTTER))
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
        for &effort in thread.agent.efforts(thread.model()) {
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
        if matches!(menu, Menu::Model { .. }) {
            self.reset_model_scroll();
        }
        self.menu = (self.menu != Some(menu)).then_some(menu);
        self.icon_rejected = false;
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

    fn menu_surface(&self, id: &'static str) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div().id(id).flex().flex_col().p_1().rounded(px(10.)).bg(theme.card)
            .border_1().border_color(theme.border).shadow_lg().occlude()
    }

    pub(crate) fn menu_panel(&self, id: &'static str, title: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
        self.menu_surface(id).w(px(220.))
            .child(div().flex_shrink_0().px_2().pt_1().pb_1p5().text_xs().font_weight(FontWeight::SEMIBOLD)
                .text_color(self.theme.muted).child(title.into()))
    }

    pub(crate) fn menu_item(
        &self,
        id: impl Into<SharedString>,
        icon: Option<Agent>,
        label: impl Into<SharedString>,
        hint: Option<&'static str>,
        active: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div().id(id.into()).flex().items_center().gap_2().h(px(MENU_ROW_HEIGHT)).px_2().rounded_md().text_sm()
            .text_color(theme.text).cursor_pointer().hover(|h| h.bg(theme.hover))
            .when_some(icon, |d, agent| d.child(agent_icon(agent, 14., theme.text)))
            .child(div().flex_1().child(label.into()))
            .when_some(hint, |d, hint| {
                d.child(div().text_xs().text_color(theme.muted).child(hint))
            })
            .child(div().w(px(12.)).text_xs().text_color(theme.accent)
                .child(if active { "✓" } else { "" }))
    }

    fn set_permission_mode(&mut self, id: u64, mode: PermissionMode) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        if thread.status == Status::Running
            || thread.permission_mode == mode
            || !thread.agent.permission_modes().contains(&mode)
        {
            return;
        }
        thread.permission_mode = mode;
        thread.auto_allow.clear();
        if !thread.pending.is_empty() {
            thread.pending.clear();
            thread.status = Status::Idle;
            thread.messages.push(Message::notice("Pending approvals cleared after permissions changed."));
        }
        self.save();
    }

    fn set_web_access(&mut self, id: u64, access: Decision) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        if thread.status == Status::Running || thread.permission_mode != PermissionMode::Manual {
            return;
        }
        thread.policy.network = access;
        if access == Decision::Deny && !thread.pending.is_empty() {
            thread.pending.retain(|approval| !WEB_TOOLS.contains(&approval.tool.as_str()));
            if thread.pending.is_empty() {
                thread.status = Status::Idle;
            }
        }
        self.save();
    }

    /// Toolbar picker for the next turn's provider mode and Manual web rules.
    fn permission_picker(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let available = thread.status != Status::Running;
        let open = available && self.menu == Some(Menu::Permissions);
        let mode = thread.permission_mode;
        let color = match mode {
            PermissionMode::Manual => theme.ok,
            PermissionMode::Auto => theme.accent,
            PermissionMode::FullAccess => theme.warn,
        };
        let button = div().id("permission-picker").flex().items_center().gap_1p5().px_2p5().py_1()
            .rounded_full().text_xs().text_color(theme.muted).bg(theme.fill)
            .when(open, |d| d.bg(theme.selected).text_color(theme.text))
            .when(available, |d| d.cursor_pointer()
                .hover(|h| h.bg(theme.hover).text_color(theme.text))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Permissions, cx))))
            .child(div().size(px(6.)).rounded_full().bg(color))
            .child(permission_label(mode))
            .when(available, |d| d.child("▾"));
        let picker = div().relative().child(button);
        if !open {
            return picker;
        }
        let id = thread.id;
        let mut menu = self.menu_panel("permission-menu", "Permissions for this thread").w(px(285.)).mt_2();
        for &choice in thread.agent.permission_modes() {
            let (key, detail) = match choice {
                PermissionMode::Manual => ("permissions-manual", "Review approval requests"),
                PermissionMode::Auto => ("permissions-auto", "Provider reviews actions"),
                PermissionMode::FullAccess => ("permissions-full", "Run tools without asking"),
            };
            menu = menu.child(
                self.menu_item(key, None, permission_label(choice), Some(detail), mode == choice)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.set_permission_mode(id, choice);
                        cx.notify();
                    })),
            );
        }
        if mode == PermissionMode::Manual {
            let codex = thread.agent == Agent::Codex;
            menu = menu.child(
                div().mt_1().border_t_1().border_color(theme.border)
                    .px_2().pt_2().pb_1().text_xs().font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted).child(if codex { "Web search" } else { "Web search and fetch" }),
            );
            for (key, access, label) in [
                ("web-ask", Decision::Ask, "Ask first"),
                ("web-allow", Decision::Allow, "Allow"),
                ("web-block", Decision::Deny, "Block"),
            ] {
                if codex && access == Decision::Ask {
                    continue;
                }
                let selected = thread.policy.network == access
                    || (codex && access == Decision::Deny && thread.policy.network == Decision::Ask);
                menu = menu.child(
                    self.menu_item(key, None, label, None, selected)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            this.set_web_access(id, access);
                            cx.notify();
                        })),
                );
            }
        }
        menu = menu.child(
            div().px_2().pt_1p5().pb_1().text_xs().text_color(theme.muted)
                .child("Changes apply to the next turn."),
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

    fn entry(&mut self, id: u64, row: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.entries.get(row) {
            Some(&Entry::Message(ix)) => self.message(id, ix, false, cx),
            Some(&Entry::GroupedTool(ix)) => self.message(id, ix, true, cx),
            Some(&Entry::ToolGroup { start, count }) => self.tool_group_row(id, start, count, cx),
            Some(&Entry::Work { start, reply }) => self.work_row(id, start, reply, cx),
            Some(&Entry::Activity) => self.turn_activity(id).into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn tool_group_row(&self, id: u64, start: usize, count: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let open = self.open_tool_groups.contains(&(id, start));
        let chevron = if open { assets::Icon::ChevronDown } else { assets::Icon::ChevronRight };
        centered(
            div().id(("tool-group", start)).flex().items_center().gap_2().px_3().py_1p5()
                .rounded(px(8.)).bg(theme.fill).border_1().border_color(theme.border)
                .text_xs().text_color(theme.muted).cursor_pointer()
                .hover(|h| h.bg(theme.hover))
                .child(assets::icon(assets::Icon::Layers, 13.))
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text).child(format!("{count} tool calls")))
                .child(assets::icon(chevron, 13.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.open_tool_groups.remove(&(id, start)) {
                        this.open_tool_groups.insert((id, start));
                    }
                    this.anchor_transcript();
                    this.transcript_built = None;
                    cx.notify();
                })),
        )
        .px_6().py_1p5().into_any_element()
    }

    /// Stands for what the agent did before it replied; a click shows or hides
    /// it. Shows how long that took and when the reply arrived.
    fn work_row(&self, id: u64, start: usize, reply: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let messages = self.store.thread(id).map_or(&[][..], |t| &t.messages);
        // The user message the work answers is the one just before it.
        let ran = start.checked_sub(1).and_then(|i| messages.get(i)).map_or(0, |m| m.worked_ms);
        let replied = messages.get(reply)
            .filter(|m| m.at > 0)
            .and_then(|m| Local.timestamp_opt(i64::try_from(m.at).ok()?, 0).single())
            .map(|at| transcript::clock(at, Local::now()));
        let chevron = if self.open_work.contains(&(id, start)) {
            assets::Icon::ChevronDown
        } else {
            assets::Icon::ChevronRight
        };
        // Pulled left by its padding, so the title lines up with the reply.
        let button = div().id(("work", start)).flex().items_center().gap_1().ml(px(-6.))
            .px_1p5().py_0p5().rounded_md().cursor_pointer()
            .hover(|h| h.bg(theme.hover).text_color(theme.text))
            .child(transcript::worked(ran))
            .child(assets::icon(chevron, 13.))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_work(id, start, cx)));
        centered(
            div().flex().flex_col().w_full().gap_1p5().text_xs().text_color(theme.muted)
                .child(
                    div().flex().items_center().justify_between()
                        .child(button)
                        .when_some(replied, |d, time| d.child(time)),
                )
                .child(div().w_full().h(px(1.)).bg(theme.border)),
        )
        .px_6()
        .py_1p5()
        .into_any_element()
    }

    fn message(&mut self, id: u64, ix: usize, grouped: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let Some(m) = self.store.thread(id).and_then(|t| t.messages.get(ix)) else {
            return div().into_any_element();
        };
        let body = div().min_w_0().text_sm().line_height(relative(1.55));
        let content = match (&m.tool, m.role) {
            (Some(name), _) => self.tool_call(id, ix, name, &m.body, cx).into_any_element(),
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
        centered(div().when(grouped, |d| d.pl_5()).child(content)).px_6().py_1p5().into_any_element()
    }

    fn tool_call(
        &self,
        id: u64,
        ix: usize,
        name: &str,
        input: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let open = self.open_tools.contains(&(id, ix));
        let preview = input.lines().next().unwrap_or_default().trim();
        let chevron = if open { assets::Icon::ChevronDown } else { assets::Icon::ChevronRight };
        let header = div().flex().items_center().gap_2().min_w_0().px_3().py_1p5().text_xs()
            .child(assets::icon(assets::Icon::Wrench, 13.))
            .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text).child(name.to_string()))
            .child(div().flex_1().min_w_0().truncate().font_family(MONO)
                .when(!open && !preview.is_empty(), |d| {
                    let mut chars = preview.chars();
                    let mut summary: String = chars.by_ref().take(120).collect();
                    if chars.next().is_some() {
                        summary.push('…');
                    }
                    d.child(summary)
                }));
        let header = if input.is_empty() {
            header.into_any_element()
        } else {
            header.id(("tool", ix))
                .cursor_pointer().hover(|h| h.bg(theme.hover))
                .child(assets::icon(chevron, 13.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.open_tools.remove(&(id, ix)) {
                        this.open_tools.insert((id, ix));
                    }
                    this.anchor_transcript();
                    if let Some(row) = this.entries.iter().position(|entry| {
                        matches!(*entry, Entry::Message(index) | Entry::GroupedTool(index) if index == ix)
                    }) {
                        this.transcript.remeasure_items(row..row + 1);
                    }
                    cx.notify();
                }))
                .into_any_element()
        };
        div().flex().flex_col().w_full().min_w_0().rounded(px(8.)).overflow_hidden().bg(theme.fill)
            .border_1().border_color(theme.border)
            .text_color(theme.muted)
            .child(header)
            .when(open, |d| {
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

    fn resume_button(&self, id: u64, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        self.round_button("resume", true)
            .w_auto().px_2p5().gap_1().text_xs().font_weight(FontWeight::SEMIBOLD)
            .child(assets::icon(assets::Icon::Play, 12.))
            .child("Resume")
            .on_click(cx.listener(move |this, _, window, cx| {
                this.resume(id, cx);
                window.focus(&this.input.focus_handle(cx), cx);
            }))
    }

    fn turn_activity(&self, id: u64) -> impl IntoElement {
        let theme = self.theme;
        let stopping = self.turns.get(&id).is_some_and(|turn| turn.process.stop_requested());
        let waiting = self.has_live_approval(id);
        let (label, color) = if stopping {
            ("Stopping…", theme.muted)
        } else if waiting {
            ("Waiting for approval", theme.warn)
        } else {
            ("Working…", theme.muted)
        };
        let dot = div().size(px(7.)).rounded_full().bg(color);
        let dot = if stopping || waiting {
            dot.into_any_element()
        } else {
            dot.with_animation(
                ("agent-activity", id),
                Animation::new(Duration::from_millis(1200)).repeat().with_max_fps(20.),
                |dot, phase| dot.opacity(1. - 1.2 * phase.min(1. - phase)),
            ).into_any_element()
        };
        centered(div().flex().items_center().gap_2().text_xs().text_color(theme.muted)
            .child(dot)
            .child(label))
            .px_6().py_1p5()
    }

    fn composer(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let running = thread.status == Status::Running;
        let stopping = self.turns.get(&thread.id).is_some_and(|t| t.process.stop_requested());
        let stopped = thread.status == Status::Stopped;
        let has_draft = stopped && !self.input.read(cx).text().trim().is_empty();
        let button = if running {
            self.stop_button(thread.id, !stopping, cx)
        } else if stopped {
            self.resume_button(thread.id, cx)
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
                            .when(running && !stopping, |d| d.child("Esc to stop")))
                        .when(has_draft, |d| d.child(self.send_button(cx)))
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
        let project = self.store.locate(thread.id).map(|(p, _)| &self.store.projects[p]);
        div().flex().flex_shrink_0().items_center().gap_3().px_5().h(px(TOOLBAR))
            .border_b_1().border_color(theme.border)
            .child(
                div().flex().flex_col().flex_1().min_w_0()
                    .child(div().truncate().text_sm().font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text).child(thread.title.clone()))
                    .when_some(project, |d, project| {
                        d.child(self.project_picker(project, window, cx))
                    }),
            )
            .child(self.permission_picker(thread, window, cx))
    }

    fn thread(&self, id: u64, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(thread) = self.store.thread(id) else {
            return self.empty_state(cx).into_any_element();
        };
        let this = cx.entity();
        let messages = list(self.transcript.clone(), move |row, _, cx| {
            this.update(cx, |this, cx| this.entry(id, row, cx))
        })
        .flex_1()
        .min_h_0()
        .py(px(TRANSCRIPT_PADDING));
        div().flex().flex_col().flex_1().min_w_0().h_full().bg(self.theme.window)
            .child(self.toolbar(thread, window, cx))
            .child(messages)
            .child(self.approval_cards(id, cx))
            .child(self.composer(thread, window, cx))
            .into_any_element()
    }
}

pub(crate) fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Manual => "Manual",
        PermissionMode::Auto => "Auto",
        PermissionMode::FullAccess => "Full access",
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

/// The provider's mark at the size of the surrounding text.
pub(crate) fn agent_icon(agent: Agent, size: f32, foreground: gpui::Rgba) -> impl IntoElement {
    let (path, color) = match agent {
        Agent::Claude => (assets::CLAUDE, rgb(0xd97757)),
        Agent::Codex => (assets::OPENAI, foreground),
    };
    svg().path(path).flex_shrink_0().size(px(size)).text_color(color)
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
        self.theme = Theme::new(window.appearance(), self.store.settings.appearance, self.theme.font);
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
        div().relative().flex().size_full().font_family(self.theme.font).text_color(self.theme.text)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev, _, cx| this.on_key(ev, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
            }))
            .child(self.sidebar(window, cx))
            .child(match self.selected {
                Some(id) => self.thread(id, window, cx),
                None => self.empty_state(cx).into_any_element(),
            })
            .children(self.render_settings(cx))
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
