use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Ruxius — packages a PHP app into a standalone Windows executable and
/// launches apps that have already been packaged.
///
/// The compiled tool itself (`ruxius.exe`) is used from the command line as
/// `rux`. Running it bare with no subcommand shows this help. Apps you
/// build with `rux build` don't take any commands at all — just double
/// click the resulting .exe to run it.
#[derive(Debug, Parser)]
#[command(name = "rux", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Package a PHP app + a PHP install into a new standalone .exe.
    /// Does not compile anything — it copies this executable and appends
    /// the app/PHP files to it.
    ///
    ///   rux build <app-path> <php> <output-path>
    ///
    /// <php> may be a name registered with `rux php add`, or a direct path
    /// to a php.exe (or its containing folder).
    Build {
        /// Folder containing the PHP application's files (its document root).
        app_path: PathBuf,

        /// Registered PHP version name, or a path to php.exe / its folder.
        php: String,

        /// Where to write the resulting standalone executable.
        output_path: PathBuf,

        /// Window title for the built app. Defaults to the app folder's name.
        #[arg(long)]
        title: Option<String>,

        /// Window width in pixels.
        #[arg(long, default_value_t = 1400)]
        width: u32,

        /// Window height in pixels.
        #[arg(long, default_value_t = 900)]
        height: u32,

        /// Rebuild even if the output already matches what would be built
        /// (by default, an up-to-date output is left untouched).
        #[arg(long)]
        force: bool,
    },

    /// Manage the registry of named PHP versions used by `rux build`.
    Php {
        #[command(subcommand)]
        action: PhpAction,
    },

    /// Check this machine for what `rux` needs to build and run apps
    /// (currently: the WebView2 Runtime).
    Doctor,

    /// Open an interactive terminal dashboard for managing PHP versions,
    /// archives, and extensions.
    Tui,
}

#[derive(Debug, Subcommand)]
pub enum PhpAction {
    /// Register a named PHP install for later use with `rux build`, e.g.:
    ///   rux php add php74 "C:\php\php7.4\php.exe"
    Add {
        /// Name to refer to this PHP install by (e.g. "php74", "php83").
        name: String,
        /// Path to a php.exe (or a directory containing one).
        path: PathBuf,
    },

    /// Remove a previously registered PHP version by name.
    Remove {
        name: String,
    },

    /// List registered PHP versions, plus installs Ruxius can find
    /// automatically (common install locations and PATH).
    List,

    /// Delete cached PHP/app archives built up by `rux build` (under
    /// `%LOCALAPPDATA%\Ruxius\cache\archives\`), forcing the next build to
    /// repack everything from scratch.
    ClearCache,

    /// Snapshot every registered PHP install into a `<name>.pack` file, so
    /// `rux build` can read it straight off disk instead of re-walking and
    /// re-compressing that install on every build. A `.pack` is trusted as
    /// current once made — re-run this after changing that PHP install.
    Archive,

    /// Manage which extensions are enabled in a registered PHP install's
    /// php.ini.
    Ext {
        #[command(subcommand)]
        action: ExtAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ExtAction {
    /// List configured extensions (enabled/disabled) plus ones available
    /// in ext/ but not configured either way.
    List {
        /// Registered PHP version name, or a path to php.exe / its folder.
        php: String,
    },

    /// Enable an extension, e.g. `rux php ext enable php74 curl`.
    Enable {
        php: String,
        /// Extension name, with or without the `php_`/`.dll` decoration.
        extension: String,
    },

    /// Disable an extension.
    Disable {
        php: String,
        extension: String,
    },
}
