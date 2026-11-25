//! The sidebar: the threads of every project in one list, active ones as cards
//! above the settled ones.
use crate::assets::{self, Icon};
use crate::workspace::{agent_icon, Menu, Workspace, TOOLBAR};
use gpui::{
    anchored, deferred, div, img, list, prelude::*, px, rgb, Anchor, Context, Div, ElementId,
    Focusable, FontWeight, IntoElement, SharedString, Stateful, Window,
};
use kiln_core::{Project, Status, Thread};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hover group of a thread's row, which reveals its actions.
const ROW: &str = "thread-row";

/// One entry of the virtualized list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// The title above the active or the settled threads.
    Header { settled: bool },
    /// `settled` is part of the row so that moving between groups remeasures it.
    Thread { id: u64, settled: bool },
}

impl Workspace {
    /// Rebuild the list after threads were added, removed, or reordered.
    pub(crate) fn sync_rows(&mut self) {
        let mut rows = Vec::new();
        for settled in [false, true] {
            let ids = self.store.thread_ids(settled);
            if !ids.is_empty() {
                rows.push(Row::Header { settled });
                rows.extend(ids.into_iter().map(|id| Row::Thread { id, settled }));
            }
        }
        // Replace only what changed, so the list keeps its scroll position.
        let (old, new) = (&self.rows, &rows);
        let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let tail = old[head..].iter().rev().zip(new[head..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        if head + tail < old.len().max(new.len()) {
            self.thread_list.splice(head..old.len() - tail, new.len() - tail - head);
        }
        self.rows = rows;
    }

    pub(crate) fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        // macOS draws its window buttons over the top of the sidebar.
        let top = if cfg!(target_os = "macos") { TOOLBAR } else { 12. };
        let (width, height) = assets::LOCKUP_SIZE;
        let brand = div().flex().flex_shrink_0().items_center().justify_between()
            // Lines up with the thread cards below.
            .pl(px(18.)).pr_2p5()
            .child(img(theme.lockup).flex_shrink_0().w(px(width)).h(px(height)))
            .when(!self.store.projects.is_empty(), |d| {
                d.child(self.new_thread_picker(window, cx))
            });
        let this = cx.entity();
        let threads = list(self.thread_list.clone(), move |ix, _, cx| {
            this.update(cx, |this, cx| this.thread_row(ix, cx))
        })
        .flex_1()
        .min_h_0();
        let add = div().id("add-project").flex().flex_shrink_0().items_center().gap_2()
            .mx_2p5().mb_2p5().px_2().h(px(30.)).rounded_md().text_sm().text_color(theme.muted)
            .cursor_pointer().hover(|h| h.bg(theme.hover).text_color(theme.text))
            .child(assets::icon(Icon::Plus, 14.)).child("Add Project")
            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx)));
        div().flex().flex_col().flex_shrink_0().gap_2().w(px(260.)).h_full().pt(px(top))
            .bg(theme.sidebar).border_r_1().border_color(theme.border)
            .child(brand)
            .child(threads)
            .child(add)
    }

    /// The button that starts a thread. Its menu lists the projects, where one
    /// can also be removed.
    fn new_thread_picker(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = self.theme;
        let open = self.menu == Some(Menu::NewThread);
        let button = self.icon_button("new-thread", Icon::SquarePen, 26.)
            .when(open, |d| d.bg(theme.selected).text_color(theme.text))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::NewThread, cx)));
        let picker = div().relative().child(button);
        if !open {
            return picker;
        }
        let mut menu = self.menu_panel("new-thread-menu", "New thread in").mt_1()
            .max_h(px(360.)).overflow_y_scroll();
        for (p, project) in self.store.projects.iter().enumerate() {
            let busy = project.threads.iter().any(|t| t.status == Status::Running);
            menu = menu.child(
                div().id(("new-thread-in", p)).group(ROW).flex().flex_shrink_0().items_center()
                    .gap_2().h(px(28.)).pl_2().pr_1().rounded_md().text_sm()
                    .text_color(theme.text).cursor_pointer().hover(|h| h.bg(theme.hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.new_thread(p, cx);
                        window.focus(&this.input.focus_handle(cx), cx);
                    }))
                    .child(self.project_mark(project))
                    .child(div().flex_1().min_w_0().truncate().child(project.name.clone()))
                    .when(!busy, |d| {
                        d.child(
                            self.icon_button(("remove-project", p), Icon::X, 20.)
                                .invisible().group_hover(ROW, |s| s.visible())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.menu = None;
                                    this.remove_project(p, cx);
                                })),
                        )
                    }),
            );
        }
        // Anchored at the button's bottom-right corner, so the menu opens below it.
        picker
            .child(self.backdrop(window, cx))
            .child(div().absolute().bottom_0().right_0().child(
                deferred(anchored().anchor(Anchor::TopRight).snap_to_window().child(menu))
                    .with_priority(2),
            ))
    }

    fn thread_row(&self, ix: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let row = match self.rows.get(ix) {
            Some(&Row::Header { settled }) => {
                let label = if settled { "Settled" } else { "Active" };
                return div().px_2p5().when(ix > 0, |d| d.pt_3()).pb_1()
                    .child(div().px_2().text_xs().font_weight(FontWeight::SEMIBOLD)
                        .text_color(self.theme.muted).child(label))
                    .into_any_element();
            }
            Some(&Row::Thread { id, .. }) => self
                .store
                .locate(id)
                .map(|(p, t)| (&self.store.projects[p], &self.store.projects[p].threads[t])),
            None => None,
        };
        let Some((project, thread)) = row else {
            return div().into_any_element();
        };
        let id = thread.id;
        let theme = self.theme;
        let active = self.selected == Some(id);
        let base = div().id(("thread", id)).group(ROW).rounded(px(8.)).px_2().cursor_pointer()
            .when(active, |d| d.bg(theme.selected))
            .when(!active, |d| d.hover(|h| h.bg(theme.hover)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected = Some(id);
                window.focus(&this.input.focus_handle(cx), cx);
                cx.notify();
            }));
        let age = age(thread.updated_at, now());
        let row = if thread.settled {
            base.flex().items_center().gap_2().h(px(30.))
                .child(self.project_mark(project))
                .child(div().flex_1().min_w_0().truncate().text_sm().text_color(theme.muted)
                    .child(thread.title.clone()))
                .child(self.row_end(thread, age, theme.muted, cx))
        } else {
            let (state, color) = match thread.status {
                Status::Running => (Some("Working"), theme.accent),
                _ if !thread.pending.is_empty() => (Some("Approval"), theme.warn),
                Status::NeedsInput => (Some("Failed"), theme.warn),
                Status::Idle => (None, theme.muted),
            };
            let state = state.map_or(age, SharedString::from);
            let branch = self.branches.get(&project.dir).cloned();
            base.flex().flex_col().gap_1().py_2()
                .child(
                    div().flex().items_center().gap_2()
                        .child(self.project_mark(project))
                        .child(div().flex_1().min_w_0().truncate().text_xs()
                            .text_color(theme.muted).child(project.name.clone()))
                        .child(self.row_end(thread, state, color, cx)),
                )
                .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text).child(thread.title.clone()))
                .child(
                    div().flex().items_center().gap_1p5().h(px(16.)).text_xs()
                        .text_color(theme.muted)
                        .child(div().flex().flex_1().min_w_0().items_center().gap_1()
                            .when_some(branch, |d, branch| {
                                d.child(assets::icon(Icon::GitBranch, 11.))
                                    .child(div().min_w_0().truncate().child(branch))
                            }))
                        .child(agent_icon(thread.agent, 13.)),
                )
        };
        div().px_2p5().pb_0p5().child(row).into_any_element()
    }

    /// The end of a row: `label`, which gives way to the thread's actions while
    /// the pointer is over the row.
    fn row_end(
        &self,
        thread: &Thread,
        label: SharedString,
        color: gpui::Rgba,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = thread.id;
        let settled = thread.settled;
        let can_settle = settled || thread.can_settle();
        let can_delete = thread.status != Status::Running;
        let mut actions = div().absolute().top_0().bottom_0().right_0().flex().items_center()
            .invisible().group_hover(ROW, |s| s.visible());
        if can_settle {
            let icon = if settled { Icon::Undo2 } else { Icon::Check };
            actions = actions.child(
                self.icon_button(("settle", id), icon, 20.)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_settled(id, !settled, cx);
                    })),
            );
        }
        if can_delete {
            actions = actions.child(
                self.icon_button(("delete", id), Icon::Trash2, 20.)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.delete_thread(id, cx);
                    })),
            );
        }
        let has_actions = can_settle || can_delete;
        // Wide enough for both actions, so showing them moves nothing.
        div().relative().flex().flex_shrink_0().items_center().justify_end()
            .min_w(px(40.)).h(px(16.))
            .child(div().text_xs().text_color(color)
                .when(has_actions, |d| d.group_hover(ROW, |s| s.invisible()))
                .child(label))
            .child(actions)
    }

    /// The project's own icon, or else its initials.
    fn project_mark(&self, project: &Project) -> Div {
        let mark = div().flex().flex_shrink_0().items_center().justify_center().size(px(16.));
        match self.icons.get(&project.dir) {
            Some(icon) => mark.child(img(icon.clone()).size_full().rounded(px(4.))),
            None => initials_mark(mark, &project.name),
        }
    }

    /// A borderless square button holding one icon.
    fn icon_button(&self, id: impl Into<ElementId>, icon: Icon, size: f32) -> Stateful<Div> {
        let theme = self.theme;
        div().id(id).flex().flex_shrink_0().items_center().justify_center().size(px(size))
            .rounded_md().text_color(theme.muted).cursor_pointer()
            .hover(|h| h.bg(theme.hover).text_color(theme.text))
            .child(assets::icon(icon, 14.))
    }
}

