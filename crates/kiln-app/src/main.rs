mod assets;
mod checkout_menu;
mod file_reference;
mod commands;
mod markdown;
mod media;
mod project_icon;
mod project_menu;
mod settings;
mod shell;
mod sidebar;
mod text_edit;
mod text_input;
mod theme;
mod tooltip;
mod transcript;
mod workspace;

use gpui::{
    point, px, size, App, AppContext, Bounds, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};

fn main() {
    gpui_platform::application().with_assets(assets::Assets).run(|cx: &mut App| {
        assets::load_icon_font(cx).expect("failed to load the icon font");
        text_input::bind_keys(cx);
        settings::bind_keys(cx);
        commands::bind_keys(cx);
        commands::set_menus(cx);
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // On macOS the content runs under a transparent title bar.
                titlebar: Some(TitlebarOptions {
                    title: Some("Kiln".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(19.), px(19.))),
                }),
                window_background: if cfg!(target_os = "macos") {
                    WindowBackgroundAppearance::Blurred
                } else {
                    WindowBackgroundAppearance::Opaque
                },
                // Lets Linux desktops match the window to an installed icon.
                app_id: Some("kiln".into()),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| workspace::Workspace::new(window, cx)),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
