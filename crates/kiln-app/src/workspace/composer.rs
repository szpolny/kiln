use super::sidebar::now;
use super::{agent_icon, centered, permission_label, FileIndex, Menu, Workspace, MENU_ROW_HEIGHT, REFRESH};
use crate::theme::Theme;
use crate::tooltip::tip;
use crate::{assets, file_reference, media};
use gpui::{
    anchored, canvas, deferred, div, ease_out_quint, fill, point, prelude::*, px, rgb, size, Anchor,
    Animation, AnimationExt, Bounds, ClickEvent, Context, DispatchPhase, ExternalPaths, Focusable,
    FontWeight, HitboxBehavior, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels,
    SharedString, Window,
};
use kiln_core::policy::{Decision, PermissionMode};
use kiln_core::{Agent, Attachment, Effort, FollowUp, Prompt, Status, Thread};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SCROLLBAR_GUTTER: f32 = 10.;

impl Workspace {
    pub(super) fn suggest_files(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).reference_query();
        if self.dismissed_file_query == query && self.dismissed_file_query.is_some() {
            return;
        }
        self.dismissed_file_query = None;
        self.file_suggestions.clear();
        cx.notify();
        let Some(query) = query else { return };
        let Some(root) = self.selected.and_then(|id| self.checkout_dir(id)) else { return };
        if let Some(index) = self.file_index.as_ref().filter(|index| index.root == root) {
            match &index.files {
                Some(files) => {
                    self.file_suggestions = file_reference::matches(files, &query);
                    if index.loaded_at.elapsed() < REFRESH {
                        return;
                    }
                }
                None => return,
            }
        }

