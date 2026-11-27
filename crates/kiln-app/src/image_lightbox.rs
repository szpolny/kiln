use crate::assets::{self, Icon};
use crate::media::{DecodePicture, Picture};
use crate::theme::Theme;
use crate::tooltip::tip;
use crate::workspace::{TOOLBAR, Workspace};
use gpui::{
    App, Bounds, ClickEvent, Context, Div, FocusHandle, KeyDownEvent, MouseButton, Pixels, Point,
    Size, Stateful, Window, div, img, point, prelude::*, px,
};

const ZOOM_STEP: f32 = 1.25;
const MAX_SCALE: f32 = 4.;

pub(crate) struct ImageLightbox {
    title: String,
    picture: Picture,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    scale: Option<f32>,
    pan: Point<Pixels>,
    drag: Option<Point<Pixels>>,
}

pub(crate) fn open(title: String, picture: Picture, window: &mut Window, cx: &mut App) {
    if let Some(workspace) = window.root::<Workspace>().flatten() {
        workspace.update(cx, |this, cx| {
            let focus = cx.focus_handle();
            let previous_focus = window.focused(cx);
            this.menu = None;
            this.palette = None;
            window.focus(&focus, cx);
            this.image_lightbox = Some(ImageLightbox {
                title,
                picture,
                focus,
                previous_focus,
                scale: None,
                pan: Point::default(),
                drag: None,
            });
            cx.notify();
        });
    }
}

#[derive(Clone, Copy)]
struct Viewport {
    image: Size<Pixels>,
    canvas: Size<Pixels>,
}

impl Viewport {
    fn fit(self) -> f32 {
        let padding = px(40.);
        let width = (self.canvas.width - padding).max(px(1.));
        let height = (self.canvas.height - padding).max(px(1.));
        (width / self.image.width)
            .min(height / self.image.height)
            .min(1.)
    }

    fn center(self) -> Point<Pixels> {
        point(self.canvas.width / 2., self.canvas.height / 2.)
    }

    fn pan_limit(self, scale: f32) -> Point<Pixels> {
        point(
            ((self.image.width * scale - self.canvas.width) / 2.).max(px(0.)),
            ((self.image.height * scale - self.canvas.height) / 2.).max(px(0.)),
        )
    }

    fn clamp_pan(self, pan: Point<Pixels>, scale: f32) -> Point<Pixels> {
        let limit = self.pan_limit(scale);
        pan.clamp(&point(-limit.x, -limit.y), &limit)
    }
}

impl ImageLightbox {
    fn scale(&self, viewport: Viewport) -> f32 {
        let fit = viewport.fit();
        self.scale.unwrap_or(fit).max(fit)
    }

    fn zoom(&mut self, viewport: Viewport, factor: f32, anchor: Point<Pixels>) {
        let old_scale = self.scale(viewport);
        let scale = (old_scale * factor).clamp(viewport.fit(), MAX_SCALE);
        let pan = viewport.clamp_pan(self.pan, old_scale);
        let offset = anchor - viewport.center();
        self.pan = viewport.clamp_pan(offset + (pan - offset) * (scale / old_scale), scale);
        self.scale = (scale > viewport.fit()).then_some(scale);
    }

    fn reset(&mut self) {
        self.scale = None;
        self.pan = Point::default();
        self.drag = None;
    }
}

