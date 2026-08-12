//! `rux tui` — an interactive terminal dashboard covering the same ground
//! the GUI used to: the PHP registry, `.pack` archiving, and the extension
//! manager, plus a live `rux doctor` view. Built on `ratatui` + `crossterm`
//! rather than a WebView, so it has no window/IPC machinery to reason
//! about — just a render loop and keyboard events.
//!
//! Every action here calls the exact same underlying functions the CLI
//! commands do (`php::resolve_external_php`, `ext::set_enabled`,
//! `pack::archive`, ...); this file is purely presentation and input
//! handling, not a second implementation of any of it.

use crate::config::AppConfig;
use crate::error::{LauncherError, Result};
use crate::{ext, pack, php};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Php,
    Ext,
    Doctor,
}

impl Tab {
    fn titles() -> [&'static str; 3] {
        ["PHP Versions", "Extensions", "Doctor"]
    }

    fn index(self) -> usize {
        match self {
            Tab::Php => 0,
            Tab::Ext => 1,
            Tab::Doctor => 2,
        }
    }

    fn next(self) -> Self {
        match self {
            Tab::Php => Tab::Ext,
            Tab::Ext => Tab::Doctor,
            Tab::Doctor => Tab::Php,
        }
    }
}

struct PhpRow {
    name: String,
    path: PathBuf,
    valid: bool,
    version: Option<String>,
    has_pack: bool,
}

struct ExtRow {
    name: String,
    enabled: bool,
    configured: bool,
}

struct DoctorInfo {
    webview2: Option<String>,
    php_ok: usize,
    php_missing: usize,
    cached_archives: usize,
    packs: usize,
}

enum InputMode {
    Normal,
    AddName(String),
    AddPath { name: String, buf: String },
}

