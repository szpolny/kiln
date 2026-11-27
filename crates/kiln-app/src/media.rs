//! Local attachments and previews. Image loading never uses a URL client.

use crate::theme::Theme;
use base64::{engine::general_purpose::STANDARD, Engine};
use gpui::{
    div, img, prelude::*, px, size, App, Asset, Bounds, Context, RenderImage, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use image::{ImageFormat, ImageReader, Limits};
use kiln_core::{Attachment, AttachmentContent};
use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TEXT_LIMIT: usize = 64 * 1024;
const IMAGE_LIMIT: usize = 5 * 1024 * 1024;
pub const TOTAL_LIMIT: usize = 20 * 1024 * 1024;
pub const COUNT_LIMIT: usize = 8;
const PREVIEW_LIMIT: usize = 10 * 1024 * 1024;
const THUMBNAIL_SIDE: u32 = 64;

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let metadata = path.metadata().map_err(|err| err.to_string())?;
    if !metadata.is_file() {
        return Err("Choose a regular file.".into());
    }
    if metadata.len() > limit as u64 {
        return Err(format!("File exceeds {} MiB.", limit / (1024 * 1024)));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|err| err.to_string())?;
    if bytes.len() > limit {
        return Err("File exceeds the size limit.".into());
    }
    Ok(bytes)
}

pub fn attach_file(path: &Path) -> Result<Attachment, String> {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned();
    let bytes = read(path, IMAGE_LIMIT)?;
    if image::guess_format(&bytes).is_ok() {
        return attach_image(name, &bytes);
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "Choose a UTF-8 text file or a supported image.")?;
    if text.contains('\0') {
        return Err("Choose a UTF-8 text file or a supported image.".into());
    }
    if bytes.len() > TEXT_LIMIT {
        return Err("Text files can be up to 64 KiB.".into());
    }
    Ok(Attachment {
        name,
        content: AttachmentContent::Text(text.into()),
    })
}

fn image_info(bytes: &[u8]) -> Result<(ImageFormat, &'static str), String> {
    let format = image::guess_format(bytes).map_err(|_| "Unrecognized image format.")?;
    let mime = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Bmp | ImageFormat::Tiff => "image/png",
        _ => return Err("Supported images are PNG, JPEG, GIF, WebP, BMP and TIFF.".into()),
    };
    Ok((format, mime))
}

fn decode(bytes: &[u8], format: ImageFormat) -> Result<image::DynamicImage, String> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|err| format!("Could not decode image: {err}"))
}

pub fn attach_image(name: String, bytes: &[u8]) -> Result<Attachment, String> {
    snapshot_image(name, bytes, IMAGE_LIMIT)
}

fn snapshot_image(name: String, bytes: &[u8], limit: usize) -> Result<Attachment, String> {
    if bytes.len() > limit {
        return Err(format!(
            "Images can be up to {} MiB.",
            limit / (1024 * 1024)
        ));
    }
    let (format, mime) = image_info(bytes)?;
    let image = decode(bytes, format)?;
    let mut converted = Cursor::new(Vec::new());
    let bytes = if matches!(format, ImageFormat::Bmp | ImageFormat::Tiff) {
        image
            .write_to(&mut converted, ImageFormat::Png)
            .map_err(|err| err.to_string())?;
        converted.get_ref().as_slice()
    } else {
        bytes
    };
    if bytes.len() > limit {
        return Err("Image exceeds the size limit after conversion to PNG.".into());
    }
    Ok(Attachment {
        name,
        content: AttachmentContent::Image {
            mime: mime.into(),
            base64: STANDARD.encode(bytes).into(),
        },
    })
}

pub fn generated_image(path: &Path, checkout: &Path, scratch: &Path) -> Result<Attachment, String> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")));
    let path = reply_image_path(Some(checkout), scratch, path)
        .or_else(|| scoped_path(&codex_home?.join("generated_images"), path))
        .ok_or("The generated image is missing or outside the checkout, thread scratch directory, and Codex image folder.")?;
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned();
    snapshot_image(name, &read(&path, PREVIEW_LIMIT)?, PREVIEW_LIMIT)
}

pub fn scoped_path(root: &Path, path: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let path = root.join(path).canonicalize().ok()?;
    path.starts_with(&root).then_some(path)
}

pub fn scratch_root(dir: &Path) -> Option<PathBuf> {
    if dir.symlink_metadata().ok()?.is_dir() {
        dir.canonicalize().ok()
    } else {
        None
    }
}

fn reply_image_path(checkout: Option<&Path>, scratch: &Path, path: &Path) -> Option<PathBuf> {
    let checkout = checkout.and_then(|root| root.canonicalize().ok());
    let target = if path.is_absolute() {
        path.canonicalize().ok()?
    } else {
        checkout.as_ref()?.join(path).canonicalize().ok()?
    };
    let scratch_parent = scratch.parent().and_then(|dir| dir.canonicalize().ok());
    // Even an enclosing checkout cannot expose another thread's scratch files.
    if scratch_parent.is_some_and(|parent| target.starts_with(parent)) {
        return (path.is_absolute() && target.starts_with(scratch_root(scratch)?)).then_some(target);
    }
    checkout.is_some_and(|root| target.starts_with(root)).then_some(target)
}

