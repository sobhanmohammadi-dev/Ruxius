// This binary always builds with the default console subsystem so that
// `rux php add/list/remove`, `rux build`, and `--help` reliably print their
// output in a terminal (a GUI-subsystem exe's stdout/stderr does not
// reliably show up when run from PowerShell/cmd). When we're actually
// about to launch a bundled app's WebView window, `hide_console_window()`
// below hides the console at runtime instead, so double-clicking a built
// .exe doesn't leave a console window behind.

mod cli;
mod config;
mod error;
mod ext;
mod extract;
mod framework;
mod icon;
mod logger;
mod pack;
mod payload;
mod php;
mod tui;
mod ui;
mod version;
mod webview;

use clap::{CommandFactory, Parser};
use cli::{Cli, Command, ConfigAction, PhpAction};
use config::AppConfig;
use error::{LauncherError, Result};
use fs2::FileExt;
use php::PhpServer;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP_QUALIFIER: &str = "com";
const APP_ORGANIZATION: &str = "Ruxius";
const APP_NAME: &str = "Ruxius";

fn main() -> ExitCode {
    ui::init();
    let cli = Cli::parse();

    let result = match cli.command {
        None => run_bare(),
        Some(Command::Build {
            app_path,
            php,
            output_path,
            title,
            width,
            height,
            force,
            watch,
            icon,
            php_ini,
        }) => run_build_command(
            &app_path, &php, &output_path, title, width, height, force, watch, icon, php_ini,
        ),
        Some(Command::Php { action }) => run_php_command(action),
        Some(Command::Doctor) => run_doctor_command(),
        Some(Command::Init) => run_init_command(),
        Some(Command::Tui) => tui::run(),
        Some(Command::Logs { lines }) => run_logs_command(lines),
        Some(Command::Config { action }) => run_config_command(action),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("Fatal error: {e:#}");
            eprintln!("{} {e:#}", ui::red("rux:"));
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------
// bare `rux` — launch the bundled app if this is a built exe, otherwise
// show the normal clap help (this is the builder)
// ---------------------------------------------------------------------

fn run_bare() -> Result<()> {
    let current_exe = std::env::current_exe().map_err(LauncherError::Io)?;

    if payload::detect(&current_exe)?.is_some() {
        // A built app: this is what happens when someone double-clicks the
        // .exe produced by `rux build`. No commands involved.
        return run_app(&current_exe);
    }

    // The bare builder tool with nothing bundled: behave like a normal CLI
    // and show the standard clap help instead of trying to open a window.
    Cli::command().print_help().ok();
    println!();
    Ok(())
}

fn run_app(current_exe: &Path) -> Result<()> {
    let payload_info = payload::detect(current_exe)?.ok_or_else(|| {
        LauncherError::Extraction("no app bundled in this executable".into())
    })?;

    // From here on we're opening a native window, not printing CLI output,
    // so hide the console window a double-click would otherwise flash open.
    hide_console_window();

    let data_dir = data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    logger::init(&data_dir).map_err(LauncherError::Other)?;
    install_panic_hook();

    log::info!(
        "Ruxius {} starting bundled app (pid {})",
        version::LAUNCHER_VERSION,
        std::process::id()
    );

    let _instance_lock = acquire_single_instance_lock(&data_dir)?;

    let checksum_hex = payload_info.checksum_hex();
    let extract_root = data_dir.join("apps").join(&checksum_hex);
    let compressed_payload = payload::read_payload_bytes(current_exe, &payload_info)?;
    let (bundle_dir, meta) =
        extract::ensure_extracted(&extract_root, &compressed_payload, &checksum_hex)?;

    let log_path = logs_dir(&data_dir).join(format!("php-{checksum_hex}.log"));

    // Shared so both the Ctrl+C handler and the window's close callback can
    // guarantee the PHP process is terminated exactly once, from whichever
    // path triggers first. Starts empty — PHP hasn't launched yet at this
    // point, since the window (with its splash screen) opens first and
    // the backend starts on its own thread from inside `webview::run_with_splash`.
    let php_server: Arc<Mutex<Option<PhpServer>>> = Arc::new(Mutex::new(None));
    install_ctrlc_handler(Arc::clone(&php_server));

    let webview_server_handle = Arc::clone(&php_server);
    let shutdown = move || {
        log::info!("Performing graceful shutdown.");
        if let Ok(mut guard) = webview_server_handle.lock() {
            if let Some(mut server) = guard.take() {
                server.shutdown();
            }
        }
        log::info!("Shutdown complete.");
    };

    let backend_server_handle = Arc::clone(&php_server);
    let router = meta.router.clone();
    let ini_overrides = meta.php_ini_overrides.clone();
    let start_backend = move || -> std::result::Result<String, String> {
        let server = PhpServer::start(&bundle_dir, router.as_deref(), Some(&log_path), &ini_overrides)
            .map_err(|e| e.to_string())?;
        let url = server.url();
        log::info!("PHP backend ready at {url}");
        *backend_server_handle.lock().unwrap() = Some(server);
        Ok(url)
    };

    webview::run_with_splash(&meta.title, meta.width, meta.height, start_backend, shutdown)?;

    // Unreachable in practice: tao's event loop takes over the process and
    // exits it directly. Kept for completeness / non-Windows testing.
    Ok(())
}

// ---------------------------------------------------------------------
// `rux build <app-path> <php> <output-path>` — package, don't compile
// ---------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn run_build_command(
    app_path: &Path,
    php: &str,
    output_path: &Path,
    title: Option<String>,
    width: u32,
    height: u32,
    force: bool,
    watch: bool,
    icon: Option<PathBuf>,
    php_ini: Vec<String>,
) -> Result<()> {
    build_once(
        app_path,
        php,
        output_path,
        title.clone(),
        width,
        height,
        force,
        icon.as_deref(),
        &php_ini,
    )?;

    if !watch {
        return Ok(());
    }

    println!();
    println!("{}", ui::bold("Watching for changes"));
    println!("{}", ui::dim(&format!("({} — Ctrl+C to stop)", app_path.display())));

    let mut last_fingerprint = fingerprint_app_dir(app_path);

    loop {
        std::thread::sleep(Duration::from_millis(750));
        let current = fingerprint_app_dir(app_path);
        if current != last_fingerprint {
            println!();
            ui::info("Change detected, rebuilding...");
            // A build failure while watching shouldn't kill the watch loop
            // — print it and keep going, the same way a typo in a file you
            // haven't saved yet wouldn't stop your editor from watching it.
            if let Err(e) = build_once(
                app_path,
                php,
                output_path,
                title.clone(),
                width,
                height,
                true,
                icon.as_deref(),
                &php_ini,
            ) {
                ui::error(&format!("{e:#}"));
            }
            last_fingerprint = current;
        }
    }
}

/// A cheap signature of `app_path`'s contents, used only to detect "did
/// anything change" for `--watch` — same idea as the build cache's
/// fingerprint, reused directly rather than reimplemented.
fn fingerprint_app_dir(app_path: &Path) -> String {
    let entries = payload::walk_sorted(app_path);
    payload::fingerprint_entries(&entries, &[])
}

#[allow(clippy::too_many_arguments)]
fn build_once(
    app_path: &Path,
    php: &str,
    output_path: &Path,
    title: Option<String>,
    width: u32,
    height: u32,
    force: bool,
    icon: Option<&Path>,
    php_ini: &[String],
) -> Result<()> {
    if !app_path.is_dir() {
        return Err(LauncherError::Extraction(format!(
            "app path '{}' is not a directory",
            app_path.display()
        )));
    }

    for entry in php_ini {
        let Some((key, _)) = entry.split_once('=') else {
            return Err(LauncherError::Extraction(format!(
                "--php-ini '{entry}' isn't in key=value form"
            )));
        };
        if key.trim().is_empty() {
            return Err(LauncherError::Extraction(format!(
                "--php-ini '{entry}' has an empty key"
            )));
        }
    }
    if let Some(icon_path) = icon {
        if !icon_path.is_file() {
            return Err(LauncherError::Extraction(format!(
                "--icon '{}' doesn't exist",
                icon_path.display()
            )));
        }
    }

    let data_dir = data_dir()?;
    let config = AppConfig::load(&data_dir);
    let php_path = config.resolve_php_reference(php);

    let resolved = php::resolve_external_php(&php_path).map_err(|e| {
        LauncherError::PhpStart(format!(
            "couldn't resolve PHP '{php}' (looked at '{}'): {e}",
            php_path.display()
        ))
    })?;
    let php_dir = resolved
        .binary
        .parent()
        .ok_or_else(|| LauncherError::PhpStart("resolved PHP binary has no parent dir".into()))?;

    // If `php` names a registered version and it's been archived with
    // `rux php archive`, use that .pack directly and skip walking/hashing/
    // compressing the PHP directory entirely — the fast path this whole
    // feature exists for. A corrupt pack is a hard error rather than a
    // silent fallback, since silently ignoring it could mask real
    // tampering or disk corruption.
    let packs_dir = AppConfig::packs_dir(&data_dir);
    let php_source = if config.php_versions.contains_key(php) {
        match pack::read(php, &packs_dir)? {
            Some(bytes) => {
                ui::info(&format!("Using archived pack for '{php}' (skipping directory scan)"));
                payload::PhpArchiveSource::Prebuilt(bytes)
            }
            None => payload::PhpArchiveSource::Directory(php_dir),
        }
    } else {
        payload::PhpArchiveSource::Directory(php_dir)
    };

    // Detect Laravel/Symfony/other public/-rooted frameworks: PHP's
    // built-in server has no rewrite rules of its own, so a framework app
    // needs both the right docroot (public/, not the project root) and a
    // router script to fall back to the front controller for anything
    // that isn't a real static file — otherwise only the homepage works.
    let detection = framework::detect(app_path);
    let app_extra_files: Vec<(String, Vec<u8>)> = if detection.needs_router {
        vec![framework::router_file()]
    } else {
        Vec::new()
    };
    for warning in framework::compatibility_warnings(app_path, &detection) {
        ui::warn(&warning);
    }

    // Safety check: if something already exists at the output path and it
    // doesn't look like a Ruxius build (i.e. it's some unrelated file), bail
    // out instead of silently overwriting it. A previous Ruxius build is
    // fine to overwrite freely — that's the normal rebuild path.
    if output_path.exists() && !force {
        let is_previous_build = payload::detect(output_path).ok().flatten().is_some();
        if !is_previous_build {
            return Err(LauncherError::Extraction(format!(
                "'{}' already exists and doesn't look like a Ruxius build — refusing to \
                 overwrite it. Pass --force if you're sure, or pick a different output path.",
                output_path.display()
            )));
        }
    }

    let title = title.unwrap_or_else(|| {
        app_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Ruxius App".to_string())
    });
    let router = detection.needs_router.then(|| framework::ROUTER_FILENAME.to_string());
    let meta = payload::BuildMeta::new(title, width, height, router, php_ini.to_vec());

    println!("{}", ui::bold("Packaging app"));
    println!("  app:       {}", app_path.display());
    if detection.kind != framework::Kind::Plain {
        println!("  framework: {} (serving from {})", detection.kind.label(), detection.docroot.display());
    }
    println!("  php:       {} ({})", php, resolved.binary.display());
    if let Some(ext_dir) = &resolved.extension_dir {
        println!("  ext:       {}", ext_dir.display());
    } else {
        println!(
            "  ext:       {}",
            ui::yellow("(none found — extensions will use PHP's own defaults)")
        );
    }
    println!(
        "  window:    \"{}\" {}x{}",
        meta.title, meta.width, meta.height
    );
    if let Some(icon_path) = icon {
        println!("  icon:      {}", icon_path.display());
    }
    if !meta.php_ini_overrides.is_empty() {
        println!("  php.ini:   {}", meta.php_ini_overrides.join(", "));
    }
    println!("  output:    {}", output_path.display());
    println!();

    let cache_dir = data_dir.join("cache").join("archives");
    let spinner = ui::Spinner::start("Packing PHP + app archives");
    let packed = match payload::pack(php_source, &detection.docroot, &app_extra_files, &meta, &cache_dir) {
        Ok(packed) => packed,
        Err(e) => {
            spinner.finish(false, "Packing failed");
            return Err(e);
        }
    };
    spinner.finish(
        true,
        &format!(
            "Packed {:.1} MiB",
            packed.len() as f64 / (1024.0 * 1024.0)
        ),
    );

    if !force && payload::matches_existing_output(output_path, &packed) {
        ui::info(&format!(
            "{} is already up to date; nothing to rebuild. (use --force to rebuild anyway)",
            output_path.display()
        ));
        return Ok(());
    }

    let current_exe = std::env::current_exe().map_err(LauncherError::Io)?;

    // Icon embedding, when requested, happens on a temp copy of the stub
    // *before* the payload is appended — never directly on current_exe or
    // output_path — so the self-appending format's footer/checksum logic
    // (already trusted) is always the last thing to touch the final file.
    let icon_temp_holder: Option<tempfile::NamedTempFile> = if let Some(icon_path) = icon {
        let icon_spinner = ui::Spinner::start("Embedding icon");
        let temp = match tempfile::Builder::new().suffix(".exe").tempfile() {
            Ok(t) => t,
            Err(e) => {
                icon_spinner.finish(false, "Embedding icon failed");
                return Err(LauncherError::Io(e));
            }
        };
        if let Err(e) = icon::embed_icon(&current_exe, icon_path, temp.path()) {
            icon_spinner.finish(false, "Embedding icon failed");
            return Err(e);
        }
        icon_spinner.finish(true, "Icon embedded");
        Some(temp)
    } else {
        None
    };
    let base_exe: &Path = icon_temp_holder
        .as_ref()
        .map(|t| t.path())
        .unwrap_or(&current_exe);

    let write_spinner = ui::Spinner::start("Writing executable");
    match payload::build_output(base_exe, &packed, output_path) {
        Ok(()) => write_spinner.finish(true, &format!("Built {}", output_path.display())),
        Err(e) => {
            write_spinner.finish(false, "Writing executable failed");
            return Err(e);
        }
    }

    Ok(())
}

/// The same packaging pipeline as `build_once` — PHP resolution (including
/// `.pack` reuse), framework detection, the overwrite-safety check, then
/// pack + write — but with no `println!`/spinner output whatsoever.
/// `rux tui` calls this from a background thread while its alternate
/// screen is active; writing raw text to stdout from there would corrupt
/// the TUI's display, so this returns a plain summary string instead of
/// printing anything, and the TUI renders its own progress UI around the
/// call instead of this function's.
///
/// Unlike `build_once`, this always overwrites a previous Ruxius build —
/// no `--force` equivalent is exposed in the TUI form, since triggering a
/// build in the TUI is already an explicit "yes, do this" action.
fn build_quiet(
    app_path: &Path,
    php: &str,
    output_path: &Path,
    title: Option<String>,
    width: u32,
    height: u32,
) -> Result<String> {
    if !app_path.is_dir() {
        return Err(LauncherError::Extraction(format!(
            "app path '{}' is not a directory",
            app_path.display()
        )));
    }

    let data_dir = data_dir()?;
    let config = AppConfig::load(&data_dir);
    let php_path = config.resolve_php_reference(php);

    let resolved = php::resolve_external_php(&php_path).map_err(|e| {
        LauncherError::PhpStart(format!(
            "couldn't resolve PHP '{php}' (looked at '{}'): {e}",
            php_path.display()
        ))
    })?;
    let php_dir = resolved
        .binary
        .parent()
        .ok_or_else(|| LauncherError::PhpStart("resolved PHP binary has no parent dir".into()))?;

    let packs_dir = AppConfig::packs_dir(&data_dir);
    let php_source = if config.php_versions.contains_key(php) {
        match pack::read(php, &packs_dir)? {
            Some(bytes) => payload::PhpArchiveSource::Prebuilt(bytes),
            None => payload::PhpArchiveSource::Directory(php_dir),
        }
    } else {
        payload::PhpArchiveSource::Directory(php_dir)
    };

    let detection = framework::detect(app_path);
    let app_extra_files: Vec<(String, Vec<u8>)> = if detection.needs_router {
        vec![framework::router_file()]
    } else {
        Vec::new()
    };

    if output_path.exists() {
        let is_previous_build = payload::detect(output_path).ok().flatten().is_some();
        if !is_previous_build {
            return Err(LauncherError::Extraction(format!(
                "'{}' already exists and doesn't look like a Ruxius build",
                output_path.display()
            )));
        }
    }

    let title = title.unwrap_or_else(|| {
        app_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Ruxius App".to_string())
    });
    let router = detection.needs_router.then(|| framework::ROUTER_FILENAME.to_string());
    let meta = payload::BuildMeta::new(title, width, height, router, Vec::new());

    let cache_dir = data_dir.join("cache").join("archives");
    let packed = payload::pack(php_source, &detection.docroot, &app_extra_files, &meta, &cache_dir)?;

    let current_exe = std::env::current_exe().map_err(LauncherError::Io)?;
    payload::build_output(&current_exe, &packed, output_path)?;

    Ok(format!(
        "Built {} ({:.1} MiB){}",
        output_path.display(),
        packed.len() as f64 / (1024.0 * 1024.0),
        if detection.kind != framework::Kind::Plain {
            format!(" — {}", detection.kind.label())
        } else {
            String::new()
        }
    ))
}

// ---------------------------------------------------------------------
// `rux php add|remove|list`
// ---------------------------------------------------------------------

fn run_php_command(action: PhpAction) -> Result<()> {
    let data_dir = data_dir()?;
    std::fs::create_dir_all(&data_dir)?;
    let mut config = AppConfig::load(&data_dir);

    match action {
        PhpAction::Add { name, path } => {
            let resolved = php::resolve_external_php(&path).map_err(|e| {
                LauncherError::PhpStart(format!(
                    "couldn't use '{}' as a PHP install: {e}",
                    path.display()
                ))
            })?;
            config
                .php_versions
                .insert(name.clone(), resolved.binary.clone());
            config.save(&data_dir).map_err(LauncherError::Other)?;
            println!("{} '{name}' -> {}", ui::green("Registered"), resolved.binary.display());
        }

        PhpAction::Remove { name } => {
            if config.php_versions.remove(&name).is_some() {
                config.save(&data_dir).map_err(LauncherError::Other)?;
                println!("{} '{name}'.", ui::green("Removed"));
            } else {
                ui::warn(&format!("No PHP version named '{name}' is registered."));
            }
        }

        PhpAction::ClearCache => {
            let cache_dir = data_dir.join("cache").join("archives");
            match std::fs::read_dir(&cache_dir) {
                Ok(entries) => {
                    let mut count = 0u64;
                    let mut bytes = 0u64;
                    for entry in entries.flatten() {
                        if let Ok(meta) = entry.metadata() {
                            bytes += meta.len();
                        }
                        if std::fs::remove_file(entry.path()).is_ok() {
                            count += 1;
                        }
                    }
                    println!(
                        "Cleared {count} cached archive(s) ({:.1} MiB).",
                        bytes as f64 / (1024.0 * 1024.0)
                    );
                }
                Err(_) => println!("No cached PHP archives to clear."),
            }
        }

        PhpAction::Archive => {
            if config.php_versions.is_empty() {
                println!("No PHP versions registered yet. Add one with:");
                println!("  rux php add <name> \"<path to php.exe>\"");
                return Ok(());
            }

            let packs_dir = AppConfig::packs_dir(&data_dir);
            // Each archive job already saturates all CPU cores internally
            // (parallel file reads + multithreaded zstd compression), so
            // archiving is done one PHP version at a time rather than
            // launching them all concurrently — running several such
            // CPU-bound jobs at once wouldn't be any faster, just more
            // contended, and this way progress per version is easy to follow.
            for (name, path) in &config.php_versions {
                let resolved = match php::resolve_external_php(path) {
                    Ok(r) => r,
                    Err(e) => {
                        ui::warn(&format!("Skipping '{name}': {e}"));
                        continue;
                    }
                };
                let php_dir = resolved.binary.parent().unwrap_or(path);
                let spinner = ui::Spinner::start(format!("Archiving '{name}'"));
                match pack::archive(name, php_dir, &packs_dir) {
                    Ok(pack_path) => {
                        let size = std::fs::metadata(&pack_path).map(|m| m.len()).unwrap_or(0);
                        spinner.finish(
                            true,
                            &format!(
                                "'{name}' -> {} ({:.1} MiB)",
                                pack_path.display(),
                                size as f64 / (1024.0 * 1024.0)
                            ),
                        );
                    }
                    Err(e) => spinner.finish(false, &format!("'{name}' failed: {e}")),
                }
            }
        }

        PhpAction::Ext { action } => run_ext_command(action, &config)?,

        PhpAction::List => {
            if config.php_versions.is_empty() {
                println!("No PHP versions registered yet. Add one with:");
                println!("  rux php add <name> \"<path to php.exe>\"");
            } else {
                println!("{}", ui::bold("Registered PHP versions:"));
                let entries: Vec<(String, PathBuf)> = config
                    .php_versions
                    .iter()
                    .map(|(name, path)| (name.clone(), path.clone()))
                    .collect();

                // Each entry means resolving the binary and (if valid)
                // spawning `php -v` — independent work, so run it all
                // concurrently instead of waiting on each process in turn.
                let results = payload::parallel_map(&entries, |(name, path)| {
                    let resolved = php::resolve_external_php(path).ok();
                    let version = resolved
                        .as_ref()
                        .and_then(|r| php_version_string(&r.binary));
                    (name.clone(), path.clone(), resolved.is_some(), version)
                });

                // Colored markers are printed as their own field, outside
                // the width-padded columns — mixing ANSI escape codes into
                // a `{:<width}` field would throw off the padding, since
                // the invisible escape bytes count toward the width too.
                for (name, path, valid, version) in results {
                    if valid {
                        let version = version.unwrap_or_else(|| "version unknown".to_string());
                        println!(
                            "  {} {name:<12} {version:<28} {}",
                            ui::green("✓"),
                            path.display()
                        );
                    } else {
                        println!(
                            "  {} {name:<12} {:<28} {}  {}",
                            ui::red("✗"),
                            "",
                            path.display(),
                            ui::red("[MISSING]")
                        );
                    }
                }
            }

            let discovered = discover_php_installations();
            if !discovered.is_empty() {
                println!("\nOther PHP installs found on this system:");

                let results = payload::parallel_map(&discovered, |path| {
                    (path.clone(), php_version_string(path))
                });

                for (path, version) in results {
                    let version = version.unwrap_or_else(|| "version unknown".to_string());
                    println!("  {version:<28} {}", path.display());
                }
                println!("\nRegister one with: rux php add <name> \"<path>\"");
            }
        }
    }

    Ok(())
}

/// Runs `php -v` and extracts just the version line (e.g. "PHP 8.3.6
/// (cli) ..."), for display in `rux php list`. Returns `None` if the
/// binary can't be run or produces no output.
fn php_version_string(binary: &Path) -> Option<String> {
    let output = std::process::Command::new(binary).arg("-v").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().next().map(|line| line.trim().to_string())
}

// ---------------------------------------------------------------------
// `rux logs`
// ---------------------------------------------------------------------

fn run_logs_command(lines: usize) -> Result<()> {
    let data_dir = data_dir()?;
    let dir = logs_dir(&data_dir);

    let Some(log_path) = latest_log_file(&dir) else {
        println!("No PHP logs yet — run a built app at least once, then try again.");
        return Ok(());
    };

    println!("{}", ui::bold(&format!("Tailing {}", log_path.display())));
    println!("{}", ui::dim("(Ctrl+C to stop)"));
    println!();

    for line in tail_lines(&log_path, lines).unwrap_or_default() {
        println!("{line}");
    }

    let mut file = File::open(&log_path)?;
    let mut size = file.metadata()?.len();
    file.seek(SeekFrom::Start(size))?;

    loop {
        std::thread::sleep(Duration::from_millis(500));
        let Ok(meta) = std::fs::metadata(&log_path) else {
            continue;
        };
        let new_size = meta.len();
        if new_size < size {
            // Log was rotated or cleared out from under us — read from the
            // top of whatever's there now instead of erroring out.
            size = 0;
            file.seek(SeekFrom::Start(0))?;
        }
        if new_size > size {
            let mut buf = String::new();
            file.read_to_string(&mut buf)?;
            print!("{buf}");
            let _ = std::io::stdout().flush();
            size = new_size;
        }
    }
}

/// The most recently modified `php-*.log` file under `dir` — "whichever
/// built app was run most recently", without needing to know which one
/// that was.
fn latest_log_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("php-") && name.ends_with(".log")
        })
        .max_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()))
        .map(|e| e.path())
}

