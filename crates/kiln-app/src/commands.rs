//! Keyboard shortcuts, the application menu, and the command palette.
use crate::workspace::sidebar::{Row, NO_PROJECT};
use crate::workspace::Workspace;
use gpui::{
    actions, div, prelude::*, px, relative, rgba, App, Context, FocusHandle, Focusable, KeyBinding,
    KeyDownEvent, Menu, MenuItem, OsAction, ScrollHandle, SharedString, Window,
};
use kiln_core::Status;

actions!(
    kiln,
    [
        Quit,
        Hide,
        Minimize,
        NewThread,
        NextThread,
        PreviousThread,
        FocusComposer,
        StopTurn,
        AddProject,
        OpenCommandPalette,
        PaletteUp,
        PaletteDown,
    ]
);

/// What a shortcut or a palette entry does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    Quit,
    Hide,
    Minimize,
    NewThread,
    NextThread,
    PreviousThread,
    FocusComposer,
    StopTurn,
    AddProject,
    OpenSettings,
}

/// A command as the palette lists it. `chord` is the shortcut after the
/// platform modifier, such as `shift-p`.
struct Entry {
    command: Command,
    label: &'static str,
    chord: Option<&'static str>,
}

const ENTRIES: &[Entry] = &[
    Entry { command: Command::NewThread, label: "New thread", chord: Some("n") },
    Entry { command: Command::NextThread, label: "Next thread", chord: Some("alt-down") },
    Entry { command: Command::PreviousThread, label: "Previous thread", chord: Some("alt-up") },
    Entry { command: Command::FocusComposer, label: "Focus the composer", chord: Some("l") },
    Entry { command: Command::StopTurn, label: "Stop the running turn", chord: None },
    Entry { command: Command::AddProject, label: "Add project", chord: Some("o") },
    Entry { command: Command::OpenSettings, label: "Open settings", chord: Some(",") },
    Entry { command: Command::Minimize, label: "Minimize window", chord: Some("m") },
    Entry { command: Command::Quit, label: "Quit Kiln", chord: Some("q") },
];

/// The shortcut text shown beside a command.
fn hint(chord: &str) -> String {
    let mac = cfg!(target_os = "macos");
    let mut text = String::from(if mac { "⌘" } else { "Ctrl+" });
    for part in chord.split('-') {
        text.push_str(match (part, mac) {
            ("shift", true) => "⇧",
            ("shift", false) => "Shift+",
            ("alt", true) => "⌥",
            ("alt", false) => "Alt+",
            ("up", _) => "↑",
            ("down", _) => "↓",
            _ => "",
        });
        if !matches!(part, "shift" | "alt" | "up" | "down") {
            text.push_str(&part.to_uppercase());
        }
    }
    text
}

