use super::{centered, Workspace, TRANSCRIPT_PADDING};
use crate::theme::MONO;
use crate::transcript::{self, markdown, selection, shell, Entry};
use crate::{assets, media, visualization};
use chrono::{Local, TimeZone};
use gpui::{
    div, prelude::*, px, relative, Animation, AnimationExt, Context, FontWeight, IntoElement,
    Window,
};
use kiln_core::checkout::{self, Checkout};
use kiln_core::{title, AttachmentContent, Role, Status};
use std::time::Duration;

impl Workspace {
    pub(super) fn sync_transcript(&mut self) {
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

    pub(super) fn entry(&mut self, id: u64, row: usize, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
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
        let mut document = selection::Document::new(self.output_selection.clone(), ix);
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
                                media::attachment_button(format!("sent-{ix}-{i}"), attachment, theme)
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
        document: &mut selection::Document,
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
        let mut document = selection::Document::new(self.output_selection.clone(), key);
        let content = div().flex().flex_col().w_full()
            .when_some(streamed, |d, blocks| {
                d.child(centered(div().min_w_0().text_sm().line_height(relative(1.55))
                    .child(markdown::render(&blocks, theme, root.as_deref(), &self.scratch_dir(id), &mut document, window, cx)))
                    .px_6().py_1p5())
            })
            .child(status);
        document.render(content, theme.accent.opacity(0.3).into())
    }
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

/// Agent text with Kiln's control lines removed.
pub(super) fn shown_reply(text: &str, place: &Checkout, projectless: bool) -> String {
    let text = title::take_title(text).0;
    if projectless {
        return checkout::take_project_request(&text).0;
    }
    match place {
        Checkout::Project => checkout::take_move_request(&text).0,
        Checkout::Worktree { .. } => text,
    }
}