fn tail_lines(path: &Path, n: usize) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(path).ok()?;
    let lines: Vec<String> = content.lines().map(str::to_string).collect();
    let start = lines.len().saturating_sub(n);
    Some(lines[start..].to_vec())
}

// ---------------------------------------------------------------------
// `rux config export|import`
// ---------------------------------------------------------------------

fn run_config_command(action: ConfigAction) -> Result<()> {
    let data_dir = data_dir()?;

    match action {
        ConfigAction::Export { path } => {
            let config = AppConfig::load(&data_dir);
            if config.php_versions.is_empty() {
                ui::warn("No PHP versions registered — exporting an empty registry.");
            }
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| LauncherError::Other(anyhow::anyhow!("serializing config: {e}")))?;
            std::fs::write(&path, json)?;
            println!("Exported {} PHP version(s) to {}", config.php_versions.len(), path.display());
        }

        ConfigAction::Import { path } => {
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                LauncherError::Extraction(format!("couldn't read {}: {e}", path.display()))
            })?;
            let imported: AppConfig = serde_json::from_str(&raw).map_err(|e| {
                LauncherError::Extraction(format!("'{}' isn't a valid Ruxius config file: {e}", path.display()))
            })?;

            let mut config = AppConfig::load(&data_dir);
            let mut added = 0;
            let mut updated = 0;
            for (name, php_path) in imported.php_versions {
                if config.php_versions.insert(name.clone(), php_path).is_some() {
                    updated += 1;
                } else {
                    added += 1;
                }
            }
            config.save(&data_dir).map_err(LauncherError::Other)?;
            println!("Imported: {added} added, {updated} updated. Run `rux php list` to check they resolve on this machine.");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------
