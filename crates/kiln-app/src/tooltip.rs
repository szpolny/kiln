//! A small label that appears when the pointer rests on an element.
use crate::theme::Theme;
use gpui::{div, prelude::*, AnyView, App, Context, IntoElement, Render, SharedString, Window};

struct Tip {
    text: SharedString,
    theme: Theme,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        div().px_2().py_1().rounded_md().bg(theme.card).border_1().border_color(theme.border)
            .shadow_md().text_xs().text_color(theme.text).child(self.text.clone())
    }
}

/// Builds the tooltip view for `.tooltip(...)`.
pub fn tip(text: impl Into<SharedString>, theme: Theme) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip { text: text.clone(), theme }).into()
}
