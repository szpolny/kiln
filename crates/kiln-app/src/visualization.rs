use crate::media::{DecodePicture, Picture};
use crate::theme::Theme;
use gpui::{div, img, prelude::*, px, App, Div, Stateful, Window};
use std::sync::Arc;

#[cfg(target_os = "macos")]
fn themed_html(html: &str, theme: Theme, interactive: bool) -> String {
    use gpui::Rgba;
    use std::fmt::Write;

    let mut document = String::from("<style>:root{");
    document.push_str(if theme.dark { "color-scheme:dark;" } else { "color-scheme:light;" });
    for (name, Rgba { r, g, b, a }) in [
        ("bg", theme.window),
        ("text", theme.text),
        ("muted", theme.muted),
        ("border", theme.border),
        ("fill", theme.fill),
        ("accent", theme.accent),
        ("on-accent", theme.on_accent),
    ] {
        let channel = |value: f32| (value * 255.).round() as u8;
        let _ = write!(document, "--kiln-{name}:#{:02x}{:02x}{:02x}{:02x};",
            channel(r), channel(g), channel(b), channel(a));
    }
    document.push_str(concat!(
        "background:var(--kiln-bg);color:var(--kiln-text);font:14px/1.55 system-ui;",
        "accent-color:var(--kiln-accent)}",
        "body{margin:0}button,input,select,textarea{font:inherit}",
        "h1,h2,h3{font-size:1em;font-weight:600;margin:0 0 .5em}",
        "svg text{font-family:inherit}button:focus-visible,input:focus-visible,select:focus-visible{",
        "outline:2px solid var(--kiln-accent);outline-offset:2px}",
    ));
    if interactive {
        document.push_str("body{padding:16px}");
    }
    document.push_str("</style>");
    document.push_str(html);
    document
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{themed_html, Theme};
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::ffi::{c_char, c_void, CStr};
    use std::sync::Arc;

    type Preview = Result<Arc<str>, String>;
    type SnapshotCallback = unsafe extern "C" fn(*mut c_void, *const u8, usize, *const c_char);

    extern "C" {
        fn kiln_visualization_snapshot(
            html: *const u8, len: usize, dark: bool, context: *mut c_void, callback: SnapshotCallback,
        );
        fn kiln_visualization_open(
            html: *const u8, len: usize, title: *const u8, title_len: usize, dark: bool,
        ) -> bool;
    }

    pub async fn snapshot(html: &str, dark: bool) -> Preview {
        let appearance = if dark { kiln_core::Appearance::Dark } else { kiln_core::Appearance::Light };
        let html = themed_html(html, Theme::new(gpui::WindowAppearance::Light, appearance, ".SystemUIFont"), false);
        let (sender, receiver) = async_channel::bounded::<Preview>(1);
        unsafe extern "C" fn complete(
            context: *mut c_void, png: *const u8, len: usize, error: *const c_char,
        ) {
            // The native renderer calls this once. Its buffers live through this callback.
            let sender = unsafe { Box::from_raw(context.cast::<async_channel::Sender<Preview>>()) };
            let preview = if !error.is_null() {
                Err(unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned())
            } else if png.is_null() || len == 0 || len > 4 * 1024 * 1024 {
                Err("The visualization preview is empty or too large.".into())
            } else {
                Ok(STANDARD.encode(unsafe { std::slice::from_raw_parts(png, len) }).into())
            };
            let _ = sender.try_send(preview);
        }
        let context = Box::into_raw(Box::new(sender)).cast();
        // This function copies the document before dispatching to AppKit.
        unsafe { kiln_visualization_snapshot(html.as_ptr(), html.len(), dark, context, complete) };
        receiver.recv().await.map_err(|_| "The visualization renderer stopped.".to_string())?
    }

    pub fn open(html: &str, title: &str, theme: Theme) -> Result<(), &'static str> {
        let html = themed_html(html, theme, true);
        // GPUI invokes this from the button's click handler on the main thread.
        unsafe { kiln_visualization_open(html.as_ptr(), html.len(), title.as_ptr(), title.len(), theme.dark) }
            .then_some(())
            .ok_or("This system WebKit cannot isolate visualizations.")
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::sync::Arc;

    const UNAVAILABLE: &str = "Visualization rendering is currently available on macOS.";

    pub async fn snapshot(_: &str, _: bool) -> Result<Arc<str>, String> {
        Err(UNAVAILABLE.into())
    }

    pub fn open(_: &str, _: &str, _: super::Theme) -> Result<(), &'static str> {
        Err(UNAVAILABLE)
    }
}

pub use platform::snapshot;

#[derive(IntoElement)]
struct InlinePreview {
    picture: Picture,
    theme: Theme,
}

impl RenderOnce for InlinePreview {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let result = window.use_asset::<DecodePicture>(&self.picture, cx);
        match result {
            Some(Ok(image)) => {
                let size = image.size(0);
                let ratio = u32::from(size.width) as f32 / u32::from(size.height) as f32;
                div().relative().w_full().aspect_ratio(ratio).overflow_hidden()
                    .child(img(image).absolute().inset_0().size_full().object_fit(gpui::ObjectFit::Contain))
            }
            other => div().text_xs().text_color(self.theme.muted).child(if other.is_some() {
                "Visualization preview unavailable"
            } else {
                "Loading visualization…"
            }),
        }
    }
}

pub fn inline(
    id: String,
    title: String,
    html: Arc<str>,
    preview: &Result<Arc<str>, String>,
    theme: Theme,
    dark_preview: Option<&Arc<str>>,
) -> Stateful<Div> {
    let button = div()
        .id("interact")
        .cursor_pointer()
        .tab_stop(true)
        .rounded_md()
        .px_1p5()
        .py_0p5()
        .text_xs()
        .text_color(theme.muted)
        .hover(|style| style.bg(theme.hover))
        .focus(|style| style.bg(theme.hover))
        .child("Open")
        .tooltip(crate::tooltip::tip("Open interactive visualization", theme))
        .on_click({
            let title = title.clone();
            move |_, window, cx| {
                if let Err(error) = platform::open(&html, &title, theme) {
                    drop(window.prompt(gpui::PromptLevel::Warning, &title, Some(error), &["OK"], cx));
                }
            }
        });
    let row = div()
        .id(id)
        .w_full()
        .max_w(px(560.))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div().flex().items_center().gap_2().text_xs().text_color(theme.muted)
                .child(div().min_w_0().truncate().child(title))
                .when(cfg!(target_os = "macos"), |row| row.child(button)),
        );
    match preview {
        Ok(png) => row.child(InlinePreview {
            picture: Picture::embedded(if theme.dark { dark_preview.unwrap_or(png) } else { png }.clone()),
            theme,
        }),
        Err(error) => row.child(div().text_xs().text_color(theme.muted).child(error.clone())),
    }
}
