//! PHP's built-in web server (`php -S`) serves files as-is — it has no
//! `.htaccess`/rewrite support, so a request for `/login` on a framework
//! app just 404s unless something routes it to the front controller.
//! Laravel and Symfony (and most modern PHP frameworks) also expect to be
//! served from a `public/` subdirectory, not the project root.
//!
//! This module detects that layout and supplies both fixes: the right
//! docroot, and a small router script (embedded here, never written to
//! the user's actual project folder — see `payload::pack`'s `extra`
//! parameter) that PHP's built-in server runs on every request, serving
//! real files as-is and falling back to the front controller for
//! everything else — exactly what `.htaccess`/nginx rewrite rules do for
//! a real server.

use std::path::{Path, PathBuf};

/// The router filename injected into the app archive when one is needed.
/// Also the argument passed to `php -S ... -t <docroot> <ROUTER_FILENAME>`.
pub const ROUTER_FILENAME: &str = "_ruxius_router.php";

/// A router script for PHP's built-in server: serve the request as a real
/// file if one exists at that path, otherwise hand off to the front
/// controller. This is the same pattern Laravel's own docs recommend for
/// `php artisan serve` (which does exactly this under the hood) and what
/// Symfony's `bin/console server:run` used to ship before it moved to a
/// dedicated local web server — so it's a well-trodden approach, not a
/// Ruxius invention.
const ROUTER_SCRIPT: &str = r#"<?php
// Ruxius router for PHP's built-in server: serve real files/assets as-is,
// route everything else to the front controller (so pretty URLs work the
// way they would behind Apache's mod_rewrite or nginx's try_files).
$path = urldecode(parse_url($_SERVER['REQUEST_URI'], PHP_URL_PATH));
$candidate = __DIR__ . $path;
if ($path !== '/' && file_exists($candidate) && !is_dir($candidate)) {
    return false; // let the built-in server handle it directly
}
require __DIR__ . '/index.php';
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Laravel,
    Symfony,
    /// Some other framework/app with a `public/index.php` layout — same
    /// docroot-and-router treatment, just not specifically identified.
    GenericPublic,
    /// A plain app with `index.php` (or similar) at its root — the
    /// original Ruxius behavior, completely unaffected by any of this.
    Plain,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Laravel => "Laravel",
            Kind::Symfony => "Symfony",
            Kind::GenericPublic => "generic (public/ layout)",
            Kind::Plain => "plain PHP app",
        }
    }
}

pub struct Detection {
    pub kind: Kind,
    /// Absolute path to use as the docroot when packing — either
    /// `app_path` itself (`Plain`) or `app_path/public`.
    pub docroot: PathBuf,
    pub needs_router: bool,
}

/// Inspects `app_path` and decides how it should be served. Detection is
/// deliberately conservative: it only redirects the docroot to `public/`
/// when `public/index.php` actually exists there, so a plain flat-file
/// app that merely happens to have a `public` folder of static assets
/// isn't misdetected.
pub fn detect(app_path: &Path) -> Detection {
    let public_index = app_path.join("public").join("index.php");
    if !public_index.is_file() {
        return Detection {
            kind: Kind::Plain,
            docroot: app_path.to_path_buf(),
            needs_router: false,
        };
    }

    let kind = if app_path.join("artisan").is_file() {
        Kind::Laravel
    } else if app_path.join("bin").join("console").is_file() {
        Kind::Symfony
    } else {
        Kind::GenericPublic
    };

    Detection {
        kind,
        docroot: app_path.join("public"),
        needs_router: true,
    }
}

/// The router file as an in-memory `(name, bytes)` pair, ready to hand to
/// `payload::pack`'s `app_extra_files` — never written to disk in the
/// user's project.
pub fn router_file() -> (String, Vec<u8>) {
    (ROUTER_FILENAME.to_string(), ROUTER_SCRIPT.as_bytes().to_vec())
}

/// Best-effort checks for the things Laravel/Symfony apps almost always
/// need but that Ruxius can't set up itself (no network access, and
/// running `composer install` on the user's behalf would be a lot of
/// implicit magic for a packaging tool). Returned as plain warning
/// strings for the caller to print — none of these block the build, since
/// an app missing `.env` might still be intentionally configured via real
/// environment variables instead.
pub fn compatibility_warnings(app_path: &Path, detection: &Detection) -> Vec<String> {
    let mut warnings = Vec::new();

    if detection.kind == Kind::Plain {
        return warnings;
    }

    if !app_path.join("vendor").join("autoload.php").is_file() {
        warnings.push(
            "vendor/autoload.php not found — run `composer install` in this project \
             before building, or the app will fail immediately on launch."
                .to_string(),
        );
    }

    if !app_path.join(".env").is_file() {
        let what = match detection.kind {
            Kind::Laravel => "APP_KEY and other Laravel settings",
            Kind::Symfony => "APP_SECRET and other Symfony settings",
            _ => "app settings",
        };
        warnings.push(format!(
            ".env not found — {what} normally come from one. Skip this if you're \
             providing configuration another way."
        ));
    }

    warnings
}
