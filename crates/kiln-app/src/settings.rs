//! The workspace's settings dialog.
use crate::theme::Theme;
use crate::workspace::Workspace;
use gpui::{
    actions, div, prelude::*, px, relative, rgba, App, Context, Div, FocusHandle, Focusable,
    FontWeight, KeyBinding, KeyDownEvent, Window,
};
use kiln_core::{policy::Decision, Appearance, Settings};

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
        self.menu = None;
        let focus = cx.focus_handle();
        let previous_focus = window.focused(cx);
        window.focus(&focus, cx);
        self.settings_modal = Some(SettingsModal {
            focus,
            previous_focus,
        });
        cx.notify();
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
        let web = self.setting_choices(
            "Web access for new threads",
            settings.web,
            [
                (Decision::Ask, "Ask first"),
                (Decision::Allow, "Allow"),
                (Decision::Deny, "Block"),
            ],
            |s, value| s.web = value,
            cx,
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
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_shrink_0()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(web),
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
        set: impl Fn(&mut Settings, T) + Copy + 'static,
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

fn settings_button(id: &'static str, theme: Theme, selected: bool) -> gpui::Stateful<Div> {
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