// `rux php ext ...`
// ---------------------------------------------------------------------

fn run_ext_command(action: cli::ExtAction, config: &AppConfig) -> Result<()> {
    match action {
        cli::ExtAction::List { php } => {
            let (php_ini, ext_dir, binary) = resolve_php_ini(config, &php)?;
            println!("{}", ui::bold(&format!("php: {}", binary.display())));
            println!("{}", ui::dim(&format!("ini: {}", php_ini.display())));
            println!();

            let configured = ext::list_configured(&php_ini)?;
            if configured.is_empty() {
                println!("No extension= lines found in this php.ini.");
            } else {
                for e in &configured {
                    let marker = if e.enabled { ui::green("✓") } else { ui::dim("○") };
                    let kind = if e.zend { " (zend)" } else { "" };
                    println!("  {marker} {}{kind}", e.name);
                }
            }

            if let Some(ext_dir) = &ext_dir {
                let available = ext::list_available_unconfigured(&php_ini, ext_dir)?;
                if !available.is_empty() {
                    println!("\n{}", ui::dim("Available but not configured:"));
                    for a in available {
                        println!("  {} {}", ui::dim("·"), a.name);
                    }
                }
            }
        }

        cli::ExtAction::Enable { php, extension } => {
            let (php_ini, ext_dir, _) = resolve_php_ini(config, &php)?;
            let outcome = ext::set_enabled(&php_ini, ext_dir.as_deref(), &extension, true)?;
            report_toggle_outcome(&extension, true, outcome);
        }

        cli::ExtAction::Disable { php, extension } => {
            let (php_ini, ext_dir, _) = resolve_php_ini(config, &php)?;
            let outcome = ext::set_enabled(&php_ini, ext_dir.as_deref(), &extension, false)?;
            report_toggle_outcome(&extension, false, outcome);
        }
    }
    Ok(())
}