/// Runs the TUI. Terminal setup/teardown happens here so a panic or error
/// partway through still restores the terminal via the `result` handling
/// below — raw mode and the alternate screen are process-wide state that
/// must never be left on if `rux tui` exits abnormally.
pub fn run() -> Result<()> {
    let data_dir = crate::data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| LauncherError::Other(anyhow::anyhow!("failed to enter alternate screen: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &data_dir);

    // Best-effort teardown regardless of how `run_app` returned — an error
    // shouldn't leave the user's terminal in raw mode / the alt screen.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    result
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, data_dir: &std::path::Path) -> Result<()> {
    let config = AppConfig::load(data_dir);
    let mut php_rows = load_php_rows(&config, data_dir);

    let mut php_state = ListState::default();
    php_state.select(if php_rows.is_empty() { None } else { Some(0) });

    let mut ext_php_index: Option<usize> = if php_rows.is_empty() { None } else { Some(0) };
    let mut ext_rows: Vec<ExtRow> = ext_php_index
        .and_then(|i| php_rows.get(i))
        .map(|row| load_ext_rows(&config, row))
        .unwrap_or_default();
    let mut ext_state = ListState::default();
    ext_state.select(if ext_rows.is_empty() { None } else { Some(0) });

    let mut doctor = load_doctor_info(data_dir, &config);

    let mut tab = Tab::Php;
    let mut input = InputMode::Normal;
    let mut status: Option<(String, bool)> = None;
    let mut archiving = false;
    let mut archive_started: Option<Instant> = None;

    let (bg_tx, bg_rx) = mpsc::channel::<std::result::Result<String, String>>();

    loop {
        if let Ok(msg) = bg_rx.try_recv() {
            archiving = false;
            archive_started = None;
            let config = AppConfig::load(data_dir);
            php_rows = load_php_rows(&config, data_dir);
            select_index(&mut php_state, php_rows.len());
            status = Some(match msg {
                Ok(summary) => (summary, false),
                Err(e) => (format!("Archiving failed: {e}"), true),
            });
        }

        let elapsed = archive_started.map(|s| s.elapsed());
        terminal.draw(|f| {
            draw(
                f, tab, &php_rows, &php_state, &ext_rows, &ext_state, ext_php_index, &doctor,
                &input, &status, archiving, elapsed,
            )
        })?;

        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Windows reports both press and release key events; without this
        // check every keystroke would be handled twice on Windows.
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match &mut input {
            InputMode::Normal => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Tab => {
                    tab = tab.next();
                    status = None;
                }
                KeyCode::Down => match tab {
                    Tab::Php => move_selection(&mut php_state, php_rows.len(), 1),
                    Tab::Ext => move_selection(&mut ext_state, ext_rows.len(), 1),
                    Tab::Doctor => {}
                },
                KeyCode::Up => match tab {
                    Tab::Php => move_selection(&mut php_state, php_rows.len(), -1),
                    Tab::Ext => move_selection(&mut ext_state, ext_rows.len(), -1),
                    Tab::Doctor => {}
                },
                KeyCode::Left if tab == Tab::Ext => {
                    switch_ext_php(-1, &php_rows, &mut ext_php_index, &mut ext_rows, &mut ext_state, data_dir);
                }
                KeyCode::Right if tab == Tab::Ext => {
                    switch_ext_php(1, &php_rows, &mut ext_php_index, &mut ext_rows, &mut ext_state, data_dir);
                }
                KeyCode::Char('a') if tab == Tab::Php => {
                    input = InputMode::AddName(String::new());
                }
                KeyCode::Char('d') if tab == Tab::Php => {
                    if let Some(row) = php_state.selected().and_then(|i| php_rows.get(i)) {
                        let mut cfg = AppConfig::load(data_dir);
                        cfg.php_versions.remove(&row.name);
                        match cfg.save(data_dir) {
                            Ok(()) => status = Some((format!("Removed '{}'.", row.name), false)),
                            Err(e) => status = Some((format!("{e}"), true)),
                        }
                        php_rows = load_php_rows(&cfg, data_dir);
                        select_index(&mut php_state, php_rows.len());
                    }
                }
                KeyCode::Char('x') if tab == Tab::Php && !archiving => {
                    if AppConfig::load(data_dir).php_versions.is_empty() {
                        status = Some(("No PHP versions registered yet — press 'a' to add one.".into(), true));
                    } else {
                        archiving = true;
                        archive_started = Some(Instant::now());
                        status = Some(("Archiving all registered PHP versions...".into(), false));
                        let tx = bg_tx.clone();
                        let data_dir_owned = data_dir.to_path_buf();
                        std::thread::spawn(move || {
                            let _ = tx.send(archive_all(&data_dir_owned));
                        });
                    }
                }
                KeyCode::Char('r') => match tab {
                    Tab::Doctor => {
                        doctor = load_doctor_info(data_dir, &AppConfig::load(data_dir));
                        status = Some(("Doctor refreshed.".into(), false));
                    }
                    Tab::Php => {
                        php_rows = load_php_rows(&AppConfig::load(data_dir), data_dir);
                        select_index(&mut php_state, php_rows.len());
                        status = Some(("Refreshed.".into(), false));
                    }
                    Tab::Ext => {
                        if let Some(row) = ext_php_index.and_then(|i| php_rows.get(i)) {
                            ext_rows = load_ext_rows(&AppConfig::load(data_dir), row);
                            select_index(&mut ext_state, ext_rows.len());
                        }
                    }
                },
                KeyCode::Enter if tab == Tab::Ext => {
                    if let (Some(prow), Some(erow)) = (
                        ext_php_index.and_then(|i| php_rows.get(i)),
                        ext_state.selected().and_then(|i| ext_rows.get(i)),
                    ) {
                        let config = AppConfig::load(data_dir);
                        match toggle_extension(&config, prow, erow) {
                            Ok(msg) => {
                                status = Some((msg, false));
                                ext_rows = load_ext_rows(&config, prow);
                                select_index(&mut ext_state, ext_rows.len());
                            }
                            Err(e) => status = Some((format!("{e}"), true)),
                        }
                    }
                }
                _ => {}
            },

            InputMode::AddName(buf) => match key.code {
                KeyCode::Enter => {
                    let name = buf.trim().to_string();
                    if !name.is_empty() {
                        input = InputMode::AddPath { name, buf: String::new() };
                    }
                }
                KeyCode::Esc => input = InputMode::Normal,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            },

            InputMode::AddPath { name, buf } => match key.code {
                KeyCode::Enter => {
                    let name = name.clone();
                    let path = PathBuf::from(buf.trim());
                    match php::resolve_external_php(&path) {
                        Ok(resolved) => {
                            let mut cfg = AppConfig::load(data_dir);
                            cfg.php_versions.insert(name.clone(), resolved.binary);
                            match cfg.save(data_dir) {
                                Ok(()) => status = Some((format!("Registered '{name}'."), false)),
                                Err(e) => status = Some((format!("{e}"), true)),
                            }
                            php_rows = load_php_rows(&cfg, data_dir);
                            select_index(&mut php_state, php_rows.len());
                        }
                        Err(e) => status = Some((format!("{e}"), true)),
                    }
                    input = InputMode::Normal;
                }
                KeyCode::Esc => input = InputMode::Normal,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            },
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------
// Selection helpers (deliberately not relying on ratatui's built-in
// ListState::select_next/previous, since those don't know the item count
// and can select an out-of-bounds index — which would panic the moment we
// index into our own Vec with it)
// ---------------------------------------------------------------------

fn move_selection(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0) as i32;
    let new = (current + delta).rem_euclid(len as i32) as usize;
    state.select(Some(new));
}

fn select_index(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }

    let current = state.selected().unwrap_or(0);
    let clamped = current.min(len - 1);

    state.select(Some(clamped));
}

fn select_index_or(state: &mut ListState, len: usize, want: usize) {
    if len == 0 {
        state.select(None);
        return;
    }

    state.select(Some(want.min(len - 1)));
}

fn switch_ext_php(
    delta: i32,
    php_rows: &[PhpRow],
    ext_php_index: &mut Option<usize>,
    ext_rows: &mut Vec<ExtRow>,
    ext_state: &mut ListState,
    data_dir: &std::path::Path,
) {
    if php_rows.is_empty() {
        return;
    }
    let current = ext_php_index.unwrap_or(0) as i32;
    let new = (current + delta).rem_euclid(php_rows.len() as i32) as usize;
    *ext_php_index = Some(new);
    let config = AppConfig::load(data_dir);
    *ext_rows = load_ext_rows(&config, &php_rows[new]);
    select_index_or(ext_state, ext_rows.len(), 0);
}

// ---------------------------------------------------------------------
// Data loading — thin wrappers around the same functions the CLI uses
// ---------------------------------------------------------------------

fn load_php_rows(config: &AppConfig, data_dir: &std::path::Path) -> Vec<PhpRow> {
    let packs_dir = AppConfig::packs_dir(data_dir);
    let packed: std::collections::HashSet<String> = pack::list_names(&packs_dir).into_iter().collect();

    let entries: Vec<(String, PathBuf)> = config
        .php_versions
        .iter()
        .map(|(n, p)| (n.clone(), p.clone()))
        .collect();

    crate::payload::parallel_map(&entries, |(name, path)| {
        let resolved = php::resolve_external_php(path).ok();
        let version = resolved.as_ref().and_then(|r| crate::php_version_string(&r.binary));
        PhpRow {
            name: name.clone(),
            path: path.clone(),
            valid: resolved.is_some(),
            version,
            has_pack: packed.contains(name),
        }
    })
}

fn load_ext_rows(config: &AppConfig, row: &PhpRow) -> Vec<ExtRow> {
    let Ok((php_ini, ext_dir, _)) = crate::resolve_php_ini(config, &row.name) else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    if let Ok(configured) = ext::list_configured(&php_ini) {
        for c in configured {
            rows.push(ExtRow { name: c.name, enabled: c.enabled, configured: true });
        }
    }
    if let Some(ext_dir) = &ext_dir {
        if let Ok(available) = ext::list_available_unconfigured(&php_ini, ext_dir) {
            for a in available {
                rows.push(ExtRow { name: a.name, enabled: false, configured: false });
            }
        }
    }
    rows
}

fn toggle_extension(config: &AppConfig, prow: &PhpRow, erow: &ExtRow) -> Result<String> {
    let (php_ini, ext_dir, _) = crate::resolve_php_ini(config, &prow.name)?;
    let enabling = !erow.enabled;
    let outcome = ext::set_enabled(&php_ini, ext_dir.as_deref(), &erow.name, enabling)?;
    Ok(match outcome {
        ext::ToggleOutcome::Changed => format!("'{}' is now {}.", erow.name, if enabling { "enabled" } else { "disabled" }),
        ext::ToggleOutcome::AddedNewLine => format!("'{}' enabled (new line added to php.ini).", erow.name),
        ext::ToggleOutcome::AlreadyInThatState => format!("'{}' was already {}.", erow.name, if enabling { "enabled" } else { "disabled" }),
        ext::ToggleOutcome::NotAvailable => {
            return Err(LauncherError::Extraction(format!(
                "'{}' isn't configured and no matching DLL was found in ext/.",
                erow.name
            )));
        }
    })
}

fn load_doctor_info(data_dir: &std::path::Path, config: &AppConfig) -> DoctorInfo {
    let mut php_ok = 0;
    let mut php_missing = 0;
    for path in config.php_versions.values() {
        if php::resolve_external_php(path).is_ok() {
            php_ok += 1;
        } else {
            php_missing += 1;
        }
    }

    let cache_dir = data_dir.join("cache").join("archives");
    let cached_archives = std::fs::read_dir(&cache_dir).map(|d| d.flatten().count()).unwrap_or(0);

    let packs_dir = AppConfig::packs_dir(data_dir);
    let packs = pack::list_names(&packs_dir).len();

    DoctorInfo {
        webview2: crate::find_webview2_runtime(),
        php_ok,
        php_missing,
        cached_archives,
        packs,
    }
}

fn archive_all(data_dir: &std::path::Path) -> std::result::Result<String, String> {
    let config = AppConfig::load(data_dir);
    let packs_dir = AppConfig::packs_dir(data_dir);
    let mut ok = 0;
    let mut failed = 0;

    for (name, path) in &config.php_versions {
        let Ok(resolved) = php::resolve_external_php(path) else {
            failed += 1;
            continue;
        };
        let php_dir = resolved.binary.parent().unwrap_or(path);
        match pack::archive(name, php_dir, &packs_dir) {
            Ok(_) => ok += 1,
            Err(_) => failed += 1,
        }
    }

    if failed == 0 {
        Ok(format!("Archived {ok} PHP version(s)."))
    } else {
        Ok(format!("Archived {ok} PHP version(s), {failed} failed (check `rux php list`)."))
    }
}

// ---------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn draw(
    f: &mut ratatui::Frame<'_>,
    tab: Tab,
    php_rows: &[PhpRow],
    php_state: &ListState,
    ext_rows: &[ExtRow],
    ext_state: &ListState,
    ext_php_index: Option<usize>,
    doctor: &DoctorInfo,
    input: &InputMode,
    status: &Option<(String, bool)>,
    archiving: bool,
    archive_elapsed: Option<Duration>,
) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    let tabs = Tabs::new(Tab::titles().iter().map(|t| Line::from(*t)).collect::<Vec<_>>())
        .block(Block::default().borders(Borders::ALL).title(" Ruxius "))
        .select(tab.index())
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(tabs, chunks[0]);

    match tab {
        Tab::Php => draw_php_tab(f, chunks[1], php_rows, php_state, archiving, archive_elapsed),
        Tab::Ext => draw_ext_tab(f, chunks[1], php_rows, ext_rows, ext_state, ext_php_index),
        Tab::Doctor => draw_doctor_tab(f, chunks[1], doctor),
    }

    draw_footer(f, chunks[2], tab, input, status);
}

