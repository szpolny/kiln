//! The workspace's settings dialog.
use crate::theme::Theme;
use crate::workspace::{permission_label, Workspace};
use crate::{
    assets::{icon, Icon},
    theme::MONO,
};
use gpui::{
    actions, div, prelude::*, px, relative, rgba, transparent_black, AnyElement, App, Context, Div,
    FocusHandle, Focusable, FontWeight, KeyBinding, KeyDownEvent, PathPromptOptions, SharedString,
    Window,
};
use kiln_core::{
    checkout::BranchPrefix, policy::Decision, Agent, Appearance, Fetch, FollowUp, Settings,
};
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
        self.prefix_input
            .update(cx, |input, cx| input.set_text(&custom, cx));
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
        let BranchPrefix::Custom(text) = &mut self.store.settings.branch_prefix else {
            return;
        };
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
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
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

        let appearance = setting_row(
            Some(row_icon(theme, Icon::SunMoon)),
            "Appearance",
            None,
            self.segmented(
                "appearance",
                settings.appearance,
                [
                    (Appearance::System, "System", Some(Icon::Monitor)),
                    (Appearance::Light, "Light", Some(Icon::Sun)),
                    (Appearance::Dark, "Dark", Some(Icon::Moon)),
                ],
                |s, value| s.appearance = value,
                cx,
            ),
        );
        let follow_up = setting_row(
            Some(row_icon(theme, Icon::CornerDownLeft)),
            "While an agent works, Enter",
            Some(hint(
                theme,
                match settings.follow_up {
                    FollowUp::Queue => {
                        "Enter saves the message for after the turn; Ctrl+Enter steers."
                    }
                    FollowUp::Steer => {
                        "Enter sends the message to the running turn; Ctrl+Enter queues it."
                    }
                },
            )),
            self.segmented(
                "follow-up",
                settings.follow_up,
                [
                    (FollowUp::Queue, "Queues", None),
                    (FollowUp::Steer, "Steers", None),
                ],
                |s, value| s.follow_up = value,
                cx,
            ),
        );

        let permissions = setting_row(
            Some(row_icon(theme, Icon::ShieldCheck)),
            "Permission mode",
            None,
            self.segmented(
                "permission-mode",
                settings.permission_mode,
                Agent::default()
                    .permission_modes()
                    .iter()
                    .copied()
                    .map(|mode| (mode, permission_label(mode), None)),
                |s, value| s.permission_mode = value,
                cx,
            ),
        );
        let web = setting_row(
            Some(row_icon(theme, Icon::Globe)),
            "Web access in Manual mode",
            None,
            self.segmented(
                "web-access",
                settings.web,
                [
                    (Decision::Ask, "Ask first", None),
                    (Decision::Allow, "Allow", None),
                    (Decision::Deny, "Block", None),
                ],
                |s, value| s.web = value,
                cx,
            ),
        );

        let folder = settings.new_project_root.as_ref().map_or_else(
            || hint(theme, "Ask for a folder each time"),
            |dir| {
                div()
                    .text_xs()
                    .font_family(MONO)
                    .text_color(theme.muted)
                    .child(dir.display().to_string())
                    .into_any_element()
            },
        );
        let projects = setting_row(
            Some(row_icon(theme, Icon::Folder)),
            "Folder for new projects",
            Some(folder),
            div()
                .flex()
                .gap_1p5()
                .when(settings.new_project_root.is_some(), |d| {
                    d.child(
                        settings_button("reset-project-folder", theme, false)
                            .child("Ask each time")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.store.settings.new_project_root = None;
                                this.save();
                                cx.notify();
                            })),
                    )
                })
                .child(
                    settings_button("choose-project-folder", theme, false)
                        .gap_1p5()
                        .child(icon(Icon::FolderOpen, 14.))
                        .child(if settings.new_project_root.is_some() {
                            "Change…"
                        } else {
                            "Choose…"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.pick_new_project_root(cx))),
                ),
        );

        let kind = match &settings.branch_prefix {
            BranchPrefix::Kiln => PrefixKind::Kiln,
            BranchPrefix::CommitType => PrefixKind::CommitType,
            BranchPrefix::Custom(_) => PrefixKind::Custom,
        };
        let custom_text = self.prefix_input.read(cx).text().replace('\n', "");
        let example = match kind {
            PrefixKind::CommitType => {
                "The agent names the type of its work. Worktrees you make use feat/.".to_string()
            }
            _ => match settings.branch_prefix.resolve(None) {
                p if p.is_empty() => "Branches get no prefix.".to_string(),
                p => format!("Branches look like {p}/add-sidebar."),
            },
        };
        let prefix = setting_row(
            Some(row_icon(theme, Icon::GitBranch)),
            "Branch prefix",
            Some(hint(theme, example)),
            self.segmented(
                "branch-prefix",
                kind,
                [
                    (PrefixKind::Kiln, "kiln/", None),
                    (PrefixKind::CommitType, "feat/, fix/, …", None),
                    (PrefixKind::Custom, "Custom", None),
                ],
                move |s, kind| {
                    s.branch_prefix = match kind {
                        PrefixKind::Kiln => BranchPrefix::Kiln,
                        PrefixKind::CommitType => BranchPrefix::CommitType,
                        PrefixKind::Custom => BranchPrefix::Custom(custom_text.clone()),
                    }
                },
                cx,
            ),
        )
        .when(kind == PrefixKind::Custom, |d| {
            d.child(
                div()
                    .id("branch-prefix-input")
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.prefix_input.focus_handle(cx), cx)
                    }))
                    .px_2p5()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.window)
                    .text_sm()
                    .font_family(MONO)
                    .child(self.prefix_input.clone()),
            )
        });

        let fetch = setting_row(
            Some(row_icon(theme, Icon::CloudDownload)),
            "Fetch origin first",
            Some(hint(
                theme,
                match settings.fetch {
                    Fetch::Ask => "The branch menu offers both. Agents' worktrees do not fetch.",
                    Fetch::Always => "Every new worktree, also an agent's, fetches origin first.",
                    Fetch::Never => "New worktrees start from what was fetched before.",
                },
            )),
            self.segmented(
                "fetch",
                settings.fetch,
                [
                    (Fetch::Ask, "Ask each time", None),
                    (Fetch::Always, "Always", None),
                    (Fetch::Never, "Never", None),
                ],
                |s, value| s.fetch = value,
                cx,
            ),
        );

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
                        .w(px(580.))
                        .max_w(relative(1.))
                        .max_h(relative(1.))
                        .rounded(px(14.))
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
                                .gap_5()
                                .px_5()
                                .pt_4()
                                .pb_5()
                                .child(settings_section(
                                    theme,
                                    "General",
                                    [appearance, follow_up, self.spelling_settings(cx)],
                                ))
                                .child(settings_section(theme, "New threads", [permissions, web]))
                                .child(settings_section(
                                    theme,
                                    "Projects and worktrees",
                                    [projects, prefix, fetch],
                                )),
                        ),
                ),
        )
    }

    /// A segmented control that writes the chosen value to the settings.
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        current: T,
        choices: impl IntoIterator<Item = (T, &'static str, Option<Icon>)>,
        set: impl Fn(&mut Settings, T) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let theme = self.theme;
        div()
            .id(id)
            .flex()
            .flex_shrink_0()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.window)
            .children(choices.into_iter().map(|(value, name, glyph)| {
                let set = set.clone();
                let on = current == value;
                div()
                    .id(name)
                    .tab_index(0)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(24.))
                    .px_2p5()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(transparent_black())
                    .text_sm()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme.selected).text_color(theme.text))
                    .when(!on, |d| {
                        d.text_color(theme.muted)
                            .hover(|d| d.text_color(theme.text))
                    })
                    .focus_visible(|d| d.border_color(theme.accent))
                    .children(glyph.map(|glyph| icon(glyph, 13.)))
                    .child(name)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        set(&mut this.store.settings, value);
                        this.save();
                        cx.notify();
                    }))
            }))
    }
}