/// Resolves a `php` CLI argument (registered name or path) down to its
/// php.ini, extension directory (if any), and the binary itself — the
/// trio every `rux php ext` subcommand needs.
fn resolve_php_ini(
    config: &AppConfig,
    php: &str,
) -> Result<(PathBuf, Option<PathBuf>, PathBuf)> {
    let path = config.resolve_php_reference(php);
    let resolved = php::resolve_external_php(&path).map_err(|e| {
        LauncherError::PhpStart(format!(
            "couldn't resolve PHP '{php}' (looked at '{}'): {e}",
            path.display()
        ))
    })?;
    let php_ini = resolved.php_ini.ok_or_else(|| {
        LauncherError::Extraction(format!(
            "no php.ini found next to {}",
            resolved.binary.display()
        ))
    })?;
    Ok((php_ini, resolved.extension_dir, resolved.binary))
}

fn report_toggle_outcome(extension: &str, enabling: bool, outcome: ext::ToggleOutcome) {
    match outcome {
        ext::ToggleOutcome::Changed => {
            let verb = if enabling { "enabled" } else { "disabled" };
            println!("{} '{extension}' {verb}.", ui::green("✓"));
        }
        ext::ToggleOutcome::AddedNewLine => {
            println!("{} '{extension}' enabled (added new line to php.ini).", ui::green("✓"));
        }
        ext::ToggleOutcome::AlreadyInThatState => {
            let state = if enabling { "enabled" } else { "disabled" };
            ui::info(&format!("'{extension}' was already {state}."));
        }
        ext::ToggleOutcome::NotAvailable => {
            ui::error(&format!(
                "'{extension}' isn't configured and no matching DLL was found in ext/."
            ));
        }
    }
}

