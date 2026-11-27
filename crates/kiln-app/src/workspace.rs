use crate::assets;
use crate::commands::Palette;
use crate::file_reference;
use crate::{markdown, media, output_selection, visualization};
use crate::project_icon;
use crate::settings::SettingsModal;
use crate::shell;
use crate::sidebar::{now, Row};
use crate::text_input::{self, InputEvent, TextInput};
use crate::theme::{ui_font, Theme, MONO};
use crate::tooltip::tip;
use crate::transcript::{self, Entry};
use chrono::{Local, TimeZone};
use gpui::{
    anchored, ClickEvent, canvas, deferred, div, fill, img, list, point, prelude::*, px, relative, rgb, size,
    svg, Anchor, AnchoredPositionMode, Animation, AnimationExt, Bounds, Context, DispatchPhase,
    Entity, FocusHandle, Focusable, FontWeight, HitboxBehavior, IntoElement, KeyDownEvent,
    ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathPromptOptions, Pixels, Render, RenderImage, ScrollHandle, SharedString, Task, Window,
};
use kiln_agent::{
    claude, codex, process::Process, project, worktree, AgentEvent, ApprovalId, ApprovalRequest,
    Client,
};
use kiln_core::checkout::{self, Checkout, MoveRequest, ProjectRequest, Sharing, DEFAULT_COMMIT_TYPE};
use kiln_core::policy::{Decision, PermissionMode};
use kiln_core::persist::{self, Loaded};
use kiln_core::title;
use kiln_core::{
    Attachment, AttachmentContent, Prompt, Fetch, FollowUp, Agent, Effort, Message, Role, Status, Store, Thread, NEW_THREAD_TITLE,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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
/// How long typing pauses before the composer's draft is written to disk.
const DRAFT_SAVE: Duration = Duration::from_secs(1);
const SCROLLBAR_GUTTER: f32 = 10.;

/// A turn in progress.
struct RunningTurn {
    process: Process,
    started: Instant,
    client: Client,
    /// Requests the agent is waiting on, oldest first.
    approvals: Vec<ApprovalRequest>,
    /// The agent asked to change the working directory; the turn is stopped.
    move_request: Option<ThreadMove>,
    /// The message the agent is writing.
    stream: Stream,
}

enum ThreadMove {
    Worktree(MoveRequest),
    Project(ProjectRequest),
}

/// A message arriving in pieces, shown a finished word at a time.
#[derive(Default)]
struct Stream {
    text: String,
    /// How much of `text` is shown; see [`transcript::stream_shown`].
    shown: usize,
    /// The shown text parsed, made when next drawn.
    blocks: Option<Rc<[markdown::Block]>>,
}

impl RunningTurn {
    fn stop(&self) {
        self.client.stop(&self.process);
    }
}

struct FileIndex {
    root: PathBuf,
    loaded_at: Instant,
    files: Option<Vec<file_reference::File>>,
}

pub struct Workspace {
    pub(crate) store: Store,
    /// None disables saving (e.g. the existing file could not be read).
    state_path: Option<std::path::PathBuf>,
    /// Kiln's own folder, which holds the worktrees it makes.
    data_dir: PathBuf,
    pub(crate) selected: Option<u64>,
    /// Each running turn, by thread id.
    turns: HashMap<u64, RunningTurn>,
    pub(crate) input: Entity<TextInput>,
    /// The custom branch prefix in the settings dialog.
    pub(crate) prefix_input: Entity<TextInput>,
    focus: FocusHandle,
    /// Palette for the current appearance preference, refreshed every render.
    pub(crate) theme: Theme,
    pub(crate) settings_modal: Option<SettingsModal>,
    pub(crate) spelling: crate::spelling_settings::SpellingState,
    pub(crate) image_lightbox: Option<crate::image_lightbox::ImageLightbox>,
    pub(crate) palette: Option<Palette>,
    /// The command palette's search field.
    pub(crate) palette_input: Entity<TextInput>,
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
    output_selection: Rc<output_selection::Selection>,
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
    /// Syntax spans for expanded shell commands in the selected thread.
    shell_highlights: HashMap<usize, Option<Vec<shell::Span>>>,
    /// The dropdown menu that is open, if any.
    pub(crate) menu: Option<Menu>,
    model_scroll: ScrollHandle,
    model_scroll_grab: Rc<Cell<Option<Pixels>>>,
    /// The open project menu reports that the picked file is no icon.
    pub(crate) icon_rejected: bool,
    /// The pending write of a changed draft; replacing it restarts the wait.
    draft_save: Task<()>,
    file_index: Option<FileIndex>,
    file_index_load: Task<()>,
    file_suggestions: Vec<String>,
    dismissed_file_query: Option<file_reference::Query>,
    attachment_jobs: HashSet<u64>,
    attachment_errors: HashMap<u64, String>,
    reply_images: HashMap<usize, Vec<(String, media::Picture)>>,
    /// The pending write of a typed branch prefix.
    pub(crate) prefix_save: Task<()>,
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.stop_all();
        // Keep a draft typed in the last second.
        self.save();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Menu {
    NewThread,
    Model { agent: Agent, expanded: bool, choosing_provider: bool },
    Effort,
    Permissions,
    Project,
    Checkout,
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
        let data_dir = path.parent().map(PathBuf::from).unwrap_or_default();
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
        let input = cx.new(|cx| {
            let mut input = TextInput::new("Ask the agent to do something", window, cx);
            input.paste_images = true;
            input.file_references = true;
            input.spelling_theme = Some(theme);
            input
        });
        cx.subscribe(&input, |this, _, ev, cx| match ev {
            InputEvent::Submit => this.submit(false, cx),
            InputEvent::SubmitAlternate => this.submit(true, cx),
            InputEvent::Changed => this.keep_draft(cx),
            InputEvent::CaretMoved => this.suggest_files(cx),
            InputEvent::Images(images) => {
                if let Some(id) = this.selected {
                    let images = images.clone();
                    this.load_attachments(id, move || {
                        images.iter().take(media::COUNT_LIMIT + 1).map(|image| {
                            media::attach_image("Clipboard image".into(), &image.bytes)
                        }).collect()
                    }, cx);
                }
            }
        })
        .detach();
        let prefix_input = cx.new(|cx| TextInput::new("Prefix, such as team/name", window, cx));
        cx.subscribe(&prefix_input, |this, _, ev, cx| {
            if let InputEvent::Changed = ev {
                this.keep_branch_prefix(cx);
            }
        })
        .detach();
        let palette_input =
            cx.new(|cx| TextInput::new("Type a command or a thread", window, cx));
        cx.subscribe_in(&palette_input, window, |this, _, ev, window, cx| match ev {
            InputEvent::Submit => this.pick_selected(window, cx),
            InputEvent::SubmitAlternate | InputEvent::CaretMoved | InputEvent::Images(_) => {}
            InputEvent::Changed => this.palette_changed(cx),
        })
        .detach();
        let mut this = Self {
            store,
            state_path,
            data_dir,
            selected,
            turns: HashMap::new(),
            input,
            prefix_input,
            focus: cx.focus_handle(),
            theme,
            settings_modal: None,
            spelling: Default::default(),
            image_lightbox: None,
            palette: None,
            palette_input,
            rows: Vec::new(),
            thread_list: ListState::new(0, ListAlignment::Top, px(200.)),
            branches: HashMap::new(),
            icons: HashMap::new(),
            icon_load: Task::ready(()),
            transcript: ListState::new(0, ListAlignment::Bottom, px(400.)),
            output_selection: Rc::new(output_selection::Selection::new(cx)),
            entries: Vec::new(),
            transcript_thread: None,
            transcript_built: None,
            open_work: HashSet::new(),
            open_tool_groups: HashSet::new(),
            open_tools: HashSet::new(),
            parsed: HashMap::new(),
            shell_highlights: HashMap::new(),
            menu: None,
            model_scroll: ScrollHandle::new(),
            model_scroll_grab: Rc::default(),
            icon_rejected: false,
            draft_save: Task::ready(()),
            file_index: None,
            file_index_load: Task::ready(()),
            file_suggestions: vec![],
            dismissed_file_query: None,
            attachment_jobs: HashSet::new(),
            attachment_errors: HashMap::new(),
            reply_images: HashMap::new(),
            prefix_save: Task::ready(()),
        };
        this.restore_spelling(cx);
        this.show_draft(cx);
        this.sync_rows();
        this.refresh(cx);
        this
    }

    /// Reread the branches and redraw, which also brings the ages up to date.
    /// The icons follow once they are read.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.branches = self
            .store
            .checkout_dirs()
            .into_iter()
            .filter_map(|dir| Some((project::branch(&dir)?, dir)))
            .map(|(branch, dir)| (dir, branch))
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

    pub(crate) fn dictionary_cache(&self) -> PathBuf {
        self.data_dir.join("dictionaries")
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
            self.output_selection.reset();
            self.transcript.reset(entries.len());
            self.parsed.clear();
            self.reply_images.clear();
            self.shell_highlights.clear();
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
                    this.new_thread(Some(p), cx);
                } else {
                    this.save();
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    /// Start a thread with the last provider chosen.
    pub(crate) fn new_thread(&mut self, p: Option<usize>, cx: &mut Context<Self>) {
        let agent = self.store.last_agent();
        let thread = match p {
            Some(p) => self.store.add_thread(p, agent, now()),
            None => Some(self.store.add_projectless_thread(agent, now())),
        };
        if let Some(id) = thread {
            self.select(Some(id), cx);
        }
    }

    /// Show thread `id` with its draft in the composer. The thread left behind
    /// is discarded if nothing was sent or typed in it.
    pub(crate) fn select(&mut self, id: Option<u64>, cx: &mut Context<Self>) {
        let left = self.selected.filter(|&old| {
            Some(old) != id && !self.attachment_jobs.contains(&old) && self.store.thread(old).is_some_and(Thread::is_untouched)
        });
        let switched = self.selected != id;
        self.selected = id;
        if let Some(thread) = id.and_then(|id| self.store.thread_mut(id)) {
            thread.unread = false;
        }
        if let Some(old) = left {
            self.discard(old);
        }
        if switched {
            self.show_draft(cx);
        }
        self.save();
        cx.notify();
    }

    /// Put the shown thread's draft in the composer.
    fn show_draft(&mut self, cx: &mut Context<Self>) {
        self.file_suggestions.clear();
        self.dismissed_file_query = None;
        let draft = self.selected.and_then(|id| self.store.thread(id)).map_or("", |t| &t.draft);
        let draft = draft.to_owned();
        let checkout = self.selected.and_then(|id| self.checkout_dir(id));
        let scratch = self.selected.map(|id| self.scratch_dir(id));
        self.input.update(cx, |input, cx| {
            input.set_reference_paths(checkout, scratch);
            input.set_text(&draft, cx);
        });
    }

    /// Store the composer's text with the shown thread, and write it once
    /// typing pauses.
    fn keep_draft(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        let Some(thread) = self.selected.and_then(|id| self.store.thread_mut(id)) else {
            return;
        };
        let text = self.input.read(cx).text();
        if thread.draft == text {
            return;
        }
        thread.draft = text.to_owned();
        self.draft_save = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_SAVE).await;
            let _ = this.update(cx, |this, _| this.save());
        });
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
        if !self.discard(id) {
            return;
        }
        if self.selected == Some(id) {
            self.select(self.store.first_thread_id(), cx);
        } else {
            self.save();
            cx.notify();
        }
    }

    /// Remove an idle thread and what was expanded in it, without saving.
    fn discard(&mut self, id: u64) -> bool {
        if !self.store.delete_thread(id) {
            return false;
        }
        self.open_work.retain(|&(thread, _)| thread != id);
        self.open_tools.retain(|&(thread, _)| thread != id);
        self.open_tool_groups.retain(|&(thread, _)| thread != id);
        self.attachment_errors.remove(&id);
        true
    }

    pub(crate) fn remove_project(&mut self, p: usize, cx: &mut Context<Self>) {
        if self.store.remove_project(p) {
            self.open_tools.retain(|&(id, _)| self.store.thread(id).is_some());
            self.open_tool_groups.retain(|&(id, _)| self.store.thread(id).is_some());
            self.attachment_errors.retain(|id, _| self.store.thread(*id).is_some());
            if self.selected.is_some_and(|id| self.store.thread(id).is_none()) {
                self.select(self.store.first_thread_id(), cx);
            }
            self.save();
            cx.notify();
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "tab"
            && ev.keystroke.modifiers == Default::default()
            && self.suggestions_visible(window, cx)
        {
            if let (Some(id), Some(query), Some(path)) = (
                self.selected,
                self.input.read(cx).reference_query(),
                self.file_suggestions.first().cloned(),
            ) {
                cx.stop_propagation();
                if !ev.is_held {
                    self.choose_reference(id, query, &path, window, cx);
                }
                return;
            }
        }

        // A held key must not close a menu and then go on to stop the turn.
        if ev.keystroke.key != "escape" || ev.is_held {
            return;
        }
        if let Some(Menu::Model { agent, expanded, choosing_provider: true }) = self.menu {
            self.menu = Some(Menu::Model { agent, expanded, choosing_provider: false });
            cx.notify();
        } else if self.menu.take().is_some() {
            cx.notify();
        } else if self.suggestions_visible(window, cx) {
            self.dismissed_file_query = self.input.read(cx).reference_query();
            self.file_suggestions.clear();
            cx.notify();
        } else if let Some(id) = self.selected {
            self.stop(id, cx);
        }
    }

    /// Stop the thread's turn. It stays running until the process has exited.
    pub(crate) fn stop(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(turn) = self.turns.get(&id) {
            turn.stop();
            cx.notify();
        }
    }

    /// Ask every running turn to stop.
    pub(crate) fn stop_all(&self) {
        for turn in self.turns.values() {
            turn.stop();
        }
    }

    fn checkout_dir(&self, id: u64) -> Option<PathBuf> {
        self.store.checkout_dir_of(id)
            .or_else(|| self.store.thread(id).map(|_| self.scratch_dir(id)))
    }

    fn scratch_dir(&self, id: u64) -> PathBuf {
        self.data_dir.join("tmp").join(id.to_string())
    }

    fn suggest_files(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).reference_query();
        if self.dismissed_file_query == query && self.dismissed_file_query.is_some() {
            return;
        }
        self.dismissed_file_query = None;
        self.file_suggestions.clear();
        cx.notify();
        let Some(query) = query else { return };
        let Some(root) = self.selected.and_then(|id| self.checkout_dir(id)) else { return };
        if let Some(index) = self.file_index.as_ref().filter(|index| index.root == root) {
            match &index.files {
                Some(files) => {
                    self.file_suggestions = file_reference::matches(files, &query);
                    if index.loaded_at.elapsed() < REFRESH {
                        return;
                    }
                }
                None => return,
            }
        }

        self.file_index = Some(FileIndex { root: root.clone(), loaded_at: Instant::now(), files: None });
        let dir = root.clone();
        let load = cx.background_executor().spawn(async move { file_reference::index(&dir) });
        self.file_index_load = cx.spawn(async move |this, cx| {
            let files = load.await;
            let _ = this.update(cx, |this, cx| {
                let Some(index) = this.file_index.as_mut().filter(|index| index.root == root) else {
                    return;
                };
                index.files = Some(files);
                index.loaded_at = Instant::now();
                this.suggest_files(cx);
            });
        });
    }

    fn suggestions_visible(&self, window: &Window, cx: &Context<Self>) -> bool {
        !self.file_suggestions.is_empty() && self.menu.is_none()
            && self.input.focus_handle(cx).is_focused(window)
            && self.selected.and_then(|id| self.checkout_dir(id)).as_deref()
                == self.file_index.as_ref().map(|index| index.root.as_path())
    }

    fn choose_reference(&mut self, id: u64, query: file_reference::Query, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected != Some(id) || self.input.read(cx).reference_query() != Some(query.clone()) {
            return;
        }
        let exists = self.checkout_dir(id).and_then(|root| media::scoped_path(&root, Path::new(path)))
            .is_some_and(|path| path.is_file());
        if exists {
            self.input.update(cx, |input, cx| input.replace_reference(query, path, cx));
            window.focus(&self.input.focus_handle(cx), cx);
        } else {
            self.file_suggestions.retain(|candidate| candidate != path);
            cx.notify();
        }
    }

    fn file_menu(&self, id: u64, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.suggestions_visible(window, cx) {
            return None;
        }
        let query = self.input.read(cx).reference_query()?;
        let theme = self.theme;
        let width = (window.viewport_size().width - px(328.)).clamp(px(160.), px(440.));
        let menu = self.menu_surface("file-suggestions").w(width).children(
            self.file_suggestions.iter().enumerate().map(|(i, path)| {
                let path = path.clone();
                let query = query.clone();
                div().id(("file-suggestion", i)).flex().items_center().gap_2().h(px(MENU_ROW_HEIGHT))
                    .px_2().rounded_md().text_sm().cursor_pointer()
                    .hover(|d| d.bg(theme.hover))
                    .child({
                        let (icon, tint) = file_reference::file_icon(Path::new(&path));
                        assets::icon(icon, 14.).text_color(tint.map_or(theme.muted, rgb))
                    })
                    .child(div().min_w_0().truncate().child(path.clone()))
                    .when(i == 0, |row| {
                        row.bg(theme.hover).child(
                            div().ml_auto().flex_shrink_0().text_xs().text_color(theme.muted).child("Tab"),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.choose_reference(id, query.clone(), &path, window, cx)))
            }),
        );
        Some(div().absolute().top_0().left_0().child(
            deferred(anchored().anchor(Anchor::BottomLeft).snap_to_window().child(menu.mb_2()))
                .with_priority(2),
        ))
    }

    fn pick_files(&mut self, id: u64, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let _ = this.update(cx, |this, cx| this.add_files(id, paths, cx));
        }).detach();
    }

    fn add_files(&mut self, id: u64, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.load_attachments(id, move || {
            paths.into_iter().take(media::COUNT_LIMIT + 1)
                .map(|path| media::attach_file(&path).map_err(|error| {
                    format!("{} · {error}", path.file_name().unwrap_or(path.as_os_str()).to_string_lossy())
                })).collect()
        }, cx);
    }

    fn set_attachment_error(&mut self, id: u64, error: Option<String>, cx: &mut Context<Self>) {
        self.attachment_errors.remove(&id);
        if let Some(error) = error {
            self.attachment_errors.insert(id, error);
        }
        cx.notify();
    }

    fn load_attachments(
        &mut self,
        id: u64,
        load: impl FnOnce() -> Vec<Result<Attachment, String>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.store.thread(id).is_none() {
            return;
        }
        if !self.attachment_jobs.insert(id) {
            self.set_attachment_error(id, Some("Finish adding the current files before adding more.".into()), cx);
            return;
        }
        self.set_attachment_error(id, None, cx);
        let task = cx.background_executor().spawn(async move { load() });
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            let _ = this.update(cx, |this, cx| {
                this.attachment_jobs.remove(&id);
                let Some(thread) = this.store.thread_mut(id) else { return };
                let mut errors = vec![];
                let draft = &mut thread.draft_attachments;
                let mut size = draft.iter().map(Attachment::stored_bytes).sum::<usize>();
                for result in loaded {
                    match result {
                        Ok(file) => {
                            size += file.stored_bytes();
                            if draft.len() >= media::COUNT_LIMIT || size > media::TOTAL_LIMIT {
                                errors.push("A draft can hold up to 8 attachments and 20 MiB of stored data.".into());
                                break;
                            }
                            draft.push(file);
                        }
                        Err(error) => errors.push(error),
                    }
                }
                this.save();
                this.set_attachment_error(id, (!errors.is_empty()).then(|| errors.join("\n")), cx);
            });
        }).detach();
    }

    fn composer_prompt(&self, thread: &Thread, cx: &Context<Self>) -> Prompt {
        Prompt {
            body: self.input.read(cx).text().trim().to_owned(),
            attachments: thread.draft_attachments.clone(),
        }
    }

    fn clear_composer(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(thread) = self.store.thread_mut(id) {
            thread.draft.clear();
            thread.draft_attachments.clear();
        }
        self.attachment_errors.remove(&id);
        self.input.update(cx, |input, cx| input.clear(cx));
    }

    fn follow_up_mode(&self, alternate: bool) -> FollowUp {
        let mode = self.store.settings.follow_up;
        if alternate { mode.other() } else { mode }
    }

    /// Send a new turn or a follow-up to the thread that is running.
    fn submit(&mut self, alternate: bool, cx: &mut Context<Self>) {
        let Some(id) = self.selected else { return };
        let Some(thread) = self.store.thread(id) else { return };
        if self.attachment_jobs.contains(&id) {
            return;
        }
        let prompt = self.composer_prompt(thread, cx);
        if prompt.is_empty() {
            return;
        }
        self.clear_composer(id, cx);
        if self.store.thread(id).is_some_and(|thread| thread.status == Status::Running) {
            match self.follow_up_mode(alternate) {
                FollowUp::Steer => self.steer(id, prompt, cx),
                FollowUp::Queue => {
                    if let Some(thread) = self.store.thread_mut(id) {
                        thread.enqueue(prompt);
                    }
                    self.save();
                    cx.notify();
                }
            }
        } else {
            self.send(id, prompt, cx);
        }
    }

    fn send(&mut self, id: u64, prompt: Prompt, cx: &mut Context<Self>) {
        let checkout = self.checkout_dir(id);
        let Some(thread) = self.store.thread_mut(id) else { return };
        if thread.title == NEW_THREAD_TITLE {
            let label = if prompt.body.is_empty() {
                prompt.attachments.first().map_or(NEW_THREAD_TITLE, |file| file.name.as_str())
            } else {
                &prompt.body
            };
            thread.title = label.chars().take(40).collect();
        }
        let mut message = Message::user(prompt.body.clone());
        message.attachments = prompt.attachments.clone();
        message.checkout_dir = checkout;
        thread.messages.push(message);
        thread.touch(now());
        self.transcript_thread = None;
        self.save();
        self.start_turn(id, prompt, cx);
    }

    fn steer(&mut self, id: u64, prompt: Prompt, cx: &mut Context<Self>) {
        let sent = match self.turns.get(&id).filter(|turn| !turn.process.stop_requested()) {
            Some(turn) => turn.client.steer(&prompt.body, &prompt.attachments),
            None => Err(std::io::Error::other("the turn is ending")),
        };
        let checkout = self.checkout_dir(id);
        let Some(thread) = self.store.thread_mut(id) else { return };
        match sent {
            Ok(()) => {
                let mut message = Message::user(prompt.body).at(now());
                message.attachments = prompt.attachments;
                message.checkout_dir = checkout;
                thread.messages.push(message);
            }
            // Too late to steer, so the message goes out after the turn instead.
            Err(_) => { thread.enqueue(prompt); }
        }
        self.transcript_thread = None;
        self.save();
        cx.notify();
    }

    /// Start the oldest queued follow-up if the last turn ended well.
    fn send_queued(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(body) = self.store.thread_mut(id).and_then(Thread::next_queued) else {
            return false;
        };
        self.send(id, body, cx);
        true
    }

    /// Put a queued follow-up back in the composer to be changed.
    fn edit_queued(&mut self, id: u64, i: usize, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        if i >= thread.queue.len() {
            return;
        }
        let queued = &thread.queue[i];
        let size = queued.attachments.iter().chain(&thread.draft_attachments)
            .map(Attachment::stored_bytes).sum::<usize>();
        if queued.attachments.len() + thread.draft_attachments.len() > media::COUNT_LIMIT
            || size > media::TOTAL_LIMIT
        {
            self.set_attachment_error(id, Some("Remove draft attachments before editing this follow-up.".into()), cx);
            return;
        }
        let queued = thread.queue.remove(i);
        thread.draft_attachments.extend(queued.attachments);
        let queued = queued.body;
        let draft = self.input.read(cx).text();
        let text = if draft.trim().is_empty() { queued } else { format!("{queued}\n\n{draft}") };
        thread.draft.clone_from(&text);
        self.input.update(cx, |input, cx| input.set_text(&text, cx));
        self.save();
        cx.notify();
    }

    fn remove_queued(&mut self, id: u64, i: usize, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        if i < thread.queue.len() {
            thread.queue.remove(i);
            self.save();
            cx.notify();
        }
    }

    /// Start the first queued follow-up of a thread whose last turn did not
    /// end well, once the user chooses to go on.
    fn resume_queue(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        if thread.status != Status::Running && !thread.queue.is_empty() {
            let body = thread.queue.remove(0);
            self.send(id, body, cx);
        }
    }

    fn resume(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        let Some(prompt) = thread.resume_prompt() else { return };
        // A resent request is already in the thread; a nudge is a new message.
        if thread.session_id.is_some() {
            thread.messages.push(Message::user(prompt.body.clone()).at(now()));
            thread.touch(now());
            self.save();
        }
        self.transcript_thread = None;
        self.start_turn(id, prompt, cx);
    }

    /// Run one agent turn and fold its events into the thread. A turn in the
    /// shared project checkout first learns what else is working there.
    fn start_turn(&mut self, id: u64, request: Prompt, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread(id) else {
            return;
        };
        let project_dir = self.store.project_dir_of(id);
        let scratch_dir = self.scratch_dir(id);
        let dir = project_dir.as_deref().map(|project| thread.checkout.dir(project).to_path_buf());
        let shared = project_dir.is_some() && thread.checkout == Checkout::Project;
        let agent = thread.agent;
        // Changes already there cannot be this thread's before its first reply.
        let first = thread.session_id.is_none() && !thread.messages.iter().any(|m| m.role == Role::Agent);
        let Prompt { body: prompt, attachments } = request;
        let prompt = if let Some(project) = project_dir.as_deref().filter(|_| thread.handoff) {
            checkout::moved_prompt(&thread.messages, &prompt, &thread.checkout.describe(project))
        } else if first {
            format!("{}\n\n{prompt}", title::title_note())
        } else {
            prompt
        };
        let running = if shared { self.store.running_beside(id) } else { vec![] };
        let prefix = self.store.settings.branch_prefix.clone();
        let fetches = self.store.settings.fetch == Fetch::Always;
        if matches!(self.menu, Some(Menu::Permissions | Menu::Checkout)) {
            self.menu = None;
        }
        if let Some(thread) = self.store.thread_mut(id) {
            thread.status = Status::Running;
        }
        cx.notify();
        let prepare = cx.background_executor().spawn(async move {
            let scratch = std::fs::create_dir_all(&scratch_dir)
                .ok().and_then(|()| media::scratch_root(&scratch_dir));
            let dir = match dir {
                Some(dir) => dir,
                None => scratch.clone().ok_or_else(|| format!(
                    "Could not open the thread's scratch directory at {}.", scratch_dir.display()
                ))?,
            };
            if !dir.is_dir() {
                return Err(format!(
                    "The checkout at {} is missing. Choose where this thread runs from its branch \
                     menu above.",
                    dir.display()
                ));
            }
            let mut sharing = Sharing::default();
            if shared && worktree::is_repo(&dir) {
                sharing.running = running;
                if first {
                    sharing.changed_files = worktree::changed_files(&dir).unwrap_or(0);
                }
            }
            let note = match project_dir {
                Some(_) if shared => checkout::sharing_note(&sharing, &prefix, fetches),
                Some(project_dir) => worktree::local_instructions(&project_dir, &dir, agent)
                    .map_err(|e| e.to_string())?,
                None => Some(format!(
                    "<kiln-context>\n\
                     This thread has no project and runs in its own scratch folder. If the task would benefit \
                     from a new project, end your reply with this line before creating any project files:\n\
                     {} {{\"name\": \"my-project\"}}\n\
                     Use up to 80 letters, numbers, hyphens, or underscores, starting with a letter or number. \
                     Kiln uses the user's preferred folder if one is set. Otherwise it asks the user to \
                     choose a parent folder for this repository. After a folder is chosen, Kiln creates a \
                     new Git repository and continues this request there. Scratch files stay in the scratch \
                     folder.\n</kiln-context>",
                    checkout::PROJECT_REQUEST,
                )),
            };
            let prompt = match note {
                Some(note) => format!("{note}\n\n{prompt}"),
                None => prompt,
            };
            let prompt = if let Some(scratch_dir) = scratch {
                let path = scratch_dir.display();
                format!(
                    "This thread's scratch directory for temporary files, scripts, and previews is {path}. \
                     To display a local preview in the conversation, use Markdown image syntax \
                     with its absolute path, for example ![Preview](<{path}/preview.png>).\n\n{prompt}"
                )
            } else {
                prompt
            };
            Ok((dir, Prompt { body: prompt, attachments }))
        });
        cx.spawn(async move |this, cx| {
            let prepared = prepare.await;
            let _ = this.update(cx, |this, cx| this.launch(id, prepared, cx));
        })
        .detach();
    }

    /// Start the agent process of a turn once its checkout has been checked.
    fn launch(
        &mut self,
        id: u64,
        prepared: Result<(PathBuf, Prompt), String>,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.store.thread_mut(id) else {
            return;
        };
        let permission_mode = thread.permission_mode;
        let (dir, Prompt { body: prompt, attachments }) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.apply(id, AgentEvent::Done { error: Some(error) }, permission_mode);
                self.save();
                cx.notify();
                return;
            }
        };
        let agent = thread.agent;
        let policy = thread.policy.clone();
        let resume = thread.session_id.clone();
        let model = thread.model();
        let effort = thread.effort().id().to_string();
        let started = match agent {
            Agent::Claude => claude::run(claude::Turn {
                dir: dir.clone(), prompt, attachments, resume, model: Some(model.to_string()), effort: Some(effort),
                permission_mode, policy,
            }),
            Agent::Codex => codex::run(codex::Turn {
                dir: dir.clone(), prompt, attachments, resume, model: Some(model.to_string()),
                effort, permission_mode, policy,
            }),
        };
        cx.notify();
        let (process, client, rx) = match started {
            Ok(started) => started,
            Err(e) => {
                let error = Some(format!("Could not start {}: {e}", agent.name()));
                self.apply(id, AgentEvent::Done { error }, permission_mode);
                self.save();
                return;
            }
        };
        self.turns.insert(id, RunningTurn {
            process, client, started: Instant::now(), approvals: Vec::new(), move_request: None,
            stream: Stream::default(),
        });

        let scratch_dir = self.scratch_dir(id);
        cx.spawn(async move |this, cx| {
            while let Ok(mut ev) = rx.recv().await {
                if let AgentEvent::Visualization(visual) = ev {
                    let preview = visualization::snapshot(&visual.html, false).await;
                    let dark_preview = if preview.is_ok() {
                        visualization::snapshot(&visual.html, true).await.ok()
                    } else {
                        None
                    };
                    let _ = this.update(cx, |this, cx| {
                        let Some(thread) = this.store.thread_mut(id) else { return };
                        let mut message = Message::agent("").at(now());
                        message.attachments.push(Attachment {
                            name: visual.title,
                            content: AttachmentContent::Visualization { html: visual.html, preview, dark_preview },
                        });
                        thread.messages.push(message);
                        this.save();
                        cx.notify();
                    });
                    continue;
                }
                if let AgentEvent::ImageFile(path) = ev {
                    let root = dir.clone();
                    let scratch = scratch_dir.clone();
                    ev = cx.background_executor().spawn(async move {
                        match media::generated_image(&path, &root, &scratch) {
                            Ok(image) => AgentEvent::Image(image),
                            Err(error) => AgentEvent::Notice(error),
                        }
                    }).await;
                }
                let done = matches!(ev, AgentEvent::Done { .. } | AgentEvent::Stopped);
                let delta = matches!(ev, AgentEvent::TextDelta(_));
                let _ = this.update(cx, |this, cx| {
                    let mut ev = ev;
                    let mut move_request = None;
                    if done {
                        let turn = this.turns.remove(&id);
                        let projectless = this.store.project_of(id).is_none();
                        if let (Some(turn), Some(thread)) = (turn, this.store.thread_mut(id)) {
                            thread.record_work(turn.started.elapsed());
                            // A reply cut short by a stop or a failure keeps what was written.
                            let partial = shown_reply(&turn.stream.text, &thread.checkout, projectless);
                            if !partial.is_empty() {
                                let mut message = Message::agent(partial).at(now());
                                message.checkout_dir = Some(dir.clone());
                                thread.messages.push(message);
                            }
                            // Kiln stopped the turn to move it; a failed turn stays.
                            let ended = matches!(ev, AgentEvent::Stopped | AgentEvent::Done { error: None });
                            if let (true, Some(request)) = (ended, turn.move_request) {
                                move_request = Some(request);
                                ev = AgentEvent::Done { error: None };
                            }
                        }
                    }
                    let shown = |this: &Self| this.turns.get(&id).map(|turn| turn.stream.shown);
                    let before = shown(this);
                    this.apply(id, ev, permission_mode);
                    // Text within an unfinished word changes nothing on screen.
                    if delta && shown(this) == before {
                        return;
                    }
                    if done {
                        this.save();
                        // The turn may have switched branches.
                        this.refresh(cx);
                    }
                    match move_request {
                        Some(ThreadMove::Worktree(request)) => {
                            let fetch = this.store.settings.fetch == Fetch::Always;
                            this.move_to_worktree(id, Some(request), fetch, cx);
                        }
                        Some(ThreadMove::Project(request)) => this.create_thread_project(id, request, cx),
                        None if done && !this.send_queued(id, cx) && this.selected != Some(id) => {
                            // The result waits for the user in a thread out of view.
                            if let Some(thread) = this.store.thread_mut(id) {
                                thread.unread = true;
                                this.save();
                            }
                        }
                        None => {}
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

    /// Make a project for this thread and continue its request in the new checkout.
    fn create_thread_project(&mut self, id: u64, request: ProjectRequest, cx: &mut Context<Self>) {
        if self.store.project_of(id).is_some() {
            return;
        }
        let root = self.store.settings.new_project_root.clone();
        let Some(thread) = self.store.thread_mut(id) else { return };
        let Some(message) = thread.messages.iter().rev().find(|m| m.role == Role::User) else { return };
        let prompt = Prompt { body: message.body.clone(), attachments: message.attachments.clone() };
        thread.status = Status::Running;
        let choose_folder = root.is_none().then(|| cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose Project Folder".into()),
        }));
        cx.spawn(async move |this, cx| {
            let root = match choose_folder {
                Some(chooser) => match chooser.await {
                    Ok(Ok(paths)) => Ok(paths.and_then(|paths| paths.into_iter().next())),
                    Ok(Err(error)) => Err(format!("Could not open the folder picker: {error}")),
                    Err(_) => Ok(None),
                },
                None => Ok(root),
            };
            let created = match root {
                Ok(Some(root)) => cx.background_executor().spawn(async move {
                    worktree::init_project(&root, &request.name)
                        .map(Some)
                        .map_err(|error| format!("Could not create the project: {error}"))
                }).await,
                other => other,
            };
            let _ = this.update(cx, |this, cx| {
                match created {
                    Ok(Some(dir)) => {
                        if !this.store.attach_to_project(id, &dir) {
                            return;
                        }
                        let Some(thread) = this.store.thread_mut(id) else { return };
                        thread.messages.push(Message::notice(format!("Created project at {}", dir.display())));
                        this.save();
                        this.refresh(cx);
                        if this.selected == Some(id) {
                            this.suggest_files(cx);
                        }
                        this.start_turn(id, prompt, cx);
                    }
                    result => {
                        let Some(thread) = this.store.thread_mut(id) else { return };
                        let (status, note) = match result {
                            Err(error) => (Status::NeedsInput, error),
                            _ => (Status::Idle, "Project creation canceled.".into()),
                        };
                        thread.status = status;
                        thread.unread = this.selected != Some(id);
                        thread.messages.push(Message::notice(note));
                        this.save();
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Give the thread a new worktree of its project and run later turns there.
    /// The user's worktrees start from the latest main branch. The agent's
    /// `request` chooses its own start, and its request continues at once.
    /// `fetch` fetches `origin` first.
    pub(crate) fn move_to_worktree(
        &mut self,
        id: u64,
        request: Option<MoveRequest>,
        fetch: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((p, t)) = self.store.locate(id) else { return };
        let project = &self.store.projects[p];
        let thread = &project.threads[t];
        if thread.status == Status::Running {
            return;
        }
        let resume = request.is_some();
        let request = request.unwrap_or_else(|| MoveRequest {
            commit_type: Some(DEFAULT_COMMIT_TYPE.into()),
            ..MoveRequest::default()
        });
        let prefix = self.store.settings.branch_prefix.resolve(request.commit_type.as_deref());
        let plan = worktree::Plan {
            branch: checkout::branch_name(&prefix, &request.name, &thread.title),
            base: match request.base.filter(|base| !base.trim().is_empty()) {
                Some(base) => worktree::Base::Ref(base),
                None if resume => worktree::Base::Head,
                None => worktree::Base::Main,
            },
            carry_changes: request.carry_changes,
            fetch,
        };
        let repo = project.dir.clone();
        let root = worktree::root(&self.data_dir, &project.name);
        // Nothing else may start in the thread while its checkout is made.
        self.store.projects[p].threads[t].status = Status::Running;
        cx.notify();
        let create = cx.background_executor()
            .spawn(async move { worktree::create(&repo, &root, &plan) });
        cx.spawn(async move |this, cx| {
            let created = create.await;
            let _ = this.update(cx, |this, cx| {
                let Some(thread) = this.store.thread_mut(id) else { return };
                thread.touch(now());
                thread.status = Status::Idle;
                let mut next = None;
                match created {
                    Ok(created) => {
                        let mut note = String::new();
                        if let Checkout::Worktree { path, branch, base } = &created.checkout {
                            note = format!("Moved to a new worktree on {branch}, from {base}:\n{}", path.display());
                        }
                        let carried = match created.carried {
                            Some(Ok(0)) => Some("There were no uncommitted changes to carry over.".to_string()),
                            Some(Ok(n)) => Some(format!(
                                "Copied the uncommitted changes in {n} {} here; the project checkout still has them.",
                                if n == 1 { "file" } else { "files" }
                            )),
                            Some(Err(e)) => Some(format!(
                                "Could not carry over the uncommitted changes, so the worktree starts clean: {e}"
                            )),
                            None => None,
                        };
                        let fetched = match created.fetched {
                            Some(Ok(())) => Some("Fetched origin first.".to_string()),
                            Some(Err(e)) => Some(format!(
                                "Could not fetch origin, so it starts from what was fetched before: {e}"
                            )),
                            None => None,
                        };
                        for line in [&fetched, &carried].into_iter().flatten() {
                            note = format!("{note}\n{line}");
                        }
                        thread.messages.push(Message::notice(note));
                        thread.move_to(created.checkout);
                        next = resume
                            .then(|| thread.messages.iter().rev().find(|m| m.role == Role::User))
                            .flatten()
                            .map(|m| Prompt {
                                body: match &carried {
                                    Some(carried) => format!("<kiln-context>\n{carried}\n</kiln-context>\n\n{}", m.body),
                                    None => m.body.clone(),
                                },
                                attachments: m.attachments.clone(),
                            });
                    }
                    Err(e) => {
                        if resume {
                            thread.status = Status::NeedsInput;
                        }
                        thread.messages.push(Message::notice(format!(
                            "Could not create a worktree: {e}"
                        )));
                    }
                }
                this.save();
                this.refresh(cx);
                if let Some(prompt) = next {
                    this.start_turn(id, prompt, cx);
                }
            });
        })
        .detach();
    }

    /// Run the thread's later turns in the project checkout again. Its
    /// worktree stays where it is.
    pub(crate) fn return_to_project(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        let Checkout::Worktree { path, .. } = &thread.checkout else { return };
        if thread.status == Status::Running {
            return;
        }
        let note = format!("Back in the project checkout. The worktree stays at:\n{}", path.display());
        thread.messages.push(Message::notice(note));
        thread.move_to(Checkout::Project);
        thread.touch(now());
        self.save();
        self.refresh(cx);
    }

    /// Remove the thread's worktree, which Git allows only while it holds no
    /// uncommitted or untracked work, and go back to the project checkout. The
    /// branch is kept.
    pub(crate) fn remove_worktree(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(repo) = self.store.project_dir_of(id) else { return };
        let Some(thread) = self.store.thread_mut(id) else { return };
        let Checkout::Worktree { path, branch, .. } = thread.checkout.clone() else { return };
        if thread.status == Status::Running {
            return;
        }
        thread.status = Status::Running;
        cx.notify();
        let at = path.clone();
        let remove = cx.background_executor().spawn(async move { worktree::remove(&repo, &at) });
        cx.spawn(async move |this, cx| {
            let removed = remove.await;
            let _ = this.update(cx, |this, cx| {
                let Some(thread) = this.store.thread_mut(id) else { return };
                thread.status = Status::Idle;
                thread.touch(now());
                let note = match removed {
                    Ok(()) => {
                        thread.move_to(Checkout::Project);
                        format!(
                            "Removed the worktree at {}. The branch {branch} is kept, and the \
                             thread runs in the project checkout again.",
                            path.display()
                        )
                    }
                    Err(e) => format!(
                        "Could not remove the worktree: {e}\nKiln never removes a worktree that \
                         holds uncommitted or untracked work."
                    ),
                };
                thread.messages.push(Message::notice(note));
                this.save();
                this.refresh(cx);
            });
        })
        .detach();
    }

    /// Fold one event of a running turn into its thread.
    fn apply(&mut self, id: u64, ev: AgentEvent, permission_mode: PermissionMode) {
        let root = if matches!(ev, AgentEvent::Text(_)) {
            self.checkout_dir(id)
        } else {
            None
        };
        let projectless = matches!(ev, AgentEvent::Text(_)) && self.store.project_of(id).is_none();
        let Some(thread) = self.store.thread_mut(id) else { return };
        match ev {
            AgentEvent::Session(id) => {
                thread.session_id = Some(id);
                thread.handoff = false;
            }
            AgentEvent::Model(model) => thread.reported_model = Some(model),
            AgentEvent::PermissionMode(mode) => {
                if mode != permission_mode {
                    thread.messages.push(Message::notice(format!(
                        "{} is using {} permissions instead of {}.",
                        thread.agent.name(), permission_label(mode), permission_label(permission_mode)
                    )));
                }
            }
            AgentEvent::Text(text) => {
                let (text, title) = title::take_title(&text);
                if let Some(title) = title {
                    thread.title = title;
                }
                let (text, request) = if projectless {
                    let (text, request) = checkout::take_project_request(&text);
                    (text, request.map(ThreadMove::Project))
                } else if thread.checkout == Checkout::Project {
                    let (text, request) = checkout::take_move_request(&text);
                    (text, request.map(ThreadMove::Worktree))
                } else {
                    (text, None)
                };
                // Nothing may be edited here once the agent chose to move.
                if let (Some(request), Some(turn)) = (request, self.turns.get_mut(&id)) {
                    turn.move_request.get_or_insert(request);
                    turn.approvals.clear();
                    turn.stop();
                }
                if !text.is_empty() {
                    let mut message = Message::agent(text).at(now());
                    message.checkout_dir = root;
                    thread.messages.push(message);
                }
                // The whole message replaces what streamed of it.
                if let Some(turn) = self.turns.get_mut(&id) {
                    turn.stream = Stream::default();
                }
                self.remeasure_activity(id);
            }
            AgentEvent::Image(image) => {
                let mut message = Message::agent("").at(now());
                message.attachments.push(image);
                thread.messages.push(message);
            }
            AgentEvent::ImageFile(_) => {
                thread.messages.push(Message::notice("The generated image is unavailable."));
            }
            AgentEvent::Visualization(_) => unreachable!("visualizations render before applying events"),
            AgentEvent::TextDelta(delta) => {
                let Some(turn) = self.turns.get_mut(&id) else { return };
                let stream = &mut turn.stream;
                stream.text.push_str(&delta);
                let shown = transcript::stream_shown(&stream.text);
                if shown != stream.shown {
                    stream.shown = shown;
                    stream.blocks = None;
                    self.remeasure_activity(id);
                }
            }
            AgentEvent::Tool { name, input, command_bytes } => {
                let mut message = Message::tool_call(name, input);
                message.command_bytes = command_bytes;
                thread.messages.push(message);
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
            AgentEvent::Done { error } => {
                thread.touch(now());
                if let Some(e) = error {
                    thread.status = Status::NeedsInput;
                    thread.messages.push(Message::notice(format!("Error: {e}")));
                } else {
                    thread.status = Status::Idle;
                }
            }
            AgentEvent::Stopped => {
                thread.touch(now());
                thread.stop_turn();
            }
        }
    }

    /// Have the list measure the live row of thread `id` again, as its height changed.
    fn remeasure_activity(&mut self, id: u64) {
        if self.transcript_thread == Some(id) && self.entries.last() == Some(&Entry::Activity) {
            let row = self.entries.len() - 1;
            self.transcript.remeasure_items(row..row + 1);
        }
    }

    pub(crate) fn has_live_approval(&self, id: u64) -> bool {
        self.turns.get(&id).is_some_and(|turn| {
            !turn.approvals.is_empty() && !turn.process.stop_requested()
        })
    }

    pub(crate) fn is_stopping(&self, id: u64) -> bool {
        self.turns.get(&id).is_some_and(|turn| turn.process.stop_requested())
    }

    fn decide(
        &mut self,
        id: u64,
        started: Instant,
        request_id: &ApprovalId,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(turn) = self.turns.get_mut(&id) else { return };
        if turn.started != started || turn.process.stop_requested() {
            return;
        }
        let Some(index) = turn.approvals.iter().position(|request| &request.id == request_id) else { return };
        let request = turn.approvals.remove(index);
        let note = match turn.client.respond(&request, allow) {
            Ok(()) => {
                let decision = if allow { "✓ Allowed" } else { "✗ Denied" };
                format!("{decision}: {}\n{}", request.tool, request.summary)
            }
            Err(error) => format!("Could not answer the approval: {error}"),
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
        let Some(turn) = self.turns.get(&id).filter(|_| self.has_live_approval(id)) else {
            return col;
        };
        for (i, request) in turn.approvals.iter().enumerate() {
            let button = |id: String, label: &'static str, primary: bool| {
                div().id(SharedString::from(id)).px_3().py_1().rounded_md().text_xs()
                    .font_weight(FontWeight::MEDIUM).cursor_pointer()
                    .when(primary, |d| d.bg(theme.accent).text_color(theme.on_accent))
                    .when(!primary, |d| d.bg(theme.selected).text_color(theme.text))
                    .hover(|h| h.opacity(0.85))
                    .child(label)
            };
            let (started, allow_id, deny_id) = (turn.started, request.id.clone(), request.id.clone());
            col = col.child(centered(
                div().flex().items_start().gap_2().w_full()
                    .p_3().mb_2().rounded(px(12.)).bg(theme.card)
                    .border_1().border_color(theme.warn).shadow_sm()
                    .child(
                        div().flex().flex_col().flex_1().min_w_0().gap_1()
                            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.warn)
                                .child(format!("{} needs approval", request.tool)))
                            .child(div().font_family(MONO).text_xs().text_color(theme.text)
                                .child(request.summary.clone())),
                    )
                    .child(button(format!("deny{i}"), "Deny", false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.decide(id, started, &deny_id, false, cx);
                        })))
                    .child(button(format!("allow{i}"), "Allow", true)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.decide(id, started, &allow_id, true, cx);
                        }))),
            ).flex_shrink_0());
        }
        col
    }

    fn set_model(&mut self, id: u64, agent: Agent, model: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.store.choose_model(id, agent, model, now()) {
            if target != id {
                // The new thread is where the typed prompt will be sent.
                let draft = self.store.thread_mut(id).map(|t| std::mem::take(&mut t.draft));
                if let (Some(draft), Some(thread)) = (draft, self.store.thread_mut(target)) {
                    thread.draft = draft;
                }
            }
            self.select(Some(target), cx);
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
        self.save();
    }

    fn set_web_access(&mut self, id: u64, access: Decision) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        if thread.status == Status::Running || thread.permission_mode != PermissionMode::Manual {
            return;
        }
        thread.policy.network = access;
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
        div().flex().flex_col().flex_1().h_full().items_center().justify_center().gap_3()
            .bg(theme.window)
            .child(img(theme.logo).flex_shrink_0().size(px(assets::LOGO_SIZE)))
            .child(
                div().id("empty-new-thread").px_4().py_1p5().rounded_lg().text_sm()
                    .font_weight(FontWeight::MEDIUM).cursor_pointer()
                    .bg(theme.accent).text_color(theme.on_accent).child("New thread")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_thread(None, cx);
                        window.focus(&this.input.focus_handle(cx), cx);
                    })),
            )
    }

    fn entry(&mut self, id: u64, row: usize, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.entries.get(row) {
            Some(&Entry::Message(ix)) => self.message(id, ix, false, window, cx),
            Some(&Entry::GroupedTool(ix)) => self.message(id, ix, true, window, cx),
            Some(&Entry::ToolGroup { start, count }) => self.tool_group_row(id, start, count, cx),
            Some(&Entry::Work { start, reply }) => self.work_row(id, start, reply, cx),
            Some(&Entry::Activity) => self.turn_activity(id, window, cx).into_any_element(),
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

    fn message(&mut self, id: u64, ix: usize, grouped: bool, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let Some(m) = self.store.thread(id).and_then(|t| t.messages.get(ix)) else {
            return div().into_any_element();
        };
        let body = div().min_w_0().text_sm().line_height(relative(1.55));
        let root = m.checkout_dir.clone().or_else(|| self.checkout_dir(id));
        let scratch_dir = self.scratch_dir(id);
        let mut document = output_selection::Document::new(self.output_selection.clone(), ix);
        let content = match (&m.tool, m.role) {
            (Some(name), _) => {
                if shell::is_shell(name) && self.open_tools.contains(&(id, ix)) {
                    self.shell_highlights.entry(ix).or_insert_with(|| {
                        shell::highlights(name, &m.body, m.command_bytes)
                    });
                }
                self.tool_call(id, ix, name, &m.body, &mut document, cx).into_any_element()
            }
            (None, Role::User) => div().flex().justify_end()
                .child(body.max_w(px(560.)).px_3p5().py_2().rounded(px(18.))
                    .bg(theme.accent).text_color(theme.on_accent)
                    .when(!m.body.is_empty(), |d| {
                        d.child(markdown::render_user(&m.body, theme, root.as_deref(), &scratch_dir, &mut document, window, cx))
                    })
                    .when(!m.attachments.is_empty(), |d| {
                        d.child(div().flex().flex_wrap().gap_1()
                            .children(m.attachments.iter().enumerate().map(|(i, attachment)| {
                                attachment_button(format!("sent-{ix}-{i}"), attachment, theme)
                            })))
                    }))
                .into_any_element(),
            (None, Role::Agent) => {
                let blocks = self.parsed.entry(ix).or_insert_with(|| markdown::parse_at(&m.body, root.as_deref()).into());
                let images = self.reply_images.entry(ix).or_insert_with(|| {
                    m.attachments.iter().filter_map(|attachment| match &attachment.content {
                        AttachmentContent::Image { base64, .. } => {
                            Some((attachment.name.clone(), media::Picture::embedded(base64.clone())))
                        }
                        AttachmentContent::Text(_) | AttachmentContent::Visualization { .. } => None,
                    }).collect()
                });
                body.when(!blocks.is_empty(), |d| {
                    d.child(markdown::render(blocks, theme, root.as_deref(), &scratch_dir, &mut document, window, cx))
                })
                    .children(images.iter().enumerate().map(|(i, (name, picture))| {
                        media::inline(picture.clone(), name.clone(), format!("reply-image-{ix}-{i}").into(), theme)
                    }))
                    .children(m.attachments.iter().enumerate().filter_map(|(i, attachment)| {
                        let AttachmentContent::Visualization { html, preview, dark_preview } = &attachment.content else { return None };
                        Some(visualization::inline(
                            format!("visualization-{ix}-{i}"), attachment.name.clone(), html.clone(), preview, theme,
                            dark_preview.as_ref(),
                        ))
                    }))
                    .into_any_element()
            }
            (None, Role::Notice) => body.text_xs().text_color(theme.muted)
                .child(document.plain(m.body.clone()))
                .into_any_element(),
        };
        let body = centered(div().when(grouped, |d| d.pl_5()).child(content)).px_6().py_1p5();
        let highlight = if m.role == Role::User && m.tool.is_none() {
            theme.on_accent
        } else {
            theme.accent
        };
        document.render(body, highlight.opacity(0.3).into()).into_any_element()
    }

    fn tool_call(
        &self,
        id: u64,
        ix: usize,
        name: &str,
        input: &str,
        document: &mut output_selection::Document,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let open = self.open_tools.contains(&(id, ix));
        let preview = input.lines().next().unwrap_or_default().trim();
        let chevron = if open { assets::Icon::ChevronDown } else { assets::Icon::ChevronRight };
        let header = div().flex().items_center().gap_2().min_w_0().px_3().py_1p5().text_xs()
            .child(assets::icon(tool_icon(name), 13.))
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
                let details = div().px_3().py_2().border_t_1().border_color(theme.border)
                    .font_family(MONO).text_xs().line_height(relative(1.6))
                    .text_color(theme.text);
                let details = match self.shell_highlights.get(&ix).and_then(Option::as_deref) {
                    Some(spans) => {
                        let styled = shell::render(input, spans, theme);
                        document.track(input.to_owned().into(), &styled, Vec::new());
                        details.child(styled)
                    }
                    None => details.child(document.plain(input.to_owned())),
                };
                d.child(details)
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
        let ready = self.selected.and_then(|id| self.store.thread(id))
            .is_some_and(|thread| self.composer_ready(thread, cx));
        self.round_button("send", ready)
            .child("↑")
            .tooltip(tip("Send", self.theme))
            .on_click(cx.listener(|this, _, _, cx| this.submit(false, cx)))
    }

    /// Takes the send button's place while a turn runs.
    fn stop_button(&self, id: u64, enabled: bool, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let mark = if enabled { self.theme.on_accent } else { self.theme.muted };
        self.round_button("stop", enabled)
            .child(div().size(px(10.)).rounded(px(2.)).bg(mark))
            .tooltip(tip("Stop", self.theme))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.stop(id, cx);
                // The click took focus; hand it back for the next message.
                window.focus(&this.input.focus_handle(cx), cx);
            }))
    }

    /// The follow-ups waiting for the thread's turn to end.
    fn queue_panel(&self, thread: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let id = thread.id;
        let running = thread.status == Status::Running;
        let mut col = div().flex().flex_col().flex_shrink_0().gap_1().px_6();
        if thread.queue.is_empty() {
            return col;
        }
        let heading = if running {
            "Queued — sent when this turn ends"
        } else {
            "Queued — not sent because the last turn did not finish"
        };
        let mut panel = div().flex().flex_col().gap_1().w_full().p_2().rounded(px(12.))
            .bg(theme.card).border_1().border_color(theme.border)
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted)
                .child(heading));
        for (i, prompt) in thread.queue.iter().enumerate() {
            let button = |id: (&'static str, usize), mark: assets::Icon, label: &'static str| {
                div().id(id).flex().flex_shrink_0().items_center().justify_center().size(px(22.))
                    .rounded_md().text_color(theme.muted).cursor_pointer()
                    .hover(|h| h.bg(theme.hover).text_color(theme.text))
                    .tooltip(tip(label, theme))
                    .child(assets::icon(mark, 14.))
            };
            panel = panel.child(
                div().flex().items_center().gap_1().child(
                    div().flex_1().min_w_0().truncate().text_sm().text_color(theme.text)
                        .child(prompt.body.lines().next().filter(|line| !line.is_empty()).unwrap_or("Attachments").to_string()),
                )
                .when(!running && i == 0, |d| {
                    d.child(button(("queued-send", i), assets::Icon::ArrowUp, "Send now")
                        .on_click(cx.listener(move |this, _, _, cx| this.resume_queue(id, cx))))
                })
                .child(button(("queued-edit", i), assets::Icon::Pencil, "Edit")
                    .on_click(cx.listener(move |this, _, _, cx| this.edit_queued(id, i, cx))))
                .child(button(("queued-remove", i), assets::Icon::X, "Remove")
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_queued(id, i, cx)))),
            );
        }
        col = col.child(centered(panel).flex_shrink_0());
        col
    }

    fn resume_button(&self, id: u64, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        self.round_button("resume", true)
            .child(assets::icon(assets::Icon::Play, 12.))
            .tooltip(tip("Resume", self.theme))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.resume(id, cx);
                window.focus(&this.input.focus_handle(cx), cx);
            }))
    }

    fn turn_activity(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        // The streamed reply takes the index its whole message will have.
        let (key, checkout) = self.store.thread(id)
            .map_or((0, Checkout::Project), |t| (t.messages.len(), t.checkout.clone()));
        let root = self.checkout_dir(id);
        let projectless = self.store.project_of(id).is_none();
        let streamed = self.turns.get_mut(&id).filter(|turn| turn.stream.shown > 0).map(|turn| {
            let stream = &mut turn.stream;
            stream.blocks.get_or_insert_with(|| {
                markdown::parse_at(&shown_reply(&stream.text[..stream.shown], &checkout, projectless), root.as_deref()).into()
            }).clone()
        });
        let stopping = self.is_stopping(id);
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
        let status = centered(div().flex().items_center().gap_2().text_xs().text_color(theme.muted)
            .child(dot)
            .child(label))
            .px_6().py_1p5();
        let mut document = output_selection::Document::new(self.output_selection.clone(), key);
        let content = div().flex().flex_col().w_full()
            .when_some(streamed, |d, blocks| {
                d.child(centered(div().min_w_0().text_sm().line_height(relative(1.55))
                    .child(markdown::render(&blocks, theme, root.as_deref(), &self.scratch_dir(id), &mut document, window, cx)))
                    .px_6().py_1p5())
            })
            .child(status);
        document.render(content, theme.accent.opacity(0.3).into())
    }

    fn composer_ready(&self, thread: &Thread, cx: &Context<Self>) -> bool {
        !self.attachment_jobs.contains(&thread.id)
            && (!thread.draft_attachments.is_empty() || !self.input.read(cx).text().trim().is_empty())
    }

    fn remove_attachment(&mut self, id: u64, i: usize, cx: &mut Context<Self>) {
        if let Some(thread) = self.store.thread_mut(id) {
            if i < thread.draft_attachments.len() {
                thread.draft_attachments.remove(i);
                self.save();
                self.set_attachment_error(id, None, cx);
            }
        }
    }

    fn composer(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let running = thread.status == Status::Running;
        let stopping = self.turns.get(&thread.id).is_some_and(|t| t.process.stop_requested());
        let stopped = thread.status == Status::Stopped;
        let id = thread.id;
        let loading = self.attachment_jobs.contains(&id);
        let typed = self.composer_ready(thread, cx);
        let button = if running && typed && !stopping {
            let mark = match self.follow_up_mode(window.modifiers().control) {
                FollowUp::Steer => assets::Icon::CornerDownRight,
                FollowUp::Queue => assets::Icon::ListEnd,
            };
            let hint = match self.store.settings.follow_up {
                FollowUp::Steer => "Steer the running turn (Enter) · Ctrl+Enter to queue instead",
                FollowUp::Queue => "Queue for after this turn (Enter) · Ctrl+Enter to steer instead",
            };
            self.round_button("follow-up", true)
                .child(assets::icon(mark, 14.))
                .tooltip(tip(hint, theme))
                .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                    this.submit(ev.modifiers().control, cx);
                }))
                .into_any_element()
        } else if running {
            self.stop_button(thread.id, !stopping, cx).into_any_element()
        } else if stopped && !typed && !loading {
            self.resume_button(thread.id, cx).into_any_element()
        } else {
            self.send_button(cx).into_any_element()
        };
        centered(
            div().id("composer-card").relative().flex().flex_col().gap_2().w_full().p_2().rounded(px(16.))
                .bg(theme.card).border_1().border_color(theme.border).shadow_md()
                .on_drop(cx.listener(move |this, files: &gpui::ExternalPaths, _, cx| {
                    this.add_files(id, files.paths().to_vec(), cx);
                }))
                .when(!thread.draft_attachments.is_empty(), |d| {
                    d.child(div().flex().flex_wrap().gap_1().px_2()
                        .children(thread.draft_attachments.iter().enumerate().map(|(i, attachment)| {
                            div().flex().items_center().rounded_md().bg(theme.fill).text_color(theme.text)
                                .child(attachment_button(format!("draft-preview-{i}"), attachment, theme))
                                .child(div().id(("remove-attachment", i)).p_1()
                                    .cursor_pointer().hover(|h| h.bg(theme.hover))
                                    .tooltip(tip("Remove attachment", theme))
                                    .child(assets::icon(assets::Icon::X, 12.))
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(id, i, cx))))
                        })))
                })
                .when_some(self.attachment_errors.get(&id), |d, error| {
                    d.child(div().px_2().text_xs().text_color(theme.warn).child(error.clone()))
                })
                .children(self.file_menu(id, window, cx))
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
                            .child(composer_tool("attach-file", assets::Icon::Paperclip, theme)
                                .tooltip(tip("Attach text files or images", theme))
                                .on_click(cx.listener(move |this, _, _, cx| this.pick_files(id, cx))))
                            .child(self.model_picker(thread, window, cx))
                            .child(self.effort_picker(thread, window, cx)))
                        .child(div().flex_1().min_w_0().truncate().text_xs()
                            .text_color(theme.muted).text_right()
                            .when(loading, |d| d.child("Adding attachments…"))
                            .when(!loading && running && !stopping, |d| d.child("Esc to stop")))
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
        let project = self.store.project_of(thread.id);
        div().flex().flex_shrink_0().items_center().gap_3().px_5().h(px(TOOLBAR))
            .border_b_1().border_color(theme.border)
            .child(
                div().flex().flex_col().flex_1().min_w_0()
                    .child(div().truncate().text_sm().font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text).child(thread.title.clone()))
                    .when(project.is_none(), |d| {
                        d.child(div().text_xs().text_color(theme.muted).child(crate::sidebar::NO_PROJECT))
                    })
                    .when_some(project, |d, project| {
                        d.child(div().flex().min_w_0().items_center().gap_1()
                            .child(self.project_picker(project, window, cx))
                            .children(self.checkout_picker(project, thread, window, cx)))
                    }),
            )
            .child(self.permission_picker(thread, window, cx))
    }

    fn thread(&self, id: u64, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(thread) = self.store.thread(id) else {
            return self.empty_state(cx).into_any_element();
        };
        let this = cx.entity();
        let messages = list(self.transcript.clone(), move |row, window, cx| {
            this.update(cx, |this, cx| this.entry(id, row, window, cx))
        })
        .flex_1()
        .min_h_0()
        .py(px(TRANSCRIPT_PADDING));
        div().flex().flex_col().flex_1().min_w_0().h_full().bg(self.theme.window)
            .child(self.toolbar(thread, window, cx))
            .child(div().flex().flex_col().flex_1().min_h_0()
                .track_focus(&self.output_selection.focus)
                .key_context("Transcript")
                .on_action(cx.listener(|this, _: &text_input::Copy, _, cx| this.output_selection.copy(cx)))
                .child(messages))
            .child(self.approval_cards(id, cx))
            .child(self.queue_panel(thread, cx))
            .child(self.composer(thread, window, cx))
            .into_any_element()
    }
}