fn draw_php_tab(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    rows: &[PhpRow],
    state: &ListState,
    archiving: bool,
    elapsed: Option<Duration>,
) {
    if rows.is_empty() {
        let msg = Paragraph::new("No PHP versions registered. Press 'a' to add one.")
            .block(Block::default().borders(Borders::ALL).title(" PHP Versions "));
        f.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| {
            let status_span = if r.valid {
                Span::styled("ok", Style::default().fg(Color::Green))
            } else {
                Span::styled("missing", Style::default().fg(Color::Red))
            };
            let pack_span = if r.has_pack {
                Span::styled(" [packed]", Style::default().fg(Color::Magenta))
            } else {
                Span::raw("")
            };
            let version = r.version.clone().unwrap_or_else(|| "-".to_string());
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<14}", r.name), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{version:<24} ")),
                status_span,
                pack_span,
                Span::raw(format!("  {}", r.path.display())),
            ]))
        })
        .collect();

    let title = if archiving {
        format!(" PHP Versions — archiving... ({:.0}s) ", elapsed.unwrap_or_default().as_secs_f64())
    } else {
        " PHP Versions ".to_string()
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    let mut state = state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_ext_tab(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    php_rows: &[PhpRow],
    ext_rows: &[ExtRow],
    ext_state: &ListState,
    ext_php_index: Option<usize>,
) {
    let selected_name = ext_php_index
        .and_then(|i| php_rows.get(i))
        .map(|r| r.name.as_str())
        .unwrap_or("(none)");

    if php_rows.is_empty() {
        let msg = Paragraph::new("No PHP versions registered yet — add one from the PHP Versions tab.")
            .block(Block::default().borders(Borders::ALL).title(" Extensions "));
        f.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = ext_rows
        .iter()
        .map(|e| {
            let (label, color) = if !e.configured {
                ("available", Color::DarkGray)
            } else if e.enabled {
                ("enabled", Color::Green)
            } else {
                ("disabled", Color::Yellow)
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<24}", e.name)),
                Span::styled(label, Style::default().fg(color)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Extensions — {selected_name} (\u{2190}/\u{2192} to switch PHP) ")),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    let mut state = ext_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_doctor_tab(f: &mut ratatui::Frame<'_>, area: Rect, doctor: &DoctorInfo) {
    let mut lines = Vec::new();

    match &doctor.webview2 {
        Some(v) => lines.push(Line::from(vec![
            Span::styled("[ok]   ", Style::default().fg(Color::Green)),
            Span::raw(format!("WebView2 Runtime found (version {v})")),
        ])),
        None => lines.push(Line::from(vec![
            Span::styled("[fail] ", Style::default().fg(Color::Red)),
            Span::raw("WebView2 Runtime not found — built apps need it to open their window."),
        ])),
    }

    let php_style = if doctor.php_missing == 0 { Color::Green } else { Color::Yellow };
    lines.push(Line::from(vec![
        Span::styled(if doctor.php_missing == 0 { "[ok]   " } else { "[warn] " }, Style::default().fg(php_style)),
        Span::raw(format!("{} registered PHP version(s) valid, {} missing", doctor.php_ok, doctor.php_missing)),
    ]));

    lines.push(Line::from(format!("[info] {} cached build archive(s)", doctor.cached_archives)));
    lines.push(Line::from(format!("[info] {} .pack file(s)", doctor.packs)));

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Doctor "));
    f.render_widget(p, area);
}

fn draw_footer(f: &mut ratatui::Frame<'_>, area: Rect, tab: Tab, input: &InputMode, status: &Option<(String, bool)>) {
    let line = match input {
        InputMode::AddName(buf) => Line::from(vec![
            Span::styled("Name: ", Style::default().fg(Color::Cyan)),
            Span::raw(buf.clone()),
            Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
        ]),
        InputMode::AddPath { name, buf } => Line::from(vec![
            Span::styled(format!("Path for '{name}': "), Style::default().fg(Color::Cyan)),
            Span::raw(buf.clone()),
            Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
        ]),
        InputMode::Normal => match status {
            Some((msg, is_error)) => Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(if *is_error { Color::Red } else { Color::Green }),
            )),
            None => Line::from(Span::styled(help_text(tab), Style::default().fg(Color::DarkGray))),
        },
    };

    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn help_text(tab: Tab) -> &'static str {
    match tab {
        Tab::Php => "Tab: switch view  ↑/↓: select  a: add  d: remove  x: archive all  r: refresh  q: quit",
        Tab::Ext => "Tab: switch view  ↑/↓: select ext  ←/→: switch PHP  Enter: toggle  q: quit",
        Tab::Doctor => "Tab: switch view  r: refresh  q: quit",
    }
}