/// Looks for PHP installs in common Windows locations (`C:\php*`,
/// `C:\Program Files\PHP*`) and on PATH, purely as a convenience for
/// `rux php list`. This is best-effort discovery, not exhaustive.
fn discover_php_installations() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    #[cfg(windows)]
    let scan_roots: &[&str] = &["C:\\", "C:\\Program Files", "C:\\Program Files (x86)"];
    #[cfg(target_os = "macos")]
    let scan_roots: &[&str] = &[
        "/usr/local/bin",
        "/usr/local/opt", // Homebrew (Intel)
        "/opt/homebrew/opt", // Homebrew (Apple Silicon)
        "/opt/homebrew/bin",
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    let scan_roots: &[&str] = &["/usr/bin", "/usr/local/bin", "/opt", "/usr/local/php"];

    for root in scan_roots {
        let root = Path::new(root);
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy().to_lowercase();
            if name.starts_with("php") {
                candidates.push(entry.path());
            }
        }
        // Also consider the root itself a candidate directory (covers
        // e.g. `/usr/bin/php` directly, or `/usr/local/bin/php` sitting
        // right in a dir we just scanned for php*-named subfolders).
        candidates.push(root.to_path_buf());
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            candidates.push(dir);
        }
    }

    // Checking each candidate is a filesystem stat; with a long PATH this
    // can be dozens of directories, so check them all concurrently instead
    // of one at a time.
    let checked = payload::parallel_map(&candidates, |dir| {
        let windows_style = dir.join("php.exe");
        if windows_style.is_file() {
            return Some(windows_style);
        }
        let unix_style = dir.join("php");
        unix_style.is_file().then_some(unix_style)
    });

    let mut found = Vec::new();
    for candidate in checked.into_iter().flatten() {
        if !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    found
}