pub fn bind_keys(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    // Bindings live for the whole run, so leaking their text once is harmless.
    let key = |chord: &str| -> &'static str { format!("{modifier}-{chord}").leak() };
    cx.bind_keys([
        KeyBinding::new(key("q"), Quit, None),
        KeyBinding::new(key("n"), NewThread, None),
        KeyBinding::new(key("alt-down"), NextThread, None),
        KeyBinding::new(key("alt-up"), PreviousThread, None),
        KeyBinding::new("ctrl-tab", NextThread, None),
        KeyBinding::new("ctrl-shift-tab", PreviousThread, None),
        KeyBinding::new(key("l"), FocusComposer, None),
        KeyBinding::new(key("o"), AddProject, None),
        KeyBinding::new(key("shift-p"), OpenCommandPalette, None),
        KeyBinding::new(key("k"), OpenCommandPalette, None),
        KeyBinding::new(key("m"), Minimize, None),
        // Later than the text field's own bindings, so these win while it is focused.
        KeyBinding::new("up", PaletteUp, Some("Palette > TextInput")),
        KeyBinding::new("down", PaletteDown, Some("Palette > TextInput")),
        KeyBinding::new("ctrl-p", PaletteUp, Some("Palette > TextInput")),
        KeyBinding::new("ctrl-n", PaletteDown, Some("Palette > TextInput")),
    ]);
    if cfg!(target_os = "macos") {
        cx.bind_keys([KeyBinding::new("cmd-h", Hide, None)]);
    }
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    // Closing the last window ends the app, so no process outlives its window.
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

/// The menu bar on macOS, which also lists the shortcuts.
pub fn set_menus(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.set_menus([
        Menu::new("Kiln").items([
            MenuItem::action("Settings…", crate::settings::OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Hide Kiln", Hide),
            MenuItem::separator(),
            MenuItem::action("Quit Kiln", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Thread", NewThread),
            MenuItem::action("Add Project…", AddProject),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Cut", crate::text_input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", crate::text_input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", crate::text_input::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", crate::text_input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Go").items([
            MenuItem::action("Command Palette…", OpenCommandPalette),
            MenuItem::separator(),
            MenuItem::action("Next Thread", NextThread),
            MenuItem::action("Previous Thread", PreviousThread),
            MenuItem::action("Focus the Composer", FocusComposer),
        ]),
        Menu::new("Window").items([MenuItem::action("Minimize", Minimize)]),
    ]);
}

/// A row of the palette.
#[derive(Clone)]
enum Choice {
    Command(Command),
    Thread(u64),
}

pub(crate) struct Palette {
    previous_focus: Option<FocusHandle>,
    selected: usize,
    scroll: ScrollHandle,
}

impl Workspace {
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() || self.settings_modal.is_some() || self.image_lightbox.is_some() {
            return;
        }
        self.menu = None;
        self.palette_input.update(cx, |input, cx| input.clear(cx));
        self.palette = Some(Palette {
            previous_focus: window.focused(cx),
            selected: 0,
            scroll: ScrollHandle::new(),
        });
        window.focus(&self.palette_input.focus_handle(cx), cx);
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = self.palette.take() {
            let focus = palette.previous_focus.unwrap_or_else(|| self.input.focus_handle(cx));
            window.focus(&focus, cx);
            cx.notify();
        }
    }

    /// Run a command, unless a dialog is open that should keep the keyboard.
    pub(crate) fn run(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::Quit => return self.quit(cx),
            Command::Hide => return cx.hide(),
            _ => {}
        }
        if self.settings_modal.is_some() || self.image_lightbox.is_some() {
            return;
        }
        self.close_palette(window, cx);
        match command {
            Command::Quit | Command::Hide => {}
            Command::Minimize => window.minimize_window(),
            Command::NewThread => self.new_thread_here(window, cx),
            Command::NextThread => self.step_thread(1, cx),
            Command::PreviousThread => self.step_thread(-1, cx),
            Command::FocusComposer => window.focus(&self.input.focus_handle(cx), cx),
            Command::StopTurn => {
                if let Some(id) = self.selected {
                    self.stop(id, cx);
                }
            }
            Command::AddProject => self.add_project(cx),
            Command::OpenSettings => self.open_settings(window, cx),
        }
    }

    /// Stop what is running and leave. Dropping the workspace would do the same,
    /// but quitting does not wait for it.
    fn quit(&mut self, cx: &mut Context<Self>) {
        self.stop_all();
        self.save();
        cx.quit();
    }

    /// Start a thread alongside the shown one, or in the first project when
    /// nothing is selected.
    fn new_thread_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = match self.selected {
            Some(id) => self.store.locate(id).map(|(p, _)| p),
            None => (!self.store.projects.is_empty()).then_some(0),
        };
        self.new_thread(project, cx);
        window.focus(&self.input.focus_handle(cx), cx);
    }

    /// Show the thread `by` places down the sidebar, wrapping around.
    fn step_thread(&mut self, by: isize, cx: &mut Context<Self>) {
        let ids: Vec<u64> = self
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Thread { id, .. } => Some(*id),
                Row::Header { .. } => None,
            })
            .collect();
        if ids.is_empty() {
            return;
        }
        let next = match self.selected.and_then(|id| ids.iter().position(|&i| i == id)) {
            Some(at) => (at as isize + by).rem_euclid(ids.len() as isize) as usize,
            None if by > 0 => 0,
            None => ids.len() - 1,
        };
        self.select(Some(ids[next]), cx);
    }

    /// The palette's rows for what is typed: commands, then threads.
    fn palette_choices(&self, cx: &Context<Self>) -> Vec<(Choice, SharedString, Option<String>)> {
        let query = self.palette_input.read(cx).text().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let matches = |label: &str| {
            let label = label.to_lowercase();
            words.iter().all(|word| label.contains(word))
        };
        let running = self
            .selected
            .and_then(|id| self.store.thread(id))
            .is_some_and(|t| t.status == Status::Running);
        let mut choices = Vec::new();
        for entry in ENTRIES {
            if entry.command == Command::StopTurn && !running {
                continue;
            }
            if matches(entry.label) {
                choices.push((
                    Choice::Command(entry.command),
                    entry.label.into(),
                    entry.chord.map(hint),
                ));
            }
        }
        for settled in [false, true] {
            for id in self.store.thread_ids(settled) {
                let Some(thread) = self.store.thread(id) else { continue };
                let project = self.store.project_of(id).map_or(NO_PROJECT, |p| &p.name);
                let title = &thread.title;
                if matches(&format!("{title} {project}")) {
                    choices.push((
                        Choice::Thread(id),
                        format!("Go to {title}").into(),
                        Some(project.to_owned()),
                    ));
                }
            }
        }
        choices
    }

    fn pick(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        match choice {
            Choice::Command(command) => self.run(command, window, cx),
            Choice::Thread(id) => {
                self.close_palette(window, cx);
                self.select(Some(id), cx);
                window.focus(&self.input.focus_handle(cx), cx);
            }
        }
    }

    /// Run the highlighted row, as Enter does.
    pub(crate) fn pick_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = &self.palette else { return };
        let selected = palette.selected;
        if let Some((choice, ..)) = self.palette_choices(cx).into_iter().nth(selected) {
            self.pick(choice, window, cx);
        }
    }

    /// The typed text changed, so the first match is highlighted again.
    pub(crate) fn palette_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(palette) = &mut self.palette {
            palette.selected = 0;
            palette.scroll.scroll_to_item(0);
            cx.notify();
        }
    }

    fn move_palette(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.palette_choices(cx).len();
        let Some(palette) = &mut self.palette else { return };
        if count == 0 {
            return;
        }
        palette.selected = (palette.selected as isize + by).rem_euclid(count as isize) as usize;
        palette.scroll.scroll_to_item(palette.selected);
        cx.notify();
    }

    /// Hook the shortcuts up on the root element.
    pub(crate) fn command_handlers(&self, root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        macro_rules! on {
            ($root:ident, $action:ty, $command:expr) => {
                $root = $root.on_action(cx.listener(|this, _: &$action, window, cx| {
                    this.run($command, window, cx)
                }));
            };
        }
        let mut root = root;
        on!(root, Quit, Command::Quit);
        on!(root, Hide, Command::Hide);
        on!(root, Minimize, Command::Minimize);
        on!(root, NewThread, Command::NewThread);
        on!(root, NextThread, Command::NextThread);
        on!(root, PreviousThread, Command::PreviousThread);
        on!(root, FocusComposer, Command::FocusComposer);
        on!(root, StopTurn, Command::StopTurn);
        on!(root, AddProject, Command::AddProject);
        on!(root, crate::settings::OpenSettings, Command::OpenSettings);
        root.on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| {
            this.open_palette(window, cx);
        }))
        .on_action(cx.listener(|this, _: &PaletteUp, _, cx| this.move_palette(-1, cx)))
        .on_action(cx.listener(|this, _: &PaletteDown, _, cx| this.move_palette(1, cx)))
    }

    pub(crate) fn render_palette(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let palette = self.palette.as_ref()?;
        let theme = self.theme;
        let choices = self.palette_choices(cx);
        let mut list = div()
            .id("palette-list")
            .track_scroll(&palette.scroll)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .max_h(px(320.))
            .p_1();
        if choices.is_empty() {
            list = list.child(
                div().px_2().py_1p5().text_sm().text_color(theme.muted).child("Nothing matches"),
            );
        }
        for (ix, (choice, label, hint)) in choices.into_iter().enumerate() {
            list = list.child(
                div()
                    .id(("palette-choice", ix))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .h(px(30.))
                    .px_2()
                    .rounded_md()
                    .text_sm()
                    .text_color(theme.text)
                    .cursor_pointer()
                    .when(ix == palette.selected, |d| d.bg(theme.selected))
                    .hover(|h| h.bg(theme.hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick(choice.clone(), window, cx)
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when_some(hint, |d, hint| {
                        d.child(div().flex_shrink_0().text_xs().text_color(theme.muted).child(hint))
                    }),
            );
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .pt(relative(0.15))
                .p_4()
                .child(
                    div()
                        .id("palette-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x00000055))
                        .occlude()
                        .on_click(cx.listener(|this, _, window, cx| this.close_palette(window, cx))),
                )
                .child(
                    div()
                        .id("palette")
                        .key_context("Palette")
                        .occlude()
                        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                            if ev.keystroke.key == "escape" {
                                cx.stop_propagation();
                                if !ev.is_held {
                                    this.close_palette(window, cx);
                                }
                            }
                        }))
                        .flex()
                        .flex_col()
                        .w(px(520.))
                        .max_w(relative(1.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.card)
                        .shadow_lg()
                        .child(
                            div()
                                .px_3()
                                .py_2p5()
                                .border_b_1()
                                .border_color(theme.border)
                                .text_sm()
                                .child(self.palette_input.clone()),
                        )
                        .child(list),
                ),
        )
    }
}
