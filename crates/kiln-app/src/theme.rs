//! Light and dark palettes drawn from the Kiln mark: warm stone neutrals and an
//! ember accent.
use gpui::{rgb, rgba, Rgba, WindowAppearance};
use kiln_core::Appearance;

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
    pub dark: bool,
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
    pub fn new(system: WindowAppearance, preference: Appearance, font: &'static str) -> Self {
        let sidebar_alpha = if cfg!(target_os = "macos") { 0xd8 } else { 0xff };
        let appearance = match preference {
            Appearance::System => system,
            Appearance::Light => WindowAppearance::Light,
            Appearance::Dark => WindowAppearance::Dark,
        };
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self {
                font,
                dark: false,
                window: rgb(0xfdfcfb),
                sidebar: rgba(0xf3f1ee00 | sidebar_alpha),
                card: rgb(0xffffff),
                fill: rgb(0xf5f3f0),
                border: rgba(0x1c191717),
                hover: rgba(0x1c19170a),
                selected: rgba(0x1c191714),
                text: rgb(0x1c1917),
                muted: rgb(0x78716c),
                accent: rgb(0xc8430d),
                on_accent: rgb(0xffffff),
                ok: rgb(0x2f7d32),
                warn: rgb(0xa16207),
                logo: crate::assets::LOGO_LIGHT,
                lockup: crate::assets::LOCKUP_LIGHT,
            },
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self {
                font,
                dark: true,
                window: rgb(0x1c1917),
                sidebar: rgba(0x26221f00 | sidebar_alpha),
                card: rgb(0x2b2624),
                fill: rgb(0x231f1d),
                border: rgba(0xf5efe617),
                hover: rgba(0xf5efe60d),
                selected: rgba(0xf5efe61a),
                text: rgb(0xf5efe6),
                muted: rgb(0xa8a29e),
                accent: rgb(0xff7a3d),
                on_accent: rgb(0x1c1917),
                ok: rgb(0x4ade80),
                warn: rgb(0xfbbf24),
                logo: crate::assets::LOGO_DARK,
                lockup: crate::assets::LOCKUP_DARK,
            },
        }
    }
}