// ---------------------------------------------------------------------
// `rux init`
// ---------------------------------------------------------------------

fn run_init_command() -> Result<()> {
    println!("{}", ui::bold("Ruxius setup"));
    println!();

    match find_webview_runtime() {
        Some(version) => ui::ok(&format!("WebView backend found ({version})")),
        None => ui::warn("No WebView backend found yet — run `rux doctor` for install instructions."),
    }
    println!();

    let data_dir = data_dir()?;
    std::fs::create_dir_all(&data_dir)?;
    let mut config = AppConfig::load(&data_dir);

    if config.php_versions.is_empty() {
        println!("No PHP versions registered yet — let's add one.");
    } else {
        println!("Already registered:");
        for (name, path) in &config.php_versions {
            println!("  {name:<12} {}", path.display());
        }
        if prompt_yes_no("Register another PHP install?", false)? {
            println!();
        } else {
            return finish_init(&config, &data_dir);
        }
    }

    let discovered = discover_php_installations();
    let chosen_path = if discovered.is_empty() {
        println!("Couldn't find any PHP installs automatically.");
        prompt("Path to php.exe (or its folder): ")?
    } else {
        println!("Found these PHP installs:");
        for (i, path) in discovered.iter().enumerate() {
            println!("  {}) {}", i + 1, path.display());
        }
        let answer = prompt("Pick a number, or type a path directly: ")?;
        match answer.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= discovered.len() => {
                discovered[n - 1].display().to_string()
            }
            _ => answer,
        }
    };

    let resolved = match php::resolve_external_php(Path::new(chosen_path.trim())) {
        Ok(resolved) => resolved,
        Err(e) => {
            ui::error(&format!("{e}"));
            return finish_init(&config, &data_dir);
        }
    };

    let suggested_name = php_version_string(&resolved.binary)
        .and_then(|v| v.split_whitespace().nth(1).map(|s| format!("php{}", s.chars().take(3).collect::<String>().replace('.', ""))))
        .unwrap_or_else(|| "php".to_string());
    let name_prompt = format!("Name for this PHP install [{suggested_name}]: ");
    let name_input = prompt(&name_prompt)?;
    let name = if name_input.trim().is_empty() { suggested_name } else { name_input.trim().to_string() };

    config.php_versions.insert(name.clone(), resolved.binary.clone());
    config.save(&data_dir).map_err(LauncherError::Other)?;
    ui::ok(&format!("Registered '{name}' -> {}", resolved.binary.display()));
    println!();

    finish_init(&config, &data_dir)
}