pub fn link_path(target: &str) -> Option<PathBuf> {
    let target = target
        .strip_prefix("sandbox:")
        .or_else(|| {
            target
                .strip_prefix("file://")
                .filter(|path| path.starts_with('/'))
        })
        .unwrap_or(target);
    let path = target.split('#').next()?;
    let path = path
        .split_once(':')
        .map_or(Some(path), |(name, location)| {
            location
                .split(':')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then_some(name)
        })?;
    let path = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    (!path.is_empty() && !path.starts_with("//")).then(|| PathBuf::from(path.as_ref()))
}

#[derive(Clone)]
pub enum Picture {
    Local { checkout: Option<PathBuf>, scratch: PathBuf, path: PathBuf },
    Embedded { data: Arc<str>, hash: u64 },
}

impl Picture {
    pub fn embedded(data: Arc<str>) -> Self {
        let mut hasher = DefaultHasher::new();
        data.hash(&mut hasher);
        Self::Embedded {
            data,
            hash: hasher.finish(),
        }
    }
}

impl Hash for Picture {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Local { checkout, scratch, path } => {
                checkout.hash(state);
                scratch.hash(state);
                path.hash(state);
            }
            Self::Embedded { hash, .. } => hash.hash(state),
        }
    }
}

pub(crate) struct DecodePicture;
pub(crate) struct DecodeThumbnail;

impl Asset for DecodeThumbnail {
    type Source = Picture;
    type Output = Result<Arc<RenderImage>, String>;

    fn load(source: Picture, cx: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        cx.background_executor().spawn(async move { decode_picture(source, Some(THUMBNAIL_SIDE)) })
    }
}

impl Asset for DecodePicture {
    type Source = Picture;
    type Output = Result<Arc<RenderImage>, String>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        cx.background_executor().spawn(async move { decode_picture(source, None) })
    }
}

fn decode_picture(source: Picture, thumbnail_side: Option<u32>) -> Result<Arc<RenderImage>, String> {
    let (bytes, local_extension) = match source {
        Picture::Local { checkout, scratch, path } => {
            let resolved = reply_image_path(checkout.as_deref(), &scratch, &path)
                .ok_or("Image is missing or outside the checkout and thread scratch directory.")?;
            let extension = path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase);
            (read(&resolved, PREVIEW_LIMIT)?, extension)
        }
        Picture::Embedded { data, .. } => {
            if data.len() > PREVIEW_LIMIT.div_ceil(3) * 4 {
                return Err("Image exceeds the preview limit.".into());
            }
            (STANDARD.decode(&*data).map_err(|_| "Invalid image data.")?, None)
        }
    };
    let mut pixels = match local_extension.as_deref() {
        Some("svg") => crate::project_icon::svg(&bytes, thumbnail_side.unwrap_or(2048))
            .ok_or("Could not decode SVG image.")?,
        extension => {
            let format = if extension == Some("ico") && matches!(image::guess_format(&bytes), Ok(ImageFormat::Ico)) {
                ImageFormat::Ico
            } else {
                image_info(&bytes)?.0
            };
            let image = decode(&bytes, format)?;
            match thumbnail_side {
                Some(side) if image.width().max(image.height()) > side => image.thumbnail(side, side).into_rgba8(),
                _ => image.into_rgba8(),
            }
        }
    };
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new([image::Frame::new(pixels)])))
}

#[derive(IntoElement)]
struct PreviewPicture {
    picture: Picture,
    label: String,
    theme: Theme,
    id: SharedString,
}

impl RenderOnce for PreviewPicture {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let status = |text: String| {
            div()
                .text_sm()
                .text_color(self.theme.muted)
                .child(text)
                .into_any_element()
        };
        let image = match window.use_asset::<DecodePicture>(&self.picture, cx) {
            Some(Ok(image)) => image,
            Some(Err(_)) => return status(format!("Image unavailable · {}", self.label)),
            None => return status(format!("Loading {}…", self.label)),
        };
        let dimensions = image.size(0).map(|pixels| px(u32::from(pixels) as f32));
        // Leave room for the sidebar, transcript padding, and nested Markdown.
        let width = (window.viewport_size().width - px(344.)).clamp(px(60.), px(660.));
        let height = px(260.);
        let scale = (width / dimensions.width).min(height / dimensions.height);
        let image = img(image)
            .object_fit(gpui::ObjectFit::Fill)
            .w(dimensions.width * scale)
            .h(dimensions.height * scale)
            .max_w_full();
        let Self { picture, label, theme, id } = self;
        div()
            .id(id)
            .self_start()
            .cursor_pointer()
            .rounded_md()
            .overflow_hidden()
            .tooltip(crate::tooltip::tip("Open image viewer", theme))
            .child(image)
            .on_click(move |_, window, cx| {
                crate::image_lightbox::open(label.clone(), picture.clone(), window, cx)
            })
            .into_any_element()
    }
}