fn composer_tool(id: &'static str, icon: assets::Icon, theme: Theme) -> gpui::Stateful<gpui::Div> {
    div().id(id).flex().flex_shrink_0().items_center().justify_center()
        .size(px(26.)).rounded_md().text_color(theme.muted).cursor_pointer()
        .hover(|h| h.bg(theme.hover).text_color(theme.text))
        .child(assets::icon(icon, 14.))
}

fn attachment_button(id: String, attachment: &Attachment, theme: Theme) -> gpui::Stateful<gpui::Div> {
    let preview = attachment.clone();
    let icon = match attachment.content {
        AttachmentContent::Text(_) => assets::Icon::FileText,
        AttachmentContent::Image { .. } | AttachmentContent::Visualization { .. } => assets::Icon::Image,
    };
    div().id(SharedString::from(id)).flex().items_center().gap_1()
        .rounded_md().px_2().py_1().max_w(px(250.)).text_xs()
        .cursor_pointer().hover(|h| h.bg(theme.hover).text_color(theme.text))
        .child(assets::icon(icon, 12.))
        .child(div().min_w_0().truncate().child(attachment.name.clone()))
        .tooltip(tip("Open attachment preview", theme))
        .on_click(move |_, window, cx| media::preview_attachment(&preview, theme, window, cx))
}