fn finish_init(config: &AppConfig, data_dir: &Path) -> Result<()> {
    let sample_dir = Path::new("ruxius-sample-app");
    if !sample_dir.exists() && prompt_yes_no("Create a minimal sample app to try building?", true)? {
        std::fs::create_dir_all(sample_dir)?;
        std::fs::write(
            sample_dir.join("index.php"),
            "<?php\necho \"<h1>Hello from Ruxius</h1>\";\necho \"<p>PHP \" . phpversion() . \"</p>\";\n",
        )?;
        ui::ok(&format!("Created {}/index.php", sample_dir.display()));
    }

    println!();
    println!("{}", ui::bold("Next steps"));
    if let Some(name) = config.php_versions.keys().next() {
        if sample_dir.join("index.php").is_file() {
            println!(
                "  rux build {} {name} MyApp.exe",
                sample_dir.display()
            );
        } else {
            println!("  rux build <app-folder> {name} MyApp.exe");
        }
    } else {
        println!("  rux php add <name> <path-to-php.exe>   (no PHP registered yet)");
    }
    println!("  rux tui                                 (or drive all of this from a dashboard)");
    println!();
    println!("{}", ui::dim(&format!("Config lives at {}", AppConfig::config_path(data_dir).display())));

    Ok(())
}

/// Reads one line from stdin after printing `label` (no trailing newline).
fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn prompt_yes_no(question: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    let answer = prompt(&format!("{question} {hint} "))?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(match answer.as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    })
}

// ---------------------------------------------------------------------
// `rux doctor`
// ---------------------------------------------------------------------

