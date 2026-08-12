use crate::error::{LauncherError, Result};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct PhpServer {
    child: Child,
    pub port: u16,
}

/// Fully resolved information about a PHP install: the binary itself, plus
/// the extension_dir and php.ini that belong with it.
pub struct ResolvedPhp {
    pub binary: PathBuf,
    pub extension_dir: Option<PathBuf>,
    pub php_ini: Option<PathBuf>,
}

impl PhpServer {
    /// Picks a free localhost port, launches PHP's built-in web server
    /// against `bundle_dir/app` using the PHP bundled at `bundle_dir/php`,
    /// hides its console window, and blocks (with a timeout) until the
    /// server is accepting connections.
    ///
    /// `router` is the filename (relative to the docroot, e.g.
    /// `"router.php"`) of a router script to pass to PHP's built-in server,
    /// if the built app needs one — see `framework.rs`. Plain apps with no
    /// detected framework pass `None` and behave exactly as before.
    pub fn start(bundle_dir: &Path, router: Option<&str>) -> Result<Self> {
        let port = portpicker::pick_unused_port().ok_or(LauncherError::NoFreePort)?;

        let php_dir = bundle_dir.join("php");
        let resolved = resolve_external_php(&php_dir)?;

        let app_dir = bundle_dir.join("app");
        let docroot: PathBuf = if app_dir.is_dir() {
            app_dir
        } else {
            bundle_dir.to_path_buf()
        };

        // Only actually pass the router argument if the file is there —
        // if it went missing somehow, falling back to no-router behavior
        // is safer than pointing php.exe at a script that doesn't exist.
        let router = router.filter(|r| docroot.join(r).is_file());

        log::info!(
            "Starting PHP server: {} -S 127.0.0.1:{port} -t {}{}",
            resolved.binary.display(),
            docroot.display(),
            router.map(|r| format!(" {r}")).unwrap_or_default()
        );
        if let Some(ext_dir) = &resolved.extension_dir {
            log::info!("Using extension_dir: {}", ext_dir.display());
        }

        let mut cmd = Command::new(&resolved.binary);
        cmd.arg("-S")
            .arg(format!("127.0.0.1:{port}"))
            .arg("-t")
            .arg(&docroot)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        // Explicitly setting extension_dir on the command line is what makes
        // switching between PHP versions/installs actually work: each PHP
        // build's extensions are compiled against that build's ABI, so we
        // must always point at the extensions that ship next to whichever
        // php.exe we're running.
        if let Some(ext_dir) = &resolved.extension_dir {
            cmd.arg("-d").arg(format!("extension_dir={}", ext_dir.display()));
        }

        if let Some(php_ini) = &resolved.php_ini {
            if let Some(ini_dir) = php_ini.parent() {
                cmd.env("PHPRC", ini_dir);
            }
        }

        // The router script must be PHP's last positional argument, after
        // every -S/-t/-d flag.
        if let Some(router) = router {
            cmd.arg(router);
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd.spawn().map_err(|e| {
            LauncherError::PhpStart(format!("{e} (binary: {})", resolved.binary.display()))
        })?;

        let server = PhpServer { child, port };
        server.wait_until_ready(Duration::from_secs(15))?;
        Ok(server)
    }

    fn wait_until_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let addr = format!("127.0.0.1:{}", self.port);

        while Instant::now() < deadline {
            if TcpStream::connect(&addr).is_ok() {
                log::info!("PHP server is accepting connections on {addr}");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        Err(LauncherError::PhpNotReady)
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Terminates the PHP process. Called explicitly on graceful shutdown
    /// and also from `Drop` as a last-resort safety net so no orphan
    /// `php.exe` process can survive the launcher exiting.
    pub fn shutdown(&mut self) {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                log::info!("PHP server already exited with status {status}");
                return;
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("Failed to poll PHP process status: {e}");
            }
        }

        log::info!("Terminating PHP server (pid {})", self.child.id());
        if let Err(e) = self.child.kill() {
            log::warn!("Failed to kill PHP process: {e}");
        }
        // Reap the process so it doesn't linger as a zombie.
        let _ = self.child.wait();
    }
}

impl Drop for PhpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Given either a direct path to a php.exe or a directory containing one,
/// figures out the binary, its extension_dir, and its php.ini, so PHP
/// works correctly regardless of which version/install was selected.
///
/// Used both by `rux build` (to validate and locate the PHP being
/// packaged) and by `PhpServer::start` (to resolve the PHP bundled inside
/// an already-built app).
pub fn resolve_external_php(path: &Path) -> Result<ResolvedPhp> {
    let binary = if path.is_dir() {
        let candidate = path.join("php.exe");
        if candidate.is_file() {
            candidate
        } else {
            let fallback = path.join("php");
            if fallback.is_file() {
                fallback
            } else {
                return Err(LauncherError::PhpStart(format!(
                    "no php.exe found in {}",
                    path.display()
                )));
            }
        }
    } else {
        if !path.is_file() {
            return Err(LauncherError::PhpStart(format!(
                "PHP binary not found at {}",
                path.display()
            )));
        }
        path.to_path_buf()
    };

    let php_dir = binary
        .parent()
        .ok_or_else(|| LauncherError::PhpStart("PHP binary has no parent directory".into()))?
        .to_path_buf();

    let ext_dir = find_extension_dir(&php_dir);
    let php_ini = php_dir.join("php.ini");
    let php_ini = php_ini.is_file().then_some(php_ini);

    Ok(ResolvedPhp {
        binary,
        extension_dir: ext_dir,
        php_ini,
    })
}

/// PHP's official Windows distributions ship extensions in an `ext/`
/// folder next to php.exe; some third-party builds use `extensions/`
/// instead. We check both, in order, and fall back to `None` (letting PHP
/// use whatever extension_dir its own php.ini specifies) if neither exists.
fn find_extension_dir(php_dir: &Path) -> Option<PathBuf> {
    for candidate in ["ext", "extensions"] {
        let dir = php_dir.join(candidate);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    None
}
