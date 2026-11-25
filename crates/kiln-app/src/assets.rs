//! Icons compiled into the binary, so nothing is loaded from disk or the network.
use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

pub const CLAUDE: &str = "icons/claude.svg";

// GPUI rasterizes an SVG image at twice the size the file declares and shrinks it
// with plain bilinear filtering, so each brand file declares the size it is drawn
// at. A larger one comes out jagged.

/// Size of the mark files, in pixels.
pub const LOGO_SIZE: f32 = 56.;
/// Width and height of the lockup files, in pixels.
pub const LOCKUP_SIZE: (f32, f32) = (45., 18.);

/// The Kiln mark, drawn for dark and for light backgrounds.
pub const LOGO_DARK: &str = "brand/kiln-mark-dark.svg";
pub const LOGO_LIGHT: &str = "brand/kiln-mark-light.svg";
/// The small mark beside the "kiln" wordmark. The text is outlines, so it looks
/// the same with any installed fonts.
pub const LOCKUP_DARK: &str = "brand/kiln-lockup-dark.svg";
pub const LOCKUP_LIGHT: &str = "brand/kiln-lockup-light.svg";

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            CLAUDE => Some(Cow::Borrowed(include_bytes!("../assets/icons/claude.svg"))),
            LOGO_DARK => Some(Cow::Borrowed(include_bytes!("../assets/brand/kiln-mark-dark.svg"))),
            LOGO_LIGHT => Some(Cow::Borrowed(include_bytes!("../assets/brand/kiln-mark-light.svg"))),
            LOCKUP_DARK => Some(Cow::Borrowed(include_bytes!("../assets/brand/kiln-lockup-dark.svg"))),
            LOCKUP_LIGHT => Some(Cow::Borrowed(include_bytes!("../assets/brand/kiln-lockup-light.svg"))),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![
            CLAUDE.into(),
            LOGO_DARK.into(),
            LOGO_LIGHT.into(),
            LOCKUP_DARK.into(),
            LOCKUP_LIGHT.into(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_files_declare_the_size_they_are_drawn_at() {
        let mark = (LOGO_SIZE, LOGO_SIZE);
        for (path, (width, height)) in [
            (LOGO_DARK, mark),
            (LOGO_LIGHT, mark),
            (LOCKUP_DARK, LOCKUP_SIZE),
            (LOCKUP_LIGHT, LOCKUP_SIZE),
        ] {
            let file = Assets.load(path).unwrap().unwrap();
            let size = format!("width=\"{width}\" height=\"{height}\">");
            assert!(String::from_utf8_lossy(&file).contains(&size), "{path}");
        }
    }
}