        self.file_index = Some(FileIndex { root: root.clone(), loaded_at: Instant::now(), files: None });
        let dir = root.clone();
        let load = cx.background_executor().spawn(async move { file_reference::index(&dir) });
        self.file_index_load = cx.spawn(async move |this, cx| {
            let files = load.await;
            let _ = this.update(cx, |this, cx| {
                let Some(index) = this.file_index.as_mut().filter(|index| index.root == root) else {
                    return;
                };
                index.files = Some(files);
                index.loaded_at = Instant::now();
                this.suggest_files(cx);
            });
        });
    }

    pub(super) fn suggestions_visible(&self, window: &Window, cx: &Context<Self>) -> bool {
        !self.file_suggestions.is_empty() && self.menu.is_none()
            && self.input.focus_handle(cx).is_focused(window)
            && self.selected.and_then(|id| self.checkout_dir(id)).as_deref()
                == self.file_index.as_ref().map(|index| index.root.as_path())
    }

    pub(super) fn choose_reference(&mut self, id: u64, query: file_reference::Query, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected != Some(id) || self.input.read(cx).reference_query() != Some(query.clone()) {
            return;
        }
        let exists = self.checkout_dir(id).and_then(|root| media::scoped_path(&root, Path::new(path)))
            .is_some_and(|path| path.is_file());
        if exists {
            self.input.update(cx, |input, cx| input.replace_reference(query, path, cx));
            window.focus(&self.input.focus_handle(cx), cx);
        } else {
            self.file_suggestions.retain(|candidate| candidate != path);
            cx.notify();
        }
    }

    fn file_menu(&self, id: u64, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.suggestions_visible(window, cx) {
            return None;
        }
        let query = self.input.read(cx).reference_query()?;
        let theme = self.theme;
        let width = (window.viewport_size().width - px(328.)).clamp(px(160.), px(440.));
        let menu = self.menu_surface("file-suggestions").w(width).children(
            self.file_suggestions.iter().enumerate().map(|(i, path)| {
                let path = path.clone();
                let query = query.clone();
                div().id(("file-suggestion", i)).flex().items_center().gap_2().h(px(MENU_ROW_HEIGHT))
                    .px_2().rounded_md().text_sm().cursor_pointer()
                    .hover(|d| d.bg(theme.hover))
                    .child({
                        let (icon, tint) = file_reference::file_icon(Path::new(&path));
                        assets::icon(icon, 14.).text_color(tint.map_or(theme.muted, rgb))
                    })
                    .child(div().min_w_0().truncate().child(path.clone()))
                    .when(i == 0, |row| {
                        row.bg(theme.hover).child(
                            div().ml_auto().flex_shrink_0().text_xs().text_color(theme.muted).child("Tab"),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.choose_reference(id, query.clone(), &path, window, cx)))
            }),
        );
        Some(div().absolute().top_0().left_0().child(
            deferred(anchored().anchor(Anchor::BottomLeft).snap_to_window().child(menu.mb_2()))
                .with_priority(2),
        ))
    }

    fn pick_files(&mut self, id: u64, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let _ = this.update(cx, |this, cx| this.add_files(id, paths, cx));
        }).detach();
    }

    fn add_files(&mut self, id: u64, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.load_attachments(id, move || {
            paths.into_iter().take(media::COUNT_LIMIT + 1)
                .map(|path| media::attach_file(&path).map_err(|error| {
                    format!("{} · {error}", path.file_name().unwrap_or(path.as_os_str()).to_string_lossy())
                })).collect()
        }, cx);
    }

    /// A veil over the window while files from another app hover over it.
    /// Dropping them anywhere attaches them to the open thread's draft.
    pub(super) fn render_file_drop(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        // Kiln starts no drags of its own, so any drag is a file drag from outside.
        if !cx.has_active_drag() {
            return None;
        }
        let id = self.selected.filter(|&id| self.store.thread(id).is_some())?;
        let theme = self.theme;
        let icon = assets::icon(assets::Icon::FileUp, 28.).relative().text_color(theme.accent).with_animation(
            "file-drop-icon",
            Animation::new(Duration::from_millis(1400)).repeat(),
            |icon, phase| icon.top(px(-4. * (phase * std::f32::consts::TAU).sin())),
        );
        let card = div().flex().flex_col().items_center().gap_2().px_12().py_8()
            .rounded_xl().border_2().border_dashed().bg(theme.card)
            .child(icon)
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Drop files to attach"))
            .child(div().text_xs().text_color(theme.muted)
                .child(format!("Text files, images and PDFs, up to {} per message", media::COUNT_LIMIT)))
            .with_animation(
                "file-drop-card",
                Animation::new(Duration::from_millis(1400)).repeat(),
                move |card, phase| {
                    let glow = (1. - (phase * std::f32::consts::TAU).cos()) * 0.5;
                    card.border_color(theme.accent.opacity(0.4 + 0.6 * glow))
                },
            );
        Some(
            div().absolute().inset_0().flex().items_center().justify_center()
                .bg(theme.window.opacity(0.8))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    this.add_files(id, paths.paths().to_vec(), cx);
                }))
                .child(card)
                .with_animation(
                    "file-drop-fade",
                    Animation::new(Duration::from_millis(140)).with_easing(ease_out_quint()),
                    |veil, delta| veil.opacity(delta),
                ),
        )
    }

    pub(super) fn set_attachment_error(&mut self, id: u64, error: Option<String>, cx: &mut Context<Self>) {
        self.attachment_errors.remove(&id);
        if let Some(error) = error {
            self.attachment_errors.insert(id, error);
        }
        cx.notify();
    }

    pub(super) fn load_attachments(
        &mut self,
        id: u64,
        load: impl FnOnce() -> Vec<Result<Attachment, String>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.store.thread(id).is_none() {
            return;
        }
        if !self.attachment_jobs.insert(id) {
            self.set_attachment_error(id, Some("Finish adding the current files before adding more.".into()), cx);
            return;
        }
        self.set_attachment_error(id, None, cx);
        let task = cx.background_executor().spawn(async move { load() });
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            let _ = this.update(cx, |this, cx| {
                this.attachment_jobs.remove(&id);
                let Some(thread) = this.store.thread_mut(id) else { return };
                let mut errors = vec![];
                let draft = &mut thread.draft_attachments;
                let mut size = draft.iter().map(Attachment::stored_bytes).sum::<usize>();
                for result in loaded {
                    match result {
                        Ok(file) => {
                            size += file.stored_bytes();
                            if draft.len() >= media::COUNT_LIMIT || size > media::TOTAL_LIMIT {
                                errors.push("A draft can hold up to 8 attachments and 20 MiB of stored data.".into());
                                break;
                            }
                            draft.push(file);
                        }
                        Err(error) => errors.push(error),
                    }
                }
                this.save();
                this.set_attachment_error(id, (!errors.is_empty()).then(|| errors.join("\n")), cx);
            });
        }).detach();
    }

    pub(super) fn composer_prompt(&self, thread: &Thread, cx: &Context<Self>) -> Prompt {
        Prompt {
            body: self.input.read(cx).text().trim().to_owned(),
            attachments: thread.draft_attachments.clone(),
        }
    }

    pub(super) fn clear_composer(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(thread) = self.store.thread_mut(id) {
            thread.draft.clear();
            thread.draft_attachments.clear();
        }
        self.attachment_errors.remove(&id);
        self.input.update(cx, |input, cx| input.clear(cx));
    }

    fn set_model(&mut self, id: u64, agent: Agent, model: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.store.choose_model(id, agent, model, now()) {
            self.select(Some(target), cx);
        }
        cx.notify();
    }

    fn set_effort(&mut self, id: u64, effort: Effort, cx: &mut Context<Self>) {
        if self.store.select_effort(id, effort) {
            self.save();
            cx.notify();
        }
    }

    /// Compact picker in the composer footer; the choice applies from the next turn.
    fn model_picker(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        const PREVIEW_LEN: usize = 4;
        const PROVIDER_CHIP_HEIGHT: f32 = 26.;

        let theme = self.theme;
        let (agent, expanded, choosing_provider) = match self.menu {
            Some(Menu::Model { agent, expanded, choosing_provider }) => (agent, expanded, choosing_provider),
            _ => (thread.agent, false, false),
        };
        let state = Menu::Model { agent, expanded, choosing_provider };
        let button = self.footer_button("model-picker", state, cx)
            .child(agent_icon(thread.agent, 13., theme.text))
            .child(div().font_weight(FontWeight::MEDIUM)
                .child(thread.agent.model_name(thread.model()).to_string()));
        if self.menu != Some(state) {
            return self.footer_picker(button, None, window, cx);
        }

        let provider_chip = div().id("choose-model-provider").flex().items_center().gap_1p5()
            .h(px(PROVIDER_CHIP_HEIGHT)).px_2().rounded_md().bg(theme.fill).border_1().border_color(theme.border)
            .text_xs().font_weight(FontWeight::SEMIBOLD).cursor_pointer()
            .when(choosing_provider, |d| d.bg(theme.selected))
            .hover(|h| h.bg(theme.selected))
            .child(agent_icon(agent, 13., theme.text))
            .child(agent.name())
            .child(assets::icon(assets::Icon::ChevronDown, 12.).text_color(theme.muted))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu = Some(Menu::Model { agent, expanded, choosing_provider: !choosing_provider });
                cx.notify();
            }));
        let header = div().flex().items_center().justify_between().px_1().pb_1()
            .child(div().pl_1().text_xs().text_color(theme.muted).child("Models"))
            .child(provider_chip);

        let models = agent.models();
        let list_height = (window.viewport_size().height - px(164.)).clamp(px(56.), px(196.));
        let selected = (agent == thread.agent).then(|| thread.model());
        let preview = &models[..models.len().min(PREVIEW_LEN)];
        let selected_below_preview = models[preview.len()..].iter().find(|m| Some(m.id) == selected);
        let shown = if expanded { models } else { preview };
        let overflows = px(MENU_ROW_HEIGHT * shown.len() as f32) > list_height;
        let id = thread.id;
        let results = div().id(SharedString::from(format!("model-options-{}-{expanded}", agent.label())))
            .flex().flex_col().max_h(list_height).overflow_y_scroll()
            .track_scroll(&self.model_scroll)
            .when(overflows, |d| d.pr(px(SCROLLBAR_GUTTER)))
            .children(shown.iter().enumerate().map(|(index, model)| {
                let model = if !expanded && index == PREVIEW_LEN - 1 {
                    selected_below_preview.unwrap_or(model)
                } else {
                    model
                };
                let model_id = model.id;
                let active = Some(model_id) == selected;
                self.menu_item(model_id, None, model.name, None, active)
                    .flex_shrink_0()
                    .when(active, |d| d.bg(theme.selected).font_weight(FontWeight::MEDIUM))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.set_model(id, agent, model_id, cx);
                        window.focus(&this.input.focus_handle(cx), cx);
                    }))
            }));

        let menu = self.menu_surface("model-menu").relative().w(px(288.)).child(header)
            .when(agent != thread.agent && !thread.can_change_agent(), |d| {
                d.child(div().px_2().pb_1().text_xs().text_color(theme.muted)
                    .child("Selecting a model starts a new thread"))
            })
            .child(div().relative().child(results)
                .when(overflows, |d| d.child(self.model_scrollbar())))
            .when(models.len() > PREVIEW_LEN, |d| {
                let action_color = theme.accent.blend(theme.text.alpha(0.3));
                let (label, icon) = if expanded {
                    ("Show fewer models".to_string(), assets::Icon::Minus)
                } else {
                    (format!("Show all {} models", models.len()), assets::Icon::Plus)
                };
                d.child(div().flex().mt_1().child(div().id("toggle-model-list").flex().items_center().gap_1()
                        .h(px(28.)).px_2().rounded_md().text_xs().text_color(action_color)
                        .cursor_pointer().hover(|h| h.bg(theme.hover))
                        .child(assets::icon(icon, 12.))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reset_model_scroll();
                            this.menu = Some(Menu::Model { agent, expanded: !expanded, choosing_provider: false });
                            cx.notify();
                        }))))
            })
            .when(choosing_provider, |d| {
                // Paint above the model rows without changing the picker's size.
                d.child(self.menu_surface("model-providers").absolute()
                    .top(px(PROVIDER_CHIP_HEIGHT + 8.)).right_2().w(px(152.)).bg(theme.fill)
                    .children(Agent::ALL.map(|provider| {
                        let unavailable = thread.status == Status::Running && provider != thread.agent;
                        self.menu_item(provider.label(), Some(provider), provider.name(), None, provider == agent)
                            .when(provider == agent, |d| d.bg(theme.selected))
                            .when(unavailable, |d| d.opacity(0.4))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if unavailable {
                                    return;
                                }
                                if provider != agent {
                                    this.reset_model_scroll();
                                }
                                this.menu = Some(Menu::Model {
                                    agent: provider,
                                    expanded: provider == agent && expanded,
                                    choosing_provider: false,
                                });
                                cx.notify();
                            }))
                    })))
            });
        self.footer_picker(button, Some(menu), window, cx)
    }

    pub(super) fn reset_model_scroll(&self) {
        self.model_scroll.set_offset(point(px(0.), px(0.)));
        self.model_scroll_grab.set(None);
    }

    fn model_scrollbar(&self) -> impl IntoElement {
        let scroll = self.model_scroll.clone();
        let grab = self.model_scroll_grab.clone();
        let theme = self.theme;
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                let max_offset = scroll.max_offset().y;
                if max_offset <= px(0.) {
                    return;
                }
                let track_height = bounds.size.height - px(4.);
                let thumb_height = (track_height * (bounds.size.height / (bounds.size.height + max_offset)))
                    .clamp(px(16.), track_height);
                let travel = track_height - thumb_height;
                let track_top = bounds.top() + px(2.);
                let thumb_top = track_top + travel * (-scroll.offset().y / max_offset);
                let track_left = bounds.left() + px(4.);
                let paint_bar = |top, height, color, window: &mut Window| {
                    let mut bar = fill(Bounds::new(point(track_left, top), size(px(3.), height)), color);
                    bar.corner_radii = px(1.5).into();
                    window.paint_quad(bar);
                };
                paint_bar(track_top, track_height, theme.border.opacity(0.65), window);
                paint_bar(thumb_top, thumb_height, theme.muted.alpha(0.56), window);

                let scroll_to = move |y: Pixels, anchor: Pixels, window: &mut Window| {
                    let progress = ((y - track_top - anchor) / travel).clamp(0., 1.);
                    scroll.set_offset(point(px(0.), -max_offset * progress));
                    window.refresh();
                };
                let drag_scroll = scroll_to.clone();
                let start_grab = grab.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || event.button != MouseButton::Left
                        || !hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let within_thumb = event.position.y - thumb_top;
                    let anchor = if (px(0.)..=thumb_height).contains(&within_thumb) {
                        within_thumb
                    } else {
                        thumb_height / 2.
                    };
                    start_grab.set(Some(anchor));
                    scroll_to(event.position.y, anchor, window);
                    cx.stop_propagation();
                });
                let moving_grab = grab.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && event.dragging() {
                        if let Some(anchor) = moving_grab.get() {
                            drag_scroll(event.position.y, anchor, window);
                            cx.stop_propagation();
                        }
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
                    if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                        grab.set(None);
                    }
                });
            },
        )
        .absolute().top_0().right_0().bottom_0().w(px(SCROLLBAR_GUTTER))
    }

    /// Reasoning level picker beside the model; applies from the next turn.
    fn effort_picker(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let button = self.footer_button("effort-picker", Menu::Effort, cx)
            .child(assets::icon(assets::Icon::Brain, 13.).text_color(self.theme.muted))
            .child(div().font_weight(FontWeight::MEDIUM).child(thread.effort().name()));
        if self.menu != Some(Menu::Effort) {
            return self.footer_picker(button, None, window, cx);
        }
        let id = thread.id;
        let mut menu = self.menu_panel("effort-menu", "Reasoning");
        for &effort in thread.agent.efforts(thread.model()) {
            let key = SharedString::from(format!("effort-{}", effort.id()));
            menu = menu.child(
                self.menu_item(key, None, effort.name(), None, thread.effort() == effort)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.set_effort(id, effort, cx);
                    })),
            );
        }
        self.footer_picker(button, Some(menu), window, cx)
    }

    /// A dropdown button in the composer footer. The caller adds its label.
    fn footer_button(
        &self,
        id: &'static str,
        menu: Menu,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        let open = self.menu == Some(menu);
        div().id(id).flex().flex_shrink_0().items_center().gap_1p5().h(px(26.)).px_2()
            .rounded_md().text_xs().cursor_pointer()
            .when(open, |d| d.bg(theme.selected).text_color(theme.text))
            .when(!open, |d| d.text_color(theme.muted).hover(|h| h.bg(theme.hover).text_color(theme.text)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_menu(menu, cx)))
    }

    /// Finish a footer dropdown, showing `menu` above its button when open.
    fn footer_picker(
        &self,
        button: gpui::Stateful<gpui::Div>,
        menu: Option<gpui::Stateful<gpui::Div>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let button = button.child(div().text_color(self.theme.muted).child("▾"));
        let picker = div().relative().child(button);
        let Some(menu) = menu else {
            return picker;
        };
        // Anchored at the button's top-left corner, so the menu opens above it.
        picker
            .child(self.backdrop(window, cx))
            .child(div().absolute().top_0().left_0().child(
                deferred(anchored().anchor(Anchor::BottomLeft).snap_to_window().child(menu.mb_2()))
                    .with_priority(2),
            ))
    }

    fn set_permission_mode(&mut self, id: u64, mode: PermissionMode) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        if thread.status == Status::Running
            || thread.permission_mode == mode
            || !thread.agent.permission_modes().contains(&mode)
        {
            return;
        }
        thread.permission_mode = mode;
        self.save();
    }

    fn set_web_access(&mut self, id: u64, access: Decision) {
        let Some(thread) = self.store.thread_mut(id) else { return };
        if thread.status == Status::Running || thread.permission_mode != PermissionMode::Manual {
            return;
        }
        thread.policy.network = access;
        self.save();
    }

    /// Toolbar picker for the next turn's provider mode and Manual web rules.
    pub(super) fn permission_picker(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let available = thread.status != Status::Running;
        let open = available && self.menu == Some(Menu::Permissions);
        let mode = thread.permission_mode;
        let color = match mode {
            PermissionMode::Manual => theme.ok,
            PermissionMode::Auto => theme.accent,
            PermissionMode::FullAccess => theme.warn,
        };
        let button = div().id("permission-picker").flex().items_center().gap_1p5().px_2p5().py_1()
            .rounded_full().text_xs().text_color(theme.muted).bg(theme.fill)
            .when(open, |d| d.bg(theme.selected).text_color(theme.text))
            .when(available, |d| d.cursor_pointer()
                .hover(|h| h.bg(theme.hover).text_color(theme.text))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Permissions, cx))))
            .child(div().size(px(6.)).rounded_full().bg(color))
            .child(permission_label(mode))
            .when(available, |d| d.child("▾"));
        let picker = div().relative().child(button);
        if !open {
            return picker;
        }
        let id = thread.id;
        let mut menu = self.menu_panel("permission-menu", "Permissions for this thread").w(px(285.)).mt_2();
        for &choice in thread.agent.permission_modes() {
            let (key, detail) = match choice {
                PermissionMode::Manual => ("permissions-manual", "Review approval requests"),
                PermissionMode::Auto => ("permissions-auto", "Provider reviews actions"),
                PermissionMode::FullAccess => ("permissions-full", "Run tools without asking"),
            };
            menu = menu.child(
                self.menu_item(key, None, permission_label(choice), Some(detail), mode == choice)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.set_permission_mode(id, choice);
                        cx.notify();
                    })),
            );
        }
        if mode == PermissionMode::Manual {
            let codex = thread.agent == Agent::Codex;
            menu = menu.child(
                div().mt_1().border_t_1().border_color(theme.border)
                    .px_2().pt_2().pb_1().text_xs().font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted).child(if codex { "Web search" } else { "Web search and fetch" }),
            );
            for (key, access, label) in [
                ("web-ask", Decision::Ask, "Ask first"),
                ("web-allow", Decision::Allow, "Allow"),
                ("web-block", Decision::Deny, "Block"),
            ] {
                if codex && access == Decision::Ask {
                    continue;
                }
                let selected = thread.policy.network == access
                    || (codex && access == Decision::Deny && thread.policy.network == Decision::Ask);
                menu = menu.child(
                    self.menu_item(key, None, label, None, selected)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            this.set_web_access(id, access);
                            cx.notify();
                        })),
                );
            }
        }
        menu = menu.child(
            div().px_2().pt_1p5().pb_1().text_xs().text_color(theme.muted)
                .child("Changes apply to the next turn."),
        );
        // Anchored at the button's bottom-right corner, so the menu opens below it.
        picker
            .child(self.backdrop(window, cx))
            .child(div().absolute().bottom_0().right_0().child(
                deferred(anchored().anchor(Anchor::TopRight).snap_to_window().child(menu))
                    .with_priority(2),
            ))
    }

    /// The round button at the composer's corner. The caller adds its mark.
    fn round_button(&self, id: &'static str, enabled: bool) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div().id(id).flex().flex_shrink_0().items_center().justify_center()
            .size(px(28.)).rounded_full().text_sm().font_weight(FontWeight::BOLD)
            .when(enabled, |d| {
                d.bg(theme.accent).text_color(theme.on_accent).cursor_pointer()
                    .hover(|h| h.opacity(0.85))
            })
            .when(!enabled, |d| d.bg(theme.selected).text_color(theme.muted))
    }

    fn send_button(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let ready = self.selected.and_then(|id| self.store.thread(id))
            .is_some_and(|thread| self.composer_ready(thread, cx));
        self.round_button("send", ready)
            .child("↑")
            .tooltip(tip("Send", self.theme))
            .on_click(cx.listener(|this, _, _, cx| this.submit(false, cx)))
    }

    /// Takes the send button's place while a turn runs.
    fn stop_button(&self, id: u64, enabled: bool, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let mark = if enabled { self.theme.on_accent } else { self.theme.muted };
        self.round_button("stop", enabled)
            .child(div().size(px(10.)).rounded(px(2.)).bg(mark))
            .tooltip(tip("Stop", self.theme))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.stop(id, cx);
                // The click took focus; hand it back for the next message.
                window.focus(&this.input.focus_handle(cx), cx);
            }))
    }

    /// The follow-ups waiting for the thread's turn to end.
    pub(super) fn queue_panel(&self, thread: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let id = thread.id;
        let running = thread.status == Status::Running;
        let mut col = div().flex().flex_col().flex_shrink_0().gap_1().px_6();
        if thread.queue.is_empty() {
            return col;
        }
        let heading = if running {
            "Queued — sent when this turn ends"
        } else {
            "Queued — not sent because the last turn did not finish"
        };
        let mut panel = div().flex().flex_col().gap_1().w_full().p_2().rounded(px(12.))
            .bg(theme.card).border_1().border_color(theme.border)
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted)
                .child(heading));
        for (i, prompt) in thread.queue.iter().enumerate() {
            let button = |id: (&'static str, usize), mark: assets::Icon, label: &'static str| {
                div().id(id).flex().flex_shrink_0().items_center().justify_center().size(px(22.))
                    .rounded_md().text_color(theme.muted).cursor_pointer()
                    .hover(|h| h.bg(theme.hover).text_color(theme.text))
                    .tooltip(tip(label, theme))
                    .child(assets::icon(mark, 14.))
            };
            panel = panel.child(
                div().flex().items_center().gap_1().child(
                    div().flex_1().min_w_0().truncate().text_sm().text_color(theme.text)
                        .child(prompt.body.lines().next().filter(|line| !line.is_empty()).unwrap_or("Attachments").to_string()),
                )
                .when(!running && i == 0, |d| {
                    d.child(button(("queued-send", i), assets::Icon::ArrowUp, "Send now")
                        .on_click(cx.listener(move |this, _, _, cx| this.resume_queue(id, cx))))
                })
                .child(button(("queued-edit", i), assets::Icon::Pencil, "Edit")
                    .on_click(cx.listener(move |this, _, _, cx| this.edit_queued(id, i, cx))))
                .child(button(("queued-remove", i), assets::Icon::X, "Remove")
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_queued(id, i, cx)))),
            );
        }
        col = col.child(centered(panel).flex_shrink_0());
        col
    }

    fn resume_button(&self, id: u64, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        self.round_button("resume", true)
            .child(assets::icon(assets::Icon::Play, 12.))
            .tooltip(tip("Resume", self.theme))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.resume(id, cx);
                window.focus(&this.input.focus_handle(cx), cx);
            }))
    }

    fn composer_ready(&self, thread: &Thread, cx: &Context<Self>) -> bool {
        !self.attachment_jobs.contains(&thread.id)
            && (!thread.draft_attachments.is_empty() || !self.input.read(cx).text().trim().is_empty())
    }

    fn remove_attachment(&mut self, id: u64, i: usize, cx: &mut Context<Self>) {
        if let Some(thread) = self.store.thread_mut(id) {
            if i < thread.draft_attachments.len() {
                thread.draft_attachments.remove(i);
                self.save();
                self.set_attachment_error(id, None, cx);
            }
        }
    }

    pub(super) fn composer(&self, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let running = thread.status == Status::Running;
        let stopping = self.turns.get(&thread.id).is_some_and(|t| t.process.stop_requested());
        let stopped = thread.status == Status::Stopped;
        let id = thread.id;
        let loading = self.attachment_jobs.contains(&id);
        let typed = self.composer_ready(thread, cx);
        let button = if running && typed && !stopping {
            let mark = match self.follow_up_mode(window.modifiers().control) {
                FollowUp::Steer => assets::Icon::CornerDownRight,
                FollowUp::Queue => assets::Icon::ListEnd,
            };
            let hint = match self.store.settings.follow_up {
                FollowUp::Steer => "Steer the running turn (Enter) · Ctrl+Enter to queue instead",
                FollowUp::Queue => "Queue for after this turn (Enter) · Ctrl+Enter to steer instead",
            };
            self.round_button("follow-up", true)
                .child(assets::icon(mark, 14.))
                .tooltip(tip(hint, theme))
                .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                    this.submit(ev.modifiers().control, cx);
                }))
                .into_any_element()
        } else if running {
            self.stop_button(thread.id, !stopping, cx).into_any_element()
        } else if stopped && !typed && !loading {
            self.resume_button(thread.id, cx).into_any_element()
        } else {
            self.send_button(cx).into_any_element()
        };
        centered(
            div().id("composer-card").relative().flex().flex_col().gap_2().w_full().p_2().rounded(px(16.))
                .bg(theme.card).border_1().border_color(theme.border).shadow_md()
                .on_drop(cx.listener(move |this, files: &gpui::ExternalPaths, _, cx| {
                    this.add_files(id, files.paths().to_vec(), cx);
                }))
                .when(!thread.draft_attachments.is_empty(), |d| {
                    d.child(div().flex().flex_wrap().gap_1().px_2()
                        .children(thread.draft_attachments.iter().enumerate().map(|(i, attachment)| {
                            div().flex().items_center().rounded_md().bg(theme.fill).text_color(theme.text)
                                .child(media::attachment_button(format!("draft-preview-{i}"), attachment, theme))
                                .child(div().id(("remove-attachment", i)).p_1()
                                    .cursor_pointer().hover(|h| h.bg(theme.hover))
                                    .tooltip(tip("Remove attachment", theme))
                                    .child(assets::icon(assets::Icon::X, 12.))
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(id, i, cx))))
                        })))
                })
                .when_some(self.attachment_errors.get(&id), |d, error| {
                    d.child(div().px_2().text_xs().text_color(theme.warn).child(error.clone()))
                })
                .children(self.file_menu(id, window, cx))
                .child(
                    div().id("composer")
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.focus(&this.input.focus_handle(cx), cx)
                        }))
                        .px_2().py_1p5().text_sm().child(self.input.clone()),
                )
                .child(
                    div().flex().items_center().gap_3().pl_0p5()
                        .child(div().flex().items_center().gap_0p5()
                            .child(composer_tool("attach-file", assets::Icon::Paperclip, theme)
                                .tooltip(tip("Attach text files, images or PDFs", theme))
                                .on_click(cx.listener(move |this, _, _, cx| this.pick_files(id, cx))))
                            .child(self.model_picker(thread, window, cx))
                            .child(self.effort_picker(thread, window, cx)))
                        .child(div().flex_1().min_w_0().truncate().text_xs()
                            .text_color(theme.muted).text_right()
                            .when(loading, |d| d.child("Adding attachments…"))
                            .when(!loading && running && !stopping, |d| d.child("Esc to stop")))
                        .child(button),
                ),
        )
        .flex_shrink_0()
        .px_6()
        .pt_1()
        .pb_4()
    }
}

fn composer_tool(id: &'static str, icon: assets::Icon, theme: Theme) -> gpui::Stateful<gpui::Div> {
    div().id(id).flex().flex_shrink_0().items_center().justify_center()
        .size(px(26.)).rounded_md().text_color(theme.muted).cursor_pointer()
        .hover(|h| h.bg(theme.hover).text_color(theme.text))
        .child(assets::icon(icon, 14.))
}
