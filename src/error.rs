use thiserror::Error;

#[derive(Debug, Error)]
pub enum LauncherError {
    #[error("failed to determine application data directory")]
    NoDataDir,

    #[error("another instance of Ruxius is already running")]
    AlreadyRunning,

    #[error("runtime extraction failed: {0}")]
    Extraction(String),

    #[error("checksum verification failed: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("no free TCP port available on 127.0.0.1")]
    NoFreePort,

    #[error("failed to start PHP server: {0}")]
    PhpStart(String),

    #[error("PHP server did not become ready in time")]
    PhpNotReady,

    #[error("webview initialization failed: {0}")]
    WebView(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("archive error: {0}")]
    Archive(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, LauncherError>;