pub fn inline(picture: Picture, label: String, id: SharedString, theme: Theme) -> impl IntoElement {
    PreviewPicture {
        picture,
        label,
        theme,
        id,
    }
}

pub fn preview_attachment(attachment: &Attachment, theme: Theme, window: &mut Window, cx: &mut App) {
    match &attachment.content {
        AttachmentContent::Text(text) => open_text_preview(attachment.name.clone(), text.clone(), theme, cx),
        AttachmentContent::Image { base64, .. } => {
            crate::image_lightbox::open(attachment.name.clone(), Picture::embedded(base64.clone()), window, cx)
        }
        AttachmentContent::Visualization { .. } => {}
    }
}

fn open_text_preview(title: String, text: Arc<str>, theme: Theme, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(920.), px(660.)), cx);
    let _ = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone().into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |_, cx| {
            cx.new(|_| TextPreview { text, theme })
        },
    );
}

struct TextPreview {
    text: Arc<str>,
    theme: Theme,
}

impl Render for TextPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        div()
            .id("attachment-text")
            .size_full()
            .overflow_scroll()
            .p_4()
            .text_sm()
            .font_family(crate::theme::MONO)
            .bg(theme.card)
            .text_color(theme.text)
            .child(SharedString::from(self.text.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_attachments_capture_bytes_and_reject_invalid_input() {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let bytes = png.into_inner();
        let attachment = attach_image("Screenshot".into(), &bytes).unwrap();
        let AttachmentContent::Image { mime, base64 } = attachment.content else {
            panic!()
        };
        assert_eq!(mime, "image/png");
        assert_eq!(STANDARD.decode(&*base64).unwrap(), bytes);
        assert!(attach_image("Broken image".into(), &bytes[..8]).is_err());
    }

    #[test]
    fn file_references_recognize_local_paths_only() {
        assert_eq!(link_path("src/main.rs:30:4"), Some("src/main.rs".into()));
        assert_eq!(
            link_path("file:///checkout/my%20image.png"),
            Some("/checkout/my image.png".into())
        );
        assert_eq!(
            link_path("sandbox:/checkout/image.png"),
            Some("/checkout/image.png".into())
        );
        assert_eq!(link_path("https://example.com/image.png"), None);
        assert_eq!(link_path("file://server/image.png"), None);
        assert_eq!(link_path("//example.com/image.png"), None);
        assert_eq!(
            link_path("plots/a%23b.png#fragment"),
            Some("plots/a#b.png".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn reply_previews_are_scoped_to_the_checkout_and_scratch_directory() {
        let dir = std::env::temp_dir().join(format!("kiln-scoped-preview-{}", std::process::id()));
        let root = dir.join("checkout");
        let scratch = dir.join("tmp/1");
        let sibling = dir.join("tmp/2");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(root.join("local.png"), "").unwrap();
        let preview = scratch.join("preview.png");
        let sibling_preview = sibling.join("preview.png");
        std::fs::write(&preview, "").unwrap();
        std::fs::write(&sibling_preview, "").unwrap();
        std::fs::write(dir.join("outside.png"), "").unwrap();
        std::os::unix::fs::symlink("../outside.png", root.join("link.png")).unwrap();
        std::os::unix::fs::symlink("../../outside.png", scratch.join("link.png")).unwrap();
        std::os::unix::fs::symlink("../2/preview.png", scratch.join("sibling.png")).unwrap();
        let aliased_scratch = dir.join("tmp/3");
        std::os::unix::fs::symlink("2", &aliased_scratch).unwrap();
        assert_eq!(
            reply_image_path(Some(&root), &scratch, Path::new("local.png")),
            Some(root.canonicalize().unwrap().join("local.png"))
        );
        assert_eq!(
            reply_image_path(Some(&root), &scratch, &preview),
            Some(preview.canonicalize().unwrap())
        );
        assert_eq!(
            reply_image_path(None, &scratch, &preview),
            Some(preview.canonicalize().unwrap())
        );
        assert_eq!(reply_image_path(Some(&dir), &scratch, &sibling_preview), None);
        assert_eq!(reply_image_path(None, &aliased_scratch, &sibling_preview), None);
        for path in [
            PathBuf::from("preview.png"),
            PathBuf::from("../outside.png"),
            PathBuf::from("link.png"),
            scratch.join("link.png"),
            scratch.join("sibling.png"),
            sibling_preview,
            dir.join("outside.png"),
        ] {
            assert_eq!(reply_image_path(Some(&root), &scratch, &path), None);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
