mod theme;
mod workspace;

use gpui::{
    px, size, App, AppContext, Application, Bounds, WindowBounds, WindowOptions,
};

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| workspace::Workspace::demo()),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
