//! The toolbar's branch button and its menu, where a thread moves between the
//! project checkout and a worktree of its own.

use crate::assets::{self, Icon};
use crate::workspace::{Menu, Workspace};
use gpui::{anchored, deferred, div, prelude::*, px, Anchor, Context, Div, Window};
use kiln_core::{Checkout, Fetch, Project, Status, Thread};

impl Workspace {
    /// The branch the thread runs on; `None` for a project outside Git.
    pub(crate) fn checkout_picker(
        &self,
        project: &Project,
        thread: &Thread,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let theme = self.theme;
        let worktree = matches!(thread.checkout, Checkout::Worktree { .. });
        let branch = self.branches.get(thread.checkout.dir(&project.dir)).cloned();
        if branch.is_none() && !worktree {
            return None;
        }
        let missing = branch.is_none();
        let available = thread.status != Status::Running;
        let open = available && self.menu == Some(Menu::Checkout);
        let icon = if worktree { Icon::GitFork } else { Icon::GitBranch };
        let button = div().id("checkout-picker").flex().min_w_0().items_center().gap_1()
            .px_1().rounded_md().text_xs()
            .text_color(if missing { theme.warn } else { theme.muted })
            .when(open, |d| d.bg(theme.selected).text_color(theme.text))
            .when(available, |d| d.cursor_pointer()
                .hover(|h| h.bg(theme.hover).text_color(theme.text))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Checkout, cx))))
            .child(assets::icon(icon, 11.))
            .child(div().min_w_0().truncate()
                .child(branch.unwrap_or_else(|| "Worktree missing".into())))
            .when(available, |d| d.child(assets::icon(Icon::ChevronDown, 12.)));
        let picker = div().relative().flex().min_w_0().child(button);
        if !open {
            return Some(picker);
        }
        let id = thread.id;
        let mut menu = self.menu_panel("checkout-menu", "Run this thread in").w(px(285.)).mt_1()
            .child(
                self.menu_item("checkout-project", None, "Project checkout", None, !worktree)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.return_to_project(id, cx);
                    })),
            );
        // Fetching reaches the network, so it happens only as the settings say.
        let new_worktree = |key: &'static str, label: &'static str, hint, fetch: bool| {
            self.menu_item(key, None, label, Some(hint), false)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.move_to_worktree(id, None, fetch, cx);
                }))
        };
        menu = match self.store.settings.fetch {
            Fetch::Ask => menu
                .child(new_worktree("checkout-new", "New worktree", "from local main", false))
                .child(new_worktree("checkout-fetch", "Fetch, then new worktree", "from origin", true)),
            Fetch::Always => menu.child(new_worktree("checkout-new", "New worktree", "fetches first", true)),
            Fetch::Never => menu.child(new_worktree("checkout-new", "New worktree", "from main", false)),
        };
        if worktree {
            menu = menu.child(
                self.menu_item("checkout-remove", None, "Remove worktree", Some("keeps the branch"), false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.remove_worktree(id, cx);
                    })),
            );
        }
        let detail = match &thread.checkout {
            Checkout::Project => project.dir.display().to_string(),
            Checkout::Worktree { path, base, .. } => format!("{}\nStarted from {base}.", path.display()),
        };
        menu = menu.child(
            div().px_2().pt_1p5().pb_1().text_xs().text_color(theme.muted)
                .child(format!("{detail}\nMoving starts a new session with a recap.")),
        );
        // Anchored at the button's bottom-left corner, so the menu opens below it.
        Some(
            picker
                .child(self.backdrop(window, cx))
                .child(div().absolute().bottom_0().left_0().child(
                    deferred(anchored().anchor(Anchor::TopLeft).snap_to_window().child(menu))
                        .with_priority(2),
                )),
        )
    }
}
