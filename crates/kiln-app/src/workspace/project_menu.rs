//! The toolbar's project button and its menu, where the icon can be changed.

use crate::assets::{self, Icon};
use super::project_icon;
use crate::workspace::{Menu, Workspace};
use gpui::{
    anchored, deferred, div, prelude::*, px, Anchor, Context, Div, PathPromptOptions, Window,
};
use kiln_core::Project;
use std::path::{Path, PathBuf};

impl Workspace {
    /// The project's name under the thread title; a click opens its menu.
    pub(crate) fn project_picker(
        &self,
        project: &Project,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = self.theme;
        let open = self.menu == Some(Menu::Project);
        let button = div().id("project-picker").flex().min_w_0().items_center().gap_1p5()
            .px_1().rounded_md().text_xs().text_color(theme.muted).cursor_pointer()
            .when(open, |d| d.bg(theme.selected).text_color(theme.text))
            .when(!open, |d| d.hover(|h| h.bg(theme.hover).text_color(theme.text)))
            .child(self.project_mark(project))
            .child(div().min_w_0().truncate().child(project.name.clone()))
            .child(assets::icon(Icon::ChevronDown, 12.))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Project, cx)));
        // The row keeps the button as narrow as its content.
        let picker = div().relative().flex().min_w_0().child(button);
        // Pulled left so the mark lines up with the title above.
        let row = div().flex().min_w_0().ml(px(-4.));
        if !open {
            return row.child(picker);
        }
        let mut menu = self.menu_panel("project-menu", "Project").mt_1();
        let dir = project.dir.clone();
        menu = menu.child(
            self.menu_item("change-icon", None, "Change icon…", None, false)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.choose_icon(dir.clone(), cx);
                })),
        );
        if project.icon.is_some() {
            let dir = project.dir.clone();
            menu = menu.child(
                self.menu_item("detected-icon", None, "Use detected icon", None, false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.set_icon(&dir, None, cx);
                    })),
            );
        }
        if self.icon_rejected {
            menu = menu.child(
                div().px_2().pt_1p5().pb_1().text_xs().text_color(theme.warn).child(
                    "That file cannot be used. Pick a PNG, SVG, ICO, JPEG, GIF, or WebP \
                     image of up to 1 MB.",
                ),
            );
        }
        // Anchored at the button's bottom-left corner, so the menu opens below it.
        row.child(
            picker
                .child(self.backdrop(window, cx))
                .child(div().absolute().bottom_0().left_0().child(
                    deferred(anchored().anchor(Anchor::TopLeft).snap_to_window().child(menu))
                        .with_priority(2),
                )),
        )
    }

    /// Ask for an image and make it the icon of the project in `dir`.
    fn choose_icon(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose icon".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(file) = paths.into_iter().next() else { return };
            let (file, usable) = cx
                .background_executor()
                .spawn(async move {
                    let usable = project_icon::from_file(&file).is_some();
                    (file, usable)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if usable {
                    this.set_icon(&dir, Some(file), cx);
                } else if this.shows_project(&dir) {
                    this.menu = Some(Menu::Project);
                    this.icon_rejected = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Whether the shown thread belongs to the project in `dir`.
    fn shows_project(&self, dir: &Path) -> bool {
        let shown = self.selected.and_then(|id| self.store.project_dir_of(id));
        shown.is_some_and(|shown| shown == dir)
    }

    /// Save `icon` as the choice for the project in `dir`; `None` goes back to
    /// the icon found in the project.
    fn set_icon(&mut self, dir: &Path, icon: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(project) = self.store.projects.iter_mut().find(|p| p.dir == dir) else {
            return;
        };
        project.icon = icon;
        self.save();
        self.refresh(cx);
    }
}
