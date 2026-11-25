//! A project's icon file turned into the small picture the sidebar draws.

use gpui::RenderImage;
use image::{imageops, Frame, ImageReader, Limits, RgbaImage};
use kiln_agent::project;
use resvg::{tiny_skia, usvg};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

/// Longest side of a prepared icon: twice the size it is drawn at. GPUI shrinks
/// images with plain bilinear filtering, so a larger one would come out jagged.
const SIDE: u32 = 32;
/// Largest picture, in pixels per side, that is decoded.
const MAX_SIDE: u32 = 4096;

/// The icon of the project in `dir`. Reads and decodes files, so call it off the
/// UI thread.
pub fn load(dir: &Path) -> Option<Arc<RenderImage>> {
    project::icon(dir, |bytes| {
        let mut pixels = match image::guess_format(bytes) {
            Ok(_) => raster(bytes)?,
            Err(_) => svg(bytes)?,
        };
        // An invisible icon, such as an SVG that is only text, is no icon.
        if pixels.pixels().all(|p| p[3] == 0) {
            return None;
        }
        // GPUI wants blue first.
        pixels.pixels_mut().for_each(|p| p.0.swap(0, 2));
        Some(Arc::new(RenderImage::new(vec![Frame::new(pixels)])))
    })
}

/// Whether two icons look the same.
pub fn same(a: &RenderImage, b: &RenderImage) -> bool {
    a.size(0) == b.size(0) && a.as_bytes(0) == b.as_bytes(0)
}

fn raster(bytes: &[u8]) -> Option<RgbaImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    let mut pixels = reader.decode().ok()?.into_rgba8();
    let (width, height) = pixels.dimensions();
    let longest = width.max(height);
    if longest <= SIDE {
        return Some(pixels);
    }
    // Weigh colors by opacity, or unseen pixels would tint the edges.
    for p in pixels.pixels_mut() {
        let alpha = u16::from(p[3]);
        p.0[..3].iter_mut().for_each(|c| *c = ((u16::from(*c) * alpha + 127) / 255) as u8);
    }
    let fit = |side: u32| (side * SIDE / longest).max(1);
    let mut small =
        imageops::resize(&pixels, fit(width), fit(height), imageops::FilterType::Triangle);
    small.pixels_mut().for_each(|p| straighten(&mut p.0));
    Some(small)
}

fn svg(bytes: &[u8]) -> Option<RgbaImage> {
    let options = usvg::Options {
        // An SVG may name other files. Only what it embeds is drawn.
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_string: Box::new(|_, _| None),
            ..Default::default()
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let size = tree.size();
    let scale = SIDE as f32 / size.width().max(size.height());
    let fit = |side: f32| ((side * scale).round() as u32).clamp(1, SIDE);
    let mut canvas = tiny_skia::Pixmap::new(fit(size.width()), fit(size.height()))?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut canvas.as_mut());
    let mut pixels = RgbaImage::from_raw(canvas.width(), canvas.height(), canvas.take())?;
    pixels.pixels_mut().for_each(|p| straighten(&mut p.0));
    Some(pixels)
}

/// Undo premultiplied alpha.
fn straighten(pixel: &mut [u8; 4]) {
    let alpha = u32::from(pixel[3]);
    if alpha > 0 {
        pixel[..3].iter_mut().for_each(|c| {
            *c = ((u32::from(*c) * 255 + alpha / 2) / alpha).min(255) as u8;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba};
    use std::fs;

    #[test]
    fn prepares_small_pictures_from_png_ico_and_svg() {
        let dir = std::env::temp_dir().join(format!("kiln-picture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let bytes_of = |icon: Arc<RenderImage>| icon.as_bytes(0).unwrap().to_vec();
        let size_of = |icon: &RenderImage| (icon.size(0).width.0, icon.size(0).height.0);

        // Large pictures shrink, keeping their shape; blue comes first.
        let red = RgbaImage::from_pixel(200, 100, Rgba([255, 0, 0, 255]));
        red.save_with_format(dir.join("favicon.png"), ImageFormat::Png).unwrap();
        let icon = load(&dir).unwrap();
        assert_eq!(size_of(&icon), (32, 16));
        assert_eq!(bytes_of(icon)[..4], [0, 0, 255, 255]);

        // Small ones are left alone.
        let dot = RgbaImage::from_pixel(16, 16, Rgba([0, 255, 0, 255]));
        dot.save_with_format(dir.join("favicon.ico"), ImageFormat::Ico).unwrap();
        assert_eq!(size_of(&load(&dir).unwrap()), (16, 16));

        let square = r##"<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512">
            <rect width="512" height="512" fill="#0000ff"/></svg>"##;
        fs::write(dir.join("favicon.svg"), square).unwrap();
        let icon = load(&dir).unwrap();
        assert_eq!(size_of(&icon), (32, 32));
        assert!(same(&icon, &load(&dir).unwrap()));
        assert_eq!(bytes_of(icon)[..4], [255, 0, 0, 255]);

        // An SVG cannot pull in another file, which leaves it empty, so the
        // next candidate is used.
        let linking = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">
            <image href="{}" width="32" height="32"/></svg>"#,
            dir.join("favicon.png").display()
        );
        fs::write(dir.join("favicon.svg"), linking).unwrap();
        assert_eq!(size_of(&load(&dir).unwrap()), (16, 16));

        for junk in ["favicon.svg", "favicon.ico", "favicon.png"] {
            fs::write(dir.join(junk), "not a picture").unwrap();
        }
        assert!(load(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
