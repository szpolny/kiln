mod theme;
mod workspace;

use gpui::{
    point, px, size, App, AppContext, Application, Bounds, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};

fn main() {
    Application::new().run(|cx: &mut App| {
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
                ..Default::default()
            },
            |window, cx| cx.new(|cx| workspace::Workspace::new(window, cx)),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
