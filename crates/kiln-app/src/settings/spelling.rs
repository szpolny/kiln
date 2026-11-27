//! Manage the composer dictionaries from the Settings dialog.

use super::{hint, row_icon, setting_row};
use crate::{
    assets::{icon, Icon},
    spelling::{
        download::{Language, LANGUAGES},
        Dictionaries,
    },
    theme::Theme,
    workspace::Workspace,
};
use gpui::{div, prelude::*, px, Context, Div};
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

    /// The spell check row: one chip per language, toggled or downloaded in place.
    pub(crate) fn spelling_settings(&self, cx: &mut Context<Self>) -> Div {
        let theme = self.theme;
        let settings = &self.store.settings.spelling;
        let english = chip(
            theme,
            "spelling-english",
            "English (US)",
            Chip::Toggle(settings.english),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.store.settings.spelling.english = !this.store.settings.spelling.english;
            this.save();
            this.update_spelling(cx);
            cx.notify();
        }));
        let languages =
            LANGUAGES.iter().map(|language| {
                let status = self.spelling.dictionaries.get(language.code);
                let state = match status {
                    Some(Status::Ready(_)) => {
                        Chip::Toggle(settings.languages.iter().any(|code| code == language.code))
                    }
                    Some(Status::Downloading) => Chip::Busy,
                    _ if !self.spelling.restored => Chip::Busy,
                    Some(Status::Failed(_)) => Chip::Failed,
                    None => Chip::Download,
                };
                let chip = chip(theme, language.code, language.name, state);
                match state {
                    Chip::Toggle(_) => chip.on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_spelling_language(language, cx)
                    })),
                    Chip::Download | Chip::Failed => {
                        chip.on_click(cx.listener(move |this, _, _, cx| {
                            this.download_spelling_language(language, cx)
                        }))
                    }
                    Chip::Busy => chip,
                }
            });
        let errors = LANGUAGES.iter().filter_map(|language| {
            match self.spelling.dictionaries.get(language.code) {
                Some(Status::Failed(error)) => Some(
                    div()
                        .text_xs()
                        .text_color(theme.warn)
                        .child(format!("{}: {error}", language.name)),
                ),
                _ => None,
            }
        });
        setting_row(
            Some(row_icon(theme, Icon::SpellCheck)),
            "Spell check",
            Some(hint(
                theme,
                "Click a language to turn it on or off. Others download from LibreOffice and then work offline.",
            )),
            div(),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1p5()
                .pl(px(40.))
                .child(english)
                .children(languages),
        )
        .children(errors.map(|error| error.pl(px(40.))))
    }
}

/// What a language chip shows and does when clicked.
#[derive(Clone, Copy)]
enum Chip {
    /// The dictionary is here; on or off.
    Toggle(bool),
    /// Not downloaded yet.
    Download,
    /// Downloading, or still checking what is downloaded.
    Busy,
    /// The download failed; clicking retries.
    Failed,
}

fn chip(theme: Theme, id: &'static str, name: &'static str, state: Chip) -> gpui::Stateful<Div> {
    let glyph = match state {
        Chip::Toggle(true) => Some(Icon::Check),
        Chip::Toggle(false) => None,
        Chip::Download => Some(Icon::Download),
        Chip::Busy => Some(Icon::LoaderCircle),
        Chip::Failed => Some(Icon::RotateCw),
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1()
        .h(px(26.))
        .px_2p5()
        .rounded_full()
        .border_1()
        .text_sm()
        .whitespace_nowrap()
        .map(|d| match state {
            Chip::Toggle(true) => d
                .bg(theme.selected)
                .border_color(theme.accent)
                .text_color(theme.text),
            Chip::Toggle(false) => d.border_color(theme.border).text_color(theme.text),
            Chip::Download => d.border_color(theme.border).text_color(theme.muted),
            Chip::Busy => d.border_color(theme.border).text_color(theme.muted),
            Chip::Failed => d.border_color(theme.warn).text_color(theme.warn),
        })
        .when(!matches!(state, Chip::Busy), |d| {
            d.tab_index(0)
                .cursor_pointer()
                .hover(|d| d.bg(theme.hover))
                .focus_visible(|d| d.border_color(theme.accent).underline())
        })
        .children(glyph.map(|glyph| {
            icon(glyph, 12.).when(matches!(state, Chip::Toggle(true)), |d| {
                d.text_color(theme.accent)
            })
        }))
        .child(name)
}