fn tool_icon(name: &str) -> assets::Icon {
    use assets::Icon;

    match name {
        name if shell::is_shell(name) => Icon::Terminal,
        "File change" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => Icon::FilePen,
        "Read" => Icon::FileText,
        "Glob" | "Grep" => Icon::FileSearch,
        "Web search" | "WebSearch" | "WebFetch" => Icon::Globe,
        _ => Icon::Wrench,
    }
}

pub(crate) fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Manual => "Manual",
        PermissionMode::Auto => "Auto",
        PermissionMode::FullAccess => "Full access",
    }
}

/// Agent text with Kiln's control lines removed.
fn shown_reply(text: &str, place: &Checkout, projectless: bool) -> String {
    let text = title::take_title(text).0;
    if projectless {
        return checkout::take_project_request(&text).0;
    }
    match place {
        Checkout::Project => checkout::take_move_request(&text).0,
        Checkout::Worktree { .. } => text,
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
        let checkout = self.selected.and_then(|id| self.checkout_dir(id));
        let scratch = self.selected.map(|id| self.scratch_dir(id));
        self.input.update(cx, |input, _| {
            input.set_reference_paths(checkout, scratch);
            input.spelling_theme = Some(theme);
        });
        for input in [&self.input, &self.prefix_input, &self.palette_input] {
            input.update(cx, |input, _| {
                input.colors = text_input::Colors {
                    text: theme.text.into(),
                    placeholder: theme.muted.into(),
                    caret: theme.accent.into(),
                    selection: selection.into(),
                    reference: file_reference::ChipColors {
                        text: theme.text.into(),
                        border: theme.accent.opacity(0.55).into(),
                    },
                }
            });
        }
        self.sync_transcript();
        let control_held = window.modifiers().control;
        // No root background: the macOS sidebar lets the blur through.
        let root = div().relative().flex().size_full().font_family(self.theme.font).text_color(self.theme.text)
            .track_focus(&self.focus)
            .on_modifiers_changed(cx.listener(move |_, ev: &gpui::ModifiersChangedEvent, _, cx| {
                if ev.modifiers.control != control_held {
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(|this, ev, window, cx| this.on_key(ev, window, cx)));
        self.command_handlers(root, cx)
            .child(self.sidebar(window, cx))
            .child(match self.selected {
                Some(id) => self.thread(id, window, cx),
                None => self.empty_state(cx).into_any_element(),
            })
            .children(self.render_settings(cx))
            .children(self.render_palette(cx))
            .children(self.render_image_lightbox(window, cx))
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
