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
    },

    /// Manage the registry of named PHP versions used by `rux build`.
    Php {
        #[command(subcommand)]
        action: PhpAction,
    },
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
}