fn run_doctor_command() -> Result<()> {
    println!("{}", ui::bold(&format!("Ruxius {}", version::LAUNCHER_VERSION)));
    println!();

    let data_dir = data_dir()?;
    let config = AppConfig::load(&data_dir);
    let cache_dir = data_dir.join("cache").join("archives");
    let packs_dir = AppConfig::packs_dir(&data_dir);

    // These four checks don't depend on each other and are all filesystem
    // I/O (directory scans, `stat` calls) rather than CPU work, so running
    // them concurrently shortens `rux doctor`'s wall-clock time instead of
    // paying for each one back to back.
    let (webview2, php_status, cache_count, pack_count) = std::thread::scope(|scope| {
        let webview2 = scope.spawn(find_webview_runtime);
        let php_status = scope.spawn(|| {
            let mut ok = 0;
            let mut missing = 0;
            for path in config.php_versions.values() {
                match php::resolve_external_php(path) {
                    Ok(_) => ok += 1,
                    Err(_) => missing += 1,
                }
            }
            (ok, missing)
        });
        let cache_count = scope.spawn(|| std::fs::read_dir(&cache_dir).map(|d| d.flatten().count()).ok());
        let pack_count = scope.spawn(|| pack::list_names(&packs_dir).len());

        (
            webview2.join().unwrap_or(None),
            php_status.join().unwrap_or((0, 0)),
            cache_count.join().unwrap_or(None),
            pack_count.join().unwrap_or(0),
        )
    });

    match webview2 {
        Some(version) => ui::ok(&format!("WebView backend found ({version})")),
        None => {
            ui::error("WebView backend not found — built apps need it to open their window.");
            #[cfg(windows)]
            {
                println!("          Get the WebView2 Runtime from:");
                println!("          {}", ui::cyan("https://developer.microsoft.com/microsoft-edge/webview2/"));
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                println!("          Install webkit2gtk, e.g.:");
                println!("          {}", ui::cyan("sudo apt install libwebkit2gtk-4.1-0   # Debian/Ubuntu"));
                println!("          {}", ui::cyan("sudo dnf install webkit2gtk4.1          # Fedora"));
            }
        }
    }

    let (ok, missing) = php_status;
    if config.php_versions.is_empty() {
        ui::info("No PHP versions registered yet (rux php add <name> <path>).");
    } else if missing == 0 {
        ui::ok(&format!("{ok} registered PHP version(s), all valid."));
    } else {
        ui::warn(&format!(
            "{ok} registered PHP version(s) valid, {missing} missing — check `rux php list`."
        ));
    }

    match cache_count {
        Some(count) => ui::info(&format!("{count} cached build archive(s) in {}", cache_dir.display())),
        None => ui::info("No cached build archives yet."),
    }
    ui::info(&format!("{pack_count} .pack file(s) in {}", packs_dir.display()));

    Ok(())
}

/// Looks for an installed WebView2 Runtime in the locations the Evergreen
/// installer actually uses (both per-machine and per-user), and returns
/// its version folder name if found. Filesystem-based rather than a
/// registry check, so it doesn't need an extra dependency just for this.
/// Checks that this platform's WebView backend is actually available.
/// Windows needs the WebView2 Runtime installed separately; macOS always
/// has WKWebView built into the OS; Linux needs `webkit2gtk` installed
/// (both as a build-time dependency and at runtime) — checked here via
/// `pkg-config`, which is present on essentially every Linux dev/desktop
/// system, rather than adding a dependency just for this one check.
#[cfg(windows)]
fn find_webview_runtime() -> Option<String> {
    let mut candidates = vec![
        PathBuf::from(r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application"),
        PathBuf::from(r"C:\Program Files\Microsoft\EdgeWebView\Application"),
    ];
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        candidates.push(Path::new(&local_app_data).join(r"Microsoft\EdgeWebView\Application"));
    }

    for base in candidates {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        // Version folders look like "124.0.2478.97"; take the first one we see.
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn find_webview_runtime() -> Option<String> {
    // WKWebView ships with every supported macOS version — nothing to
    // install, nothing meaningful to version-check from here.
    Some("WKWebView (built into macOS)".to_string())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn find_webview_runtime() -> Option<String> {
    for pc_name in ["webkit2gtk-4.1", "webkit2gtk-4.0"] {
        let found = std::process::Command::new("pkg-config")
            .args(["--modversion", pc_name])
            .output();
        if let Ok(output) = found {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return Some(format!("{pc_name} {version}"));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------

fn data_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from(APP_QUALIFIER, APP_ORGANIZATION, APP_NAME)
        .ok_or(LauncherError::NoDataDir)?;
    Ok(dirs.data_local_dir().to_path_buf())
}

/// Where per-app PHP logs (stderr: access log + warnings/errors/notices)
/// are written — `%LOCALAPPDATA%\Ruxius\logs\php-<checksum>.log`, one file
/// per distinct built app (keyed by its payload checksum, same as its
/// extraction folder), separate from Ruxius's own diagnostic log
/// (`ruxius-YYYY-MM-DD.log` in the same directory, written by `logger.rs`).
fn logs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// Holds an exclusive advisory lock on a file in the data directory for the
/// lifetime of the process, guaranteeing only one instance of a given app
/// runs at a time. The lock is released automatically (dropping `File`
/// releases the OS-level lock) when the process exits, including on crash.
struct InstanceLock(#[allow(dead_code)] File);

fn acquire_single_instance_lock(data_dir: &Path) -> Result<InstanceLock> {
    let lock_path = data_dir.join("ruxius.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;

    file.try_lock_exclusive().map_err(|_| {
        log::warn!("Another instance is already running; exiting.");
        LauncherError::AlreadyRunning
    })?;

    Ok(InstanceLock(file))
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("PANIC: {info}");
        default_hook(info);
    }));
}

/// Hides the console window belonging to this process, if any. Used only
/// when we're about to open a bundled app's WebView window, so a built app
/// launched by double-clicking doesn't leave a console flashing on screen.
/// A no-op if there's no console (e.g. none was allocated) or on
/// non-Windows platforms.
#[cfg(windows)]
fn hide_console_window() {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    unsafe {
        let window = GetConsoleWindow();
        if window != 0 {
            ShowWindow(window, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn hide_console_window() {}

fn install_ctrlc_handler(php_server: Arc<Mutex<Option<PhpServer>>>) {
    let result = ctrlc::set_handler(move || {
        log::info!("Ctrl+C received; shutting down PHP server and exiting.");
        if let Ok(mut guard) = php_server.lock() {
            if let Some(mut server) = guard.take() {
                server.shutdown();
            }
        }
        std::process::exit(0);
    });

    if let Err(e) = result {
        log::warn!("Failed to install Ctrl+C handler: {e}");
    }
}
