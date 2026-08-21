use crate::error::{LauncherError, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// Delivered from the background thread that starts the PHP server back
/// to the event loop (the only place it's safe to touch the `WebView`
/// from) once it's known whether startup succeeded.
enum UserEvent {
    Ready(std::result::Result<String, String>),
}

/// Opens a native window immediately with a splash screen, starts the PHP
/// backend on a background thread via `start_backend`, and navigates the
/// window to the real app URL the moment it's ready — WebView2 on
/// Windows, WKWebView on macOS, WebKitGTK on Linux, all via the same
/// `wry`/`tao` APIs. `on_exit` is invoked exactly once, right before the
/// process would otherwise terminate, so the caller can shut down the PHP
/// server and release resources deterministically.
///
/// `start_backend` runs on its own thread and must never touch the
/// `WebView` directly — only the event loop (running on this thread) is
/// allowed to do that. It returns `Ok(url)` on success or `Err(message)`
/// on failure; either way the window updates to reflect it.
pub fn run_with_splash<F>(
    title: &str,
    width: u32,
    height: u32,
    start_backend: F,
    on_exit: impl FnOnce() + 'static,
) -> Result<()>
where
    F: FnOnce() -> std::result::Result<String, String> + Send + 'static,
{
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(width, height))
        .with_min_inner_size(LogicalSize::new(640, 480))
        .with_resizable(true)
        .build(&event_loop)
        .map_err(|e| LauncherError::WebView(format!("failed to create window: {e}")))?;

    let webview = WebViewBuilder::new(&window)
        .with_html(splash_html(title))
        .with_initialization_script(
            "window.addEventListener('contextmenu', e => e.preventDefault());",
        )
        .build()
        .map_err(|e| LauncherError::WebView(format!("failed to initialize the WebView: {e}")))?;

    log::info!("Splash shown, starting backend in the background");

    std::thread::spawn(move || {
        let result = start_backend();
        let _ = proxy.send_event(UserEvent::Ready(result));
    });

    let mut on_exit = Some(on_exit);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(UserEvent::Ready(Ok(url))) => {
                log::info!("Backend ready, navigating to {url}");
                webview.load_url(&url);
            }
            Event::UserEvent(UserEvent::Ready(Err(message))) => {
                log::error!("Backend failed to start: {message}");
                webview.load_url(&error_data_url(&message));
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                log::info!("Window close requested; shutting down.");
                if let Some(cb) = on_exit.take() {
                    cb();
                }
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}

/// A minimal, dependency-free splash page: dark background, the app's own
/// title, a CSS-only spinner (no JS needed for something this simple).
/// Shown the instant the window opens, before PHP has even started.
fn splash_html(title: &str) -> String {
    let escaped = html_escape(title);
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><style>
  html, body {{
    margin: 0; height: 100%; background: #14161a; color: #e8e8ea;
    display: flex; align-items: center; justify-content: center;
    font-family: -apple-system, "Segoe UI", sans-serif;
  }}
  .wrap {{ text-align: center; }}
  .spinner {{
    width: 36px; height: 36px; margin: 0 auto 16px;
    border: 3px solid #333640; border-top-color: #6fa8ff;
    border-radius: 50%; animation: spin 0.8s linear infinite;
  }}
  @keyframes spin {{ to {{ transform: rotate(360deg); }} }}
  .title {{ font-size: 15px; opacity: 0.85; }}
</style></head>
<body><div class="wrap">
  <div class="spinner"></div>
  <div class="title">Starting {escaped}...</div>
</div></body></html>"#
    )
}

/// A plain-text error page built as a `data:` URL, for when the backend
/// fails to start — `wry` has no post-construction `load_html`, only
/// `load_url`, so this is `load_url`'s way of showing arbitrary HTML.
fn error_data_url(message: &str) -> String {
    let html = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><style>
  html, body {{
    margin: 0; height: 100%; background: #1a1414; color: #f0d0d0;
    display: flex; align-items: center; justify-content: center;
    font-family: -apple-system, "Segoe UI", sans-serif;
  }}
  .wrap {{ max-width: 480px; text-align: center; padding: 24px; }}
  .heading {{ font-size: 15px; margin-bottom: 8px; color: #ff8a8a; }}
  .detail {{ font-size: 13px; opacity: 0.8; font-family: monospace; white-space: pre-wrap; }}
</style></head>
<body><div class="wrap">
  <div class="heading">Couldn't start the app</div>
  <div class="detail">{}</div>
</div></body></html>"#,
        html_escape(message)
    );
    format!("data:text/html,{}", percent_encode(&html))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Minimal RFC 3986 percent-encoder — just enough to make arbitrary HTML
/// safe inside a `data:` URL, without pulling in a dependency for it.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