/// A project's initials on a color that stays the same for its name.
fn initials_mark(mark: Div, name: &str) -> Div {
    const COLORS: [u32; 8] = [
        0xc2255c, 0x9c36b5, 0x6741d9, 0x3b5bdb, 0x1971c2, 0x0c8599, 0x2f9e44, 0xe8590c,
    ];
    let hash = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b.into()));
    mark.rounded(px(4.))
        .bg(rgb(COLORS[hash as usize % COLORS.len()])).text_color(rgb(0xffffff))
        .text_size(px(8.)).font_weight(FontWeight::BOLD)
        .child(initials(name))
}

/// The first letters of the first two words, or of a single word's start.
fn initials(name: &str) -> String {
    let mut words = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty());
    let Some(first) = words.next() else {
        return String::new();
    };
    let letters: String = match words.next() {
        Some(second) => first.chars().take(1).chain(second.chars().take(1)).collect(),
        None => first.chars().take(2).collect(),
    };
    letters.to_uppercase()
}

/// The current time in Unix seconds.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// How long ago `then` was, in its largest whole unit; empty when unknown.
fn age(then: u64, now: u64) -> SharedString {
    if then == 0 {
        return "".into();
    }
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60).into(),
        3600..86_400 => format!("{}h", secs / 3600).into(),
        _ => format!("{}d", secs / 86_400).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_uses_the_largest_whole_unit() {
        let now = 1_000_000;
        for (ago, shown) in [
            (0, "now"),
            (59, "now"),
            (60, "1m"),
            (3599, "59m"),
            (3600, "1h"),
            (86_399, "23h"),
            (172_800, "2d"),
        ] {
            assert_eq!(age(now - ago, now), shown, "{ago}");
        }
        assert_eq!(age(0, now), "");
        // A clock set back must not underflow.
        assert_eq!(age(now + 5, now), "now");
    }

    #[test]
    fn initials_come_from_the_words_of_a_name() {
        assert_eq!(initials("kiln"), "KI");
        assert_eq!(initials("agent-context"), "AC");
        assert_eq!(initials("my_web.app"), "MW");
        assert_eq!(initials("x"), "X");
        assert_eq!(initials("--"), "");
    }
}
