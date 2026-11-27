//! The workspace's settings dialog.
use crate::theme::Theme;
use crate::workspace::{permission_label, Workspace};
use gpui::{
    actions, div, prelude::*, px, relative, rgba, App, Context, Div, FocusHandle, Focusable,
    FontWeight, KeyBinding, KeyDownEvent, PathPromptOptions, Window,
};
use kiln_core::{checkout::BranchPrefix, policy::Decision, Agent, Appearance, Fetch, FollowUp, Settings};
use std::time::Duration;

/// How long typing pauses before a typed setting is written to disk.
const TYPED_SAVE: Duration = Duration::from_secs(1);

/// The kinds of branch prefix offered, apart from the custom text.
#[derive(Clone, Copy, PartialEq)]
enum PrefixKind {
    Kiln,
    CommitType,
    Custom,
}

actions!(kiln, [OpenSettings]);

pub fn bind_keys(cx: &mut App) {
    let shortcut = if cfg!(target_os = "macos") {
        "cmd-,"
    } else {
        "ctrl-,"
    };
    cx.bind_keys([KeyBinding::new(shortcut, OpenSettings, None)]);
}

pub(crate) struct SettingsModal {
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
}

impl Workspace {
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_modal.is_some() {
            return;
        }
        self.palette = None;
        self.menu = None;
        let custom = match &self.store.settings.branch_prefix {
            BranchPrefix::Custom(text) => text.clone(),
            _ => String::new(),
        };
        self.prefix_input.update(cx, |input, cx| input.set_text(&custom, cx));
        let focus = cx.focus_handle();
        let previous_focus = window.focused(cx);
        window.focus(&focus, cx);
        self.settings_modal = Some(SettingsModal {
            focus,
            previous_focus,
        });
        cx.notify();
    }

    /// Use the typed text as the branch prefix, and write it once typing pauses.
    pub(crate) fn keep_branch_prefix(&mut self, cx: &mut Context<Self>) {
        let BranchPrefix::Custom(text) = &mut self.store.settings.branch_prefix else { return };
        let typed = self.prefix_input.read(cx).text().replace('\n', "");
        if *text == typed {
            return;
        }
        *text = typed;
        cx.notify();
        self.prefix_save = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TYPED_SAVE).await;
            let _ = this.update(cx, |this, _| this.save());
        });
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(modal) = self.settings_modal.take() {
            let focus = modal
                .previous_focus
                .unwrap_or_else(|| self.input.focus_handle(cx));
            window.focus(&focus, cx);
            cx.notify();
        }
    }

    fn pick_new_project_root(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a folder for new projects".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                this.store.settings.new_project_root = Some(dir);
                this.save();
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> Option<Div> {
        let modal = self.settings_modal.as_ref()?;
        let theme = self.theme;
        let settings = &self.store.settings;
        let appearance = self.setting_choices(
            "Appearance",
            settings.appearance,
            [
                (Appearance::System, "System"),
                (Appearance::Light, "Light"),
                (Appearance::Dark, "Dark"),
            ],
            |s, value| s.appearance = value,
            cx,
        );
        let permissions = self.setting_choices(
            "Permission mode for new threads",
            settings.permission_mode,
            Agent::default().permission_modes().iter().copied().map(|mode| {
                (mode, permission_label(mode))
            }),
            |s, value| s.permission_mode = value,
            cx,
        );
        let web = self.setting_choices(
            "Web access for new threads in Manual mode",
            settings.web,
            [
                (Decision::Ask, "Ask first"),
                (Decision::Allow, "Allow"),
                (Decision::Deny, "Block"),
            ],
            |s, value| s.web = value,
            cx,
        );
        let projects = div().flex().flex_col().flex_shrink_0().gap_1p5()
            .child(div().text_sm().text_color(theme.muted).child("Folder for new projects and repositories"))
            .child(div().text_xs().text_color(theme.muted)
                .child(settings.new_project_root.as_ref().map_or_else(
                    || "Ask for a folder each time".into(),
                    |dir| dir.display().to_string(),
                )))
            .child(div().flex().gap_1p5()
                .child(settings_button("choose-project-folder", theme, false)
                    .child("Choose folder…")
                    .on_click(cx.listener(|this, _, _, cx| this.pick_new_project_root(cx))))
                .when(settings.new_project_root.is_some(), |d| {
                    d.child(settings_button("reset-project-folder", theme, false)
                        .child("Ask each time")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.store.settings.new_project_root = None;
                            this.save();
                            cx.notify();
                        })))
                }));

        let kind = match &settings.branch_prefix {
            BranchPrefix::Kiln => PrefixKind::Kiln,
            BranchPrefix::CommitType => PrefixKind::CommitType,
            BranchPrefix::Custom(_) => PrefixKind::Custom,
        };
        let typed = self.prefix_input.read(cx).text().replace('\n', "");
        let custom_text = typed.clone();
        let prefix = self.setting_choices(
            "Branch prefix for new worktrees",
            kind,
            [
                (PrefixKind::Kiln, "kiln/"),
                (PrefixKind::CommitType, "Commit type: feat/, fix/, …"),
                (PrefixKind::Custom, "Custom"),
            ],
            move |s, kind| {
                s.branch_prefix = match kind {
                    PrefixKind::Kiln => BranchPrefix::Kiln,
                    PrefixKind::CommitType => BranchPrefix::CommitType,
                    PrefixKind::Custom => BranchPrefix::Custom(custom_text.clone()),
                }
            },
            cx,
        );
        let example = match kind {
            PrefixKind::CommitType => {
                "The agent names the type of its work. Worktrees you make use feat/.".to_string()
            }
            _ => match settings.branch_prefix.resolve(None) {
                p if p.is_empty() => "Branches get no prefix.".to_string(),
                p => format!("Branches look like {p}/add-sidebar."),
            },
        };
        let prefix = prefix
            .when(kind == PrefixKind::Custom, |d| {
                d.child(
                    div().id("branch-prefix-input")
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.focus(&this.prefix_input.focus_handle(cx), cx)
                        }))
                        .px_2p5().py_1p5().rounded_md().border_1().border_color(theme.border)
                        .bg(theme.fill).text_sm().child(self.prefix_input.clone()),
                )
            })
            .child(div().text_xs().text_color(theme.muted).child(example));

        let fetch = self.setting_choices(
            "Fetch origin before a new worktree",
            settings.fetch,
            [(Fetch::Ask, "Ask each time"), (Fetch::Always, "Always"), (Fetch::Never, "Never")],
            |s, value| s.fetch = value,
            cx,
        )
        .child(div().text_xs().text_color(theme.muted).child(match settings.fetch {
            Fetch::Ask => "The branch menu offers both. Agents' worktrees do not fetch.",
            Fetch::Always => "Every new worktree, also an agent's, fetches origin first.",
            Fetch::Never => "New worktrees start from what was fetched before.",
        }));

        let follow_up = self.setting_choices(
            "While an agent works, Enter",
            settings.follow_up,
            [(FollowUp::Queue, "Queues"), (FollowUp::Steer, "Steers")],
            |s, value| s.follow_up = value,
            cx,
        )
        .child(div().text_xs().text_color(theme.muted).child(match settings.follow_up {
            FollowUp::Queue => "Enter saves the message for after the turn; Ctrl+Enter steers.",
            FollowUp::Steer => "Enter sends the message to the running turn; Ctrl+Enter queues it.",
        }));

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .child(
                    div()
                        .id("settings-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x00000055))
                        .occlude()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                        ),
                )
                .child(
                    div()
                        .id("settings-dialog")
                        .track_focus(&modal.focus)
                        .tab_group()
                        .tab_stop(false)
                        .occlude()
                        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                            match ev.keystroke.key.as_str() {
                                "escape" => {
                                    cx.stop_propagation();
                                    if !ev.is_held {
                                        this.close_settings(window, cx);
                                    }
                                }
                                "tab" => {
                                    cx.stop_propagation();
                                    if ev.keystroke.modifiers.shift {
                                        window.focus_prev(cx);
                                    } else {
                                        window.focus_next(cx);
                                    }
                                }
                                _ => {}
                            }
                        }))
                        .flex()
                        .flex_col()
                        .w(px(520.))
                        .max_w(relative(1.))
                        .max_h(relative(1.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.card)
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .justify_between()
                                .px_5()
                                .py_3()
                                .border_b_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Settings"),
                                )
                                .child(
                                    settings_button("close-settings", theme, false)
                                        .child("Done")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.close_settings(window, cx);
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .id("settings-content")
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .p_5()
                                .child(appearance)
                                .child(self.spelling_settings(cx))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_shrink_0()
                                        .gap_4()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(permissions)
                                        .child(web),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_shrink_0()
                                        .gap_4()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(projects)
                                        .child(prefix)
                                        .child(fetch)
                                        .child(follow_up),
                                ),
                        ),
                ),
        )
    }

    fn setting_choices<T: Copy + PartialEq + 'static>(
        &self,
        label: &'static str,
        current: T,
        choices: impl IntoIterator<Item = (T, &'static str)>,
        set: impl Fn(&mut Settings, T) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let theme = self.theme;
        div()
            .id(label)
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_1p5()
            .child(div().text_sm().text_color(theme.muted).child(label))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1p5()
                    .children(choices.into_iter().map(|(value, name)| {
                        let set = set.clone();
                        settings_button(name, theme, current == value)
                            .child(name)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                set(&mut this.store.settings, value);
                                this.save();
                                cx.notify();
                            }))
                    })),
            )
    }
}

pub(crate) fn settings_button(id: &'static str, theme: Theme, selected: bool) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .tab_index(0)
        .flex()
        .items_center()
        .h(px(30.))
        .px_2p5()
        .rounded_md()
        .border_1()
        .text_sm()
        .cursor_pointer()
        .when(selected, |d| {
            d.bg(theme.selected)
                .text_color(theme.text)
                .border_color(theme.accent)
        })
        .when(!selected, |d| {
            d.bg(theme.fill)
                .border_color(theme.border)
                .hover(|d| d.bg(theme.hover))
        })
        .focus_visible(|d| d.border_color(theme.accent).underline())
}
