use crate::error::{LauncherError, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// Opens a native window hosting a WebView2 view pointed at `url` and runs
/// the platform event loop until the window is closed. `on_exit` is invoked
/// exactly once, right before the process would otherwise terminate, so the
/// caller can shut down the PHP server and release resources deterministically.
pub fn run(url: &str, title: &str, width: u32, height: u32, on_exit: impl FnOnce() + 'static) -> Result<()> {
    let event_loop = EventLoop::new();

    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(width, height))
        .with_min_inner_size(LogicalSize::new(640, 480))
        .with_resizable(true)
        .build(&event_loop)
        .map_err(|e| LauncherError::WebView(format!("failed to create window: {e}")))?;

    let _webview = WebViewBuilder::new(&window)
        .with_url(url)
        .with_initialization_script(
            "window.addEventListener('contextmenu', e => e.preventDefault());",
        )
        .build()
        .map_err(|e| LauncherError::WebView(format!("failed to initialize WebView2: {e}")))?;

    log::info!("WebView2 window opened at {url}");

    let mut on_exit = Some(on_exit);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            log::info!("Window close requested; shutting down.");
            if let Some(cb) = on_exit.take() {
                cb();
            }
            *control_flow = ControlFlow::Exit;
        }
    });
}