impl Workspace {
    fn close_image_lightbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(lightbox) = self.image_lightbox.take() {
            if let Some(focus) = lightbox.previous_focus {
                window.focus(&focus, cx);
            } else {
                window.blur(cx);
            }
            cx.notify();
        }
    }

    fn update_image_lightbox(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut ImageLightbox),
    ) {
        if let Some(lightbox) = self.image_lightbox.as_mut() {
            update(lightbox);
            cx.notify();
        }
    }

    pub(crate) fn render_image_lightbox(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let lightbox = self.image_lightbox.as_ref()?;
        let theme = self.theme;
        let toolbar_height = px(TOOLBAR);
        let mut canvas_size = window.viewport_size();
        canvas_size.height = (canvas_size.height - toolbar_height).max(px(1.));
        let asset = window.use_asset::<DecodePicture>(&lightbox.picture, cx);
        let viewport = asset
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|image| Viewport {
                image: image.size(0).map(|pixels| px(u32::from(pixels) as f32)),
                canvas: canvas_size,
            });
        let scale = viewport.map(|viewport| lightbox.scale(viewport));
        let controls = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .when_some(scale, |d, scale| {
                d.child(
                    button(
                        "image-zoom-fit",
                        "Click to fit · Scroll or pinch to zoom",
                        theme,
                    )
                    .px_2()
                    .text_xs()
                    .child(format!("{:.0}%", scale * 100.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update_image_lightbox(cx, ImageLightbox::reset)
                    })),
                )
            })
            .child(
                button("image-lightbox-close", "Close image (Esc)", theme)
                    .child(assets::icon(Icon::X, 17.))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.close_image_lightbox(window, cx)),
                    ),
            );

        let canvas_origin = point(px(0.), toolbar_height);
        let mut image_bounds = None;
        let mut canvas = div()
            .id("image-lightbox-canvas")
            .relative()
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center();
        if let (Some(Ok(image)), Some(viewport), Some(scale)) = (asset.as_ref(), viewport, scale) {
            let dimensions = viewport.image.map(|length| length * scale);
            let pan = viewport.clamp_pan(lightbox.pan, scale);
            let center = viewport.center();
            let origin = center + pan - point(dimensions.width / 2., dimensions.height / 2.);
            let bounds = Bounds::new(origin + canvas_origin, dimensions);
            image_bounds = Some(bounds);
            let limit = viewport.pan_limit(scale);
            let pannable = limit.x > px(0.) || limit.y > px(0.);
            canvas = canvas
                .child(
                    img(image.clone())
                        .absolute()
                        .object_fit(gpui::ObjectFit::Fill)
                        .left(origin.x)
                        .top(origin.y)
                        .w(dimensions.width)
                        .h(dimensions.height),
                )
                .on_scroll(cx.listener(move |this, event: &gpui::ScrollEvent, _, cx| {
                    let delta = f32::from(event.delta.pixel_delta(px(32.)).y);
                    let anchor = event.position - canvas_origin;
                    this.update_image_lightbox(cx, |viewer| {
                        viewer.zoom(viewport, (delta * 0.006).exp(), anchor)
                    });
                    cx.stop_propagation();
                }))
                .on_pinch(cx.listener(move |this, event: &gpui::PinchEvent, _, cx| {
                    let anchor = event.position - canvas_origin;
                    this.update_image_lightbox(cx, |viewer| {
                        viewer.zoom(viewport, 1. + event.delta, anchor)
                    });
                    cx.stop_propagation();
                }))
                .when(pannable, |d| {
                    d.when(lightbox.drag.is_some(), |d| d.cursor_grabbing())
                        .when(lightbox.drag.is_none(), |d| d.cursor_grab())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                if bounds.contains(&event.position) {
                                    this.update_image_lightbox(cx, |viewer| {
                                        viewer.drag = Some(event.position)
                                    });
                                }
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            move |this, event: &gpui::MouseMoveEvent, _, cx| {
                                if this
                                    .image_lightbox
                                    .as_ref()
                                    .is_some_and(|viewer| viewer.drag.is_some())
                                {
                                    this.update_image_lightbox(cx, |viewer| {
                                        if event.dragging() {
                                            let previous = viewer
                                                .drag
                                                .replace(event.position)
                                                .unwrap_or(event.position);
                                            viewer.pan = viewport.clamp_pan(
                                                viewport.clamp_pan(viewer.pan, scale)
                                                    + event.position
                                                    - previous,
                                                scale,
                                            );
                                        } else {
                                            viewer.drag = None;
                                        }
                                    });
                                }
                            },
                        ))
                });
        } else {
            let status = if asset.is_some() {
                "Image unavailable"
            } else {
                "Loading image…"
            };
            canvas = canvas.child(div().text_sm().text_color(theme.muted).child(status));
        }
        let canvas = canvas.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            let outside = |position| image_bounds.is_none_or(|bounds| !bounds.contains(&position));
            if let ClickEvent::Mouse(mouse) = event {
                let distance = mouse.up.position - mouse.down.position;
                if distance.x.abs() > px(4.)
                    || distance.y.abs() > px(4.)
                    || !outside(mouse.down.position)
                {
                    return;
                }
            }
            if outside(event.position()) {
                this.close_image_lightbox(window, cx);
            }
        }));

        Some(
            div().absolute().inset_0().child(
                div()
                    .id("image-lightbox")
                    .size_full()
                    .flex()
                    .flex_col()
                    .track_focus(&lightbox.focus)
                    .tab_group()
                    .tab_stop(false)
                    .bg(theme.window.opacity(0.95))
                    .occlude()
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            if this
                                .image_lightbox
                                .as_mut()
                                .is_some_and(|viewer| viewer.drag.take().is_some())
                            {
                                cx.notify();
                            }
                        }),
                    )
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => {
                                cx.stop_propagation();
                                if !event.is_held {
                                    this.close_image_lightbox(window, cx);
                                }
                            }
                            "tab" => {
                                cx.stop_propagation();
                                if event.keystroke.modifiers.shift {
                                    window.focus_prev(cx);
                                } else {
                                    window.focus_next(cx);
                                }
                            }
                            "0" | "+" | "=" | "-"
                                if event.keystroke.modifiers == Default::default()
                                    || event.keystroke.modifiers
                                        == gpui::Modifiers {
                                            shift: true,
                                            ..Default::default()
                                        } =>
                            {
                                if let Some(viewport) = viewport {
                                    this.update_image_lightbox(cx, |viewer| {
                                        match event.keystroke.key.as_str() {
                                            "0" => viewer.reset(),
                                            "-" => viewer.zoom(
                                                viewport,
                                                1. / ZOOM_STEP,
                                                viewport.center(),
                                            ),
                                            _ => {
                                                viewer.zoom(viewport, ZOOM_STEP, viewport.center())
                                            }
                                        }
                                    });
                                }
                                cx.stop_propagation();
                            }
                            _ => {}
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_shrink_0()
                            .gap_3()
                            .h(toolbar_height)
                            .pl(px(if cfg!(target_os = "macos") { 88. } else { 16. }))
                            .pr_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(lightbox.title.clone()),
                            )
                            .child(controls),
                    )
                    .child(canvas),
            ),
        )
    }
}

fn button(id: &'static str, label: &'static str, theme: Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(28.))
        .min_w(px(28.))
        .rounded_md()
        .border_1()
        .border_color(gpui::transparent_black())
        .text_color(theme.text)
        .tooltip(tip(label, theme))
        .tab_stop(true)
        .cursor_pointer()
        .hover(|h| h.bg(theme.hover))
        .focus(|s| s.bg(theme.hover).border_color(theme.accent))
}
