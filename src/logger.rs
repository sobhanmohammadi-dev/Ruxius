use anyhow::{Context, Result};
use std::path::Path;

/// Initializes file + stdout logging. Log files are rotated by date and
/// written to `<data_dir>/logs/applauncher-YYYY-MM-DD.log`.
pub fn init(data_dir: &Path) -> Result<()> {
    let log_dir = data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).context("failed to create log directory")?;

    let date = chrono::Local::now().format("%Y-%m-%d");
    let log_file_path = log_dir.join(format!("applauncher-{date}.log"));

    let file = fern::log_file(&log_file_path).context("failed to open log file")?;

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{} {} {}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.level(),
                record.target(),
                message
            ))
        })
        .level(log::LevelFilter::Info)
        .chain(std::io::stdout())
        .chain(file)
        .apply()
        .context("failed to install logger")?;

    prune_old_logs(&log_dir);

    log::info!("Logger initialized. Writing to {}", log_file_path.display());
    Ok(())
}

/// Keeps the log directory tidy by removing log files older than 14 days.
fn prune_old_logs(log_dir: &Path) {
    let cutoff = chrono::Local::now() - chrono::Duration::days(14);
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                let modified: chrono::DateTime<chrono::Local> = modified.into();
                if modified < cutoff {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}
