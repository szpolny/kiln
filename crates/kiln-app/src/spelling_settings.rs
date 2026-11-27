//! Manage the composer dictionaries from the Settings dialog.

use crate::{
    settings::settings_button,
    spelling::Dictionaries,
    spelling_download::{Language, LANGUAGES},
    workspace::Workspace,
};
use gpui::{div, prelude::*, Context, Div};
use spellbook::Dictionary;
use std::{collections::HashMap, sync::Arc};

#[derive(Default)]
pub(crate) struct SpellingState {
    restored: bool,
    dictionaries: HashMap<&'static str, Status>,
}

enum Status {
    Downloading,
    Ready(Arc<Dictionary>),
    Failed(String),
}

impl Workspace {
    pub(crate) fn restore_spelling(&mut self, cx: &mut Context<Self>) {
        self.update_spelling(cx);
        let dir = self.dictionary_cache();
        cx.spawn(async move |this, cx| {
            let cached = blocking::unblock(move || {
                LANGUAGES
                    .iter()
                    .filter_map(|language| {
                        let status = match language.cached(&dir)? {
                            Ok(dictionary) => Status::Ready(dictionary),
                            Err(error) => Status::Failed(error),
                        };
                        Some((language.code, status))
                    })
                    .collect()
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.spelling.dictionaries = cached;
                this.spelling.restored = true;
                this.update_spelling(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn update_spelling(&self, cx: &mut Context<Self>) {
        let settings = &self.store.settings.spelling;
        let additional = settings
            .languages
            .iter()
            .rev()
            .filter_map(|code| match self.spelling.dictionaries.get(code.as_str()) {
                Some(Status::Ready(dictionary)) => Some(dictionary.clone()),
                _ => None,
            })
            .collect();
        self.input.update(cx, |input, cx| {
            input.set_dictionaries(Dictionaries::new(settings.english, additional), cx);
        });
    }

    fn toggle_spelling_language(&mut self, language: &'static Language, cx: &mut Context<Self>) {
        let enabled = &mut self.store.settings.spelling.languages;
        if let Some(index) = enabled.iter().position(|code| code == language.code) {
            enabled.remove(index);
        } else {
            enabled.push(language.code.into());
        }
        self.save();
        self.update_spelling(cx);
        cx.notify();
    }

    fn download_spelling_language(&mut self, language: &'static Language, cx: &mut Context<Self>) {
        let entries = &mut self.spelling.dictionaries;
        if !self.spelling.restored
            || matches!(
                entries.get(language.code),
                Some(Status::Downloading | Status::Ready(_))
            )
        {
            return;
        }
        entries.insert(language.code, Status::Downloading);
        cx.notify();

        let dir = self.dictionary_cache();
        cx.spawn(async move |this, cx| {
            let result = blocking::unblock(move || language.download(&dir)).await;
            let _ = this.update(cx, |this, cx| {
                let status = match result {
                    Ok(dictionary) => Status::Ready(dictionary),
                    Err(error) => Status::Failed(error),
                };
                let ready = matches!(&status, Status::Ready(_));
                this.spelling.dictionaries.insert(language.code, status);
                if ready {
                    let enabled = &mut this.store.settings.spelling.languages;
                    if !enabled.iter().any(|code| code == language.code) {
                        enabled.push(language.code.into());
                    }
                    this.save();
                    this.update_spelling(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn spelling_settings(&self, cx: &mut Context<Self>) -> Div {
        let theme = self.theme;
        let settings = &self.store.settings.spelling;
        let english = settings.english;
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_1p5()
            .pt_4()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted)
                    .child("Spell check languages"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted)
                    .child("Additional dictionaries download from LibreOffice and work offline."),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child("English (US)")
                            .child(div().text_xs().text_color(theme.muted).child("Built in")),
                    )
                    .child(
                        settings_button("spelling-english", theme, english)
                            .child(if english { "On" } else { "Off" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.store.settings.spelling.english =
                                    !this.store.settings.spelling.english;
                                this.save();
                                this.update_spelling(cx);
                                cx.notify();
                            })),
                    ),
            )
            .children(LANGUAGES.iter().map(|language| {
                let status = self.spelling.dictionaries.get(language.code);
                let control = match status {
                    Some(Status::Ready(_)) => {
                        let enabled = settings.languages.iter().any(|code| code == language.code);
                        settings_button(language.code, theme, enabled)
                            .child(if enabled { "On" } else { "Off" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_spelling_language(language, cx)
                            }))
                            .into_any_element()
                    }
                    Some(Status::Downloading) => div()
                        .px_2p5()
                        .text_sm()
                        .text_color(theme.muted)
                        .child("Downloading…")
                        .into_any_element(),
                    _ if !self.spelling.restored => div()
                        .px_2p5()
                        .text_sm()
                        .text_color(theme.muted)
                        .child("Loading…")
                        .into_any_element(),
                    _ => settings_button(language.code, theme, false)
                        .child(if matches!(status, Some(Status::Failed(_))) {
                            "Retry"
                        } else {
                            "Download"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.download_spelling_language(language, cx)
                        }))
                        .into_any_element(),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_sm().child(language.name))
                            .child(control),
                    )
                    .when_some(
                        status.and_then(|status| match status {
                            Status::Failed(error) => Some(error),
                            _ => None,
                        }),
                        |row, error| {
                            row.child(div().text_xs().text_color(theme.warn).child(error.clone()))
                        },
                    )
            }))
    }
}
