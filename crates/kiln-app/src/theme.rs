//! Light and dark palettes in a macOS style, following the system appearance.
use gpui::{rgb, rgba, Rgba, WindowAppearance};

pub const MONO: &str = if cfg!(target_os = "macos") { "Menlo" } else { "DejaVu Sans Mono" };

/// The family for interface text, given the installed ones.
///
/// GPUI's default on Linux names a font that is often missing, and its fallback
/// has no bold or italic, so pick an installed family that does.
pub fn ui_font(installed: &[String]) -> &'static str {
    const SYSTEM: &str = ".SystemUIFont";
    if cfg!(target_os = "macos") {
        return SYSTEM;
    }
    ["Inter", "Adwaita Sans", "Cantarell", "Noto Sans", "Liberation Sans", "DejaVu Sans"]
        .into_iter()
        .find(|family| installed.iter().any(|name| name == family))
        .unwrap_or(SYSTEM)
}

#[derive(Clone, Copy)]
pub struct Theme {
    /// Interface font family.
    pub font: &'static str,
    pub window: Rgba,
    /// Slightly see-through on macOS so the blurred desktop shows behind it.
    pub sidebar: Rgba,
    /// Raised surfaces: composer and approval cards.
    pub card: Rgba,
    /// Quiet fill for tool calls, code and control tracks.
    pub fill: Rgba,
    pub border: Rgba,
    pub hover: Rgba,
    pub selected: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub on_accent: Rgba,
    pub ok: Rgba,
    pub warn: Rgba,
    /// The Kiln mark and lockup assets that read on this palette.
    pub logo: &'static str,
    pub lockup: &'static str,
}

impl Theme {
    pub fn new(appearance: WindowAppearance, font: &'static str) -> Self {
        let sidebar_alpha = if cfg!(target_os = "macos") { 0xd8 } else { 0xff };
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self {
                font,
                window: rgb(0xffffff),
                sidebar: rgba(0xf2f2f400 | sidebar_alpha),
                card: rgb(0xffffff),
                fill: rgb(0xf5f5f7),
                border: rgba(0x00000017),
                hover: rgba(0x0000000a),
                selected: rgba(0x00000014),
                text: rgb(0x1d1d1f),
                muted: rgb(0x6e6e73),
                accent: rgb(0x007aff),
                on_accent: rgb(0xffffff),
                ok: rgb(0x248a3d),
                warn: rgb(0xc25e00),
                logo: crate::assets::LOGO_LIGHT,
                lockup: crate::assets::LOCKUP_LIGHT,
            },
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self {
                font,
                window: rgb(0x1e1e1f),
                sidebar: rgba(0x28282a00 | sidebar_alpha),
                card: rgb(0x2c2c2e),
                fill: rgb(0x242426),
                border: rgba(0xffffff17),
                hover: rgba(0xffffff0d),
                selected: rgba(0xffffff1a),
                text: rgb(0xf5f5f7),
                muted: rgb(0x98989d),
                accent: rgb(0x0a84ff),
                on_accent: rgb(0xffffff),
                ok: rgb(0x30d158),
                warn: rgb(0xff9f0a),
                logo: crate::assets::LOGO_DARK,
                lockup: crate::assets::LOCKUP_DARK,
            },
        }
    }
}