/// A titled group of setting rows on one card.
pub(crate) fn settings_section(
    theme: Theme,
    title: &'static str,
    rows: impl IntoIterator<Item = Div>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap_2()
        .child(
            div()
                .px_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted)
                .child(title.to_uppercase()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(10.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.fill)
                .children(rows.into_iter().enumerate().map(|(i, row)| {
                    row.when(i > 0, |d| d.border_t_1().border_color(theme.border))
                })),
        )
}

/// One setting: an icon, its label and hint on the left, its control on the right.
pub(crate) fn setting_row(
    lead: Option<Div>,
    label: impl IntoElement,
    hint: Option<AnyElement>,
    control: impl IntoElement,
) -> Div {
    div().flex().flex_col().gap_2().px_3p5().py_2p5().child(
        div()
            .flex()
            .items_center()
            .gap_3()
            .children(lead)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .child(label)
                    .children(hint),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .ml_1()
                    .child(control),
            ),
    )
}

/// The tile at the start of a setting row.
fn row_tile(theme: Theme) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded(px(7.))
        .bg(theme.selected)
        .text_color(theme.muted)
}

/// A row tile holding an icon.
pub(crate) fn row_icon(theme: Theme, glyph: Icon) -> Div {
    row_tile(theme).child(icon(glyph, 15.))
}

/// Muted explanatory text under a setting's label.
pub(crate) fn hint(theme: Theme, text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_xs()
        .text_color(theme.muted)
        .child(text.into())
        .into_any_element()
}

pub(crate) fn settings_button(
    id: &'static str,
    theme: Theme,
    selected: bool,
) -> gpui::Stateful<Div> {
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
