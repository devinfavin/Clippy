use std::path::PathBuf;
use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Return the size of a file in bytes. Used by the post-export toast.
#[tauri::command]
pub fn file_size(path: String) -> Result<u64, String> {
    std::fs::metadata(&path)
        .map(|m| m.len())
        .map_err(|e| format!("file_size {}: {}", path, e))
}

/// Sum the bytes used by every cached proxy/remux/waveform/project file.
#[tauri::command]
pub fn cache_size(app: AppHandle) -> Result<u64, String> {
    let dir = match app.path().app_data_dir() {
        Ok(d) => d.join("proxies"),
        Err(_) => return Ok(0),
    };
    if !dir.exists() {
        return Ok(0);
    }
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    Ok(total)
}

/// Wipe every file in the cache directory. Returns bytes freed.
#[tauri::command]
pub fn clear_cache(app: AppHandle) -> Result<u64, String> {
    let dir = match app.path().app_data_dir() {
        Ok(d) => d.join("proxies"),
        Err(_) => return Ok(0),
    };
    if !dir.exists() {
        return Ok(0);
    }
    let mut freed: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    let n = meta.len();
                    if std::fs::remove_file(entry.path()).is_ok() {
                        freed = freed.saturating_add(n);
                    }
                }
            }
        }
    }
    Ok(freed)
}

/// Compound readout for the Settings/Storage tab. Bundles every disk-space
/// signal the UI needs in one round-trip: total app-data footprint, the
/// proxies cache subdir, the persisted diag log, and the resolved paths so
/// the user can hit "Open folder" without us re-resolving on the frontend.
#[derive(Serialize)]
pub struct StorageSummary {
    app_data_dir: String,
    app_data_total_bytes: u64,
    proxies_dir: String,
    proxies_bytes: u64,
    diagnostics_log_path: String,
    diagnostics_log_bytes: u64,
    /// Anything in app-data that isn't proxies or diagnostics.log — project
    /// JSON files, persisted save-dir pref, etc. Helps the user understand
    /// the "where's the rest going" delta when total ≠ proxies + log.
    other_bytes: u64,
}

#[tauri::command]
pub fn storage_summary(app: AppHandle) -> Result<StorageSummary, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let proxies = app_data.join("proxies");
    let log = app_data.join("diagnostics.log");

    let proxies_bytes = dir_size_recursive(&proxies);
    let diagnostics_log_bytes = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let total = dir_size_recursive(&app_data);
    let other_bytes = total
        .saturating_sub(proxies_bytes)
        .saturating_sub(diagnostics_log_bytes);

    Ok(StorageSummary {
        app_data_dir: app_data.to_string_lossy().into_owned(),
        app_data_total_bytes: total,
        proxies_dir: proxies.to_string_lossy().into_owned(),
        proxies_bytes,
        diagnostics_log_path: log.to_string_lossy().into_owned(),
        diagnostics_log_bytes,
        other_bytes,
    })
}

/// Recursive byte sum for a directory tree. Returns 0 on any I/O error so a
/// missing or inaccessible subtree doesn't fail the parent measurement.
fn dir_size_recursive(dir: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total = total.saturating_add(dir_size_recursive(&entry.path()));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Auto-prune cache files that haven't been touched in `days` days. Runs on
/// app start in a background thread so a slow disk doesn't block startup.
pub(crate) fn prune_old_cache(dir: PathBuf, days: u64) {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(days * 24 * 60 * 60));
    let Some(cutoff) = cutoff else { return; };
    let Ok(entries) = std::fs::read_dir(&dir) else { return; };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue; };
        if !meta.is_file() {
            continue;
        }
        // Use mtime; access-time tracking is often disabled on Windows.
        let touched = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if touched < cutoff {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Open the OS file manager with the given file selected. Windows-specific:
/// uses explorer.exe with /select,. On other platforms we'd fall back to opening
/// the parent directory.
#[tauri::command]
pub fn reveal_in_folder(path: String) -> Result<(), String> {
    // Canonicalize first — both validates the path exists and resolves
    // to a real local filesystem path, so a malformed `path` containing
    // shell-meaningful chars or extra arguments can't reach the OS.
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;

    #[cfg(target_os = "windows")]
    {
        // Pass `/select,<path>` as ONE argv entry via Command::arg. Rust's
        // stdlib applies the correct CreateProcess quoting on Windows, so
        // we don't need raw_arg + manual quoting (which was quote-injection
        // prone if `path` contained `"`).
        let mut s = std::ffi::OsString::from("/select,");
        s.push(canonical.as_os_str());
        std::process::Command::new("explorer")
            .arg(s)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&canonical)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        // Linux fallback: open the parent dir with xdg-open
        let parent = canonical
            .parent()
            .ok_or_else(|| "no parent directory".to_string())?;
        std::process::Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
