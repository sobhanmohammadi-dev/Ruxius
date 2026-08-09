//! A small, dependency-free terminal UI layer: colored status lines and a
//! spinner for steps that take a moment (packing, extracting). Plain ANSI
//! SGR codes rather than a crate, so there's no external API to verify —
//! and it degrades cleanly (plain text) wherever color isn't appropriate.
//!
//! Color is off by default until [`init`] is called, and `init` itself
//! turns it off again if:
//! - `NO_COLOR` is set (respects <https://no-color.org>), or
//! - stdout isn't actually a terminal (e.g. output is piped to a file), or
//! - Windows' console doesn't support ANSI and enabling it fails.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static COLOR_ENABLED: AtomicBool = AtomicBool::new(false);

/// Call once at startup, before any other `ui::` function.
pub fn init() {
    let wants_color = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    let enabled = wants_color && enable_windows_ansi();
    COLOR_ENABLED.store(enabled, Ordering::Relaxed);
}

#[cfg(windows)]
fn enable_windows_ansi() -> bool {
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };

    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle == 0 || handle == -1 {
            return false;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_windows_ansi() -> bool {
    true
}

fn color_enabled() -> bool {
    COLOR_ENABLED.load(Ordering::Relaxed)
}

fn paint(code: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn green(text: &str) -> String {
    paint("32", text)
}
pub fn red(text: &str) -> String {
    paint("31", text)
}
pub fn yellow(text: &str) -> String {
    paint("33", text)
}
pub fn cyan(text: &str) -> String {
    paint("36", text)
}
pub fn bold(text: &str) -> String {
    paint("1", text)
}
pub fn dim(text: &str) -> String {
    paint("2", text)
}

pub fn ok(msg: &str) {
    println!("{} {msg}", green("[ok]"));
}
pub fn warn(msg: &str) {
    println!("{} {msg}", yellow("[warn]"));
}
pub fn error(msg: &str) {
    println!("{} {msg}", red("[error]"));
}
pub fn info(msg: &str) {
    println!("{} {msg}", cyan("[info]"));
}

/// An animated spinner for a step that takes a moment, e.g. packing or
/// extracting. Runs on its own thread so it keeps spinning while the
/// caller does blocking work on the main thread; call [`Spinner::finish`]
/// with the result once that work completes.
///
/// Falls back to a single static line (no animation) when color is
/// disabled — e.g. output is piped to a file, where a `\r`-animated
/// spinner would just produce garbage.
pub struct Spinner {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    label: String,
    started: Instant,
}

impl Spinner {
    pub fn start(label: impl Into<String>) -> Self {
        let label = label.into();

        if !color_enabled() {
            println!("{label}...");
            return Spinner {
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
                label,
                started: Instant::now(),
            };
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let thread_label = label.clone();

        let thread = std::thread::spawn(move || {
            const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut i = 0usize;
            while !stop_clone.load(Ordering::Relaxed) {
                print!("\r{} {thread_label}", cyan(&FRAMES[i % FRAMES.len()].to_string()));
                let _ = std::io::stdout().flush();
                i += 1;
                std::thread::sleep(Duration::from_millis(80));
            }
        });

        Spinner {
            stop,
            thread: Some(thread),
            label,
            started: Instant::now(),
        }
    }

    /// Stops the spinner and prints a final result line with elapsed time.
    pub fn finish(mut self, success: bool, message: &str) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            let clear_width = self.label.chars().count() + 4;
            print!("\r{}\r", " ".repeat(clear_width));
        }

        let elapsed = self.started.elapsed();
        let mark = if success { green("✓") } else { red("✗") };
        println!("{mark} {message} {}", dim(&format!("({:.1}s)", elapsed.as_secs_f64())));
    }
}
