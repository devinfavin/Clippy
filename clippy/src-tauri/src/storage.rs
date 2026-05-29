use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Return the size of a file in bytes. Used by the post-export toast.
#[tauri::command]
pub fn file_size(path: String) -> Result<u64, String> {
    std::fs::metadata(&path)
        .map(|m| m.len())
        .map_err(|e| format!("file_size {}: {}", path, e))
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

/// Phase-4 storage cap surface — usage breakdown for the Storage settings
/// panel. Measures the *user-facing* buckets the cap policy operates on.
///   - `saved_replays_bytes`: video files in the user's save dir
///   - `cache_bytes`: proxy cache (decoded MP4s + waveforms for scrubbing)
///   - `other_bytes`: rest of %APPDATA%/Clippy — diag log, project JSONs,
///     allowlist, opened-replays index, save-dir pref, etc.
///   - `total_bytes`: sum used to drive the cap progress bar.
#[derive(Serialize, Default)]
pub struct StorageUsage {
    pub save_dir: String,
    pub saved_replays_bytes: u64,
    pub saved_replays_count: u64,
    pub cache_bytes: u64,
    pub other_bytes: u64,
    pub total_bytes: u64,
}

/// Walk the save directory and bucket video files only — exclude any sidecar
/// JSON / README the user dropped in. Returns (total bytes, file count).
fn saved_replays_stats(dir: &Path) -> (u64, u64) {
    let mut bytes: u64 = 0;
    let mut count: u64 = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if is_video_file(&entry.file_name().to_string_lossy()) {
            bytes = bytes.saturating_add(meta.len());
            count += 1;
        }
    }
    (bytes, count)
}

fn is_video_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".mp4")
        || lower.ends_with(".mkv")
        || lower.ends_with(".mov")
        || lower.ends_with(".webm")
        || lower.ends_with(".m4v")
}

/// Single saved-replay entry as the frontend's Recent empty state expects.
/// `modified_secs` is unix-epoch seconds — the frontend formats relative
/// times (e.g. "4 min ago"). Sorted newest-first by the caller.
#[derive(Serialize)]
pub struct SavedReplay {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub modified_secs: i64,
}

/// List saved replays in the user's save dir, newest-first. Bounded by
/// `limit` to keep the empty-state render cheap on large folders (the user
/// can scroll the list, but we don't need to ship every clip's metadata for
/// a 4-row preview). Returns an empty list on any I/O error so the Recent
/// view degrades to Hero rather than failing the load path.
#[tauri::command]
pub fn storage_list_replays(
    app: AppHandle,
    limit: Option<u32>,
) -> Result<Vec<SavedReplay>, String> {
    let save_dir = crate::replay::load_save_dir(&app);
    let Ok(entries) = std::fs::read_dir(&save_dir) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<SavedReplay> = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_video_file(&name) {
            continue;
        }
        let modified_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(SavedReplay {
            path: entry.path().to_string_lossy().into_owned(),
            name,
            size_bytes: meta.len(),
            modified_secs,
        });
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.modified_secs));
    if let Some(n) = limit {
        out.truncate(n as usize);
    }
    Ok(out)
}

#[tauri::command]
pub fn storage_usage(app: AppHandle) -> Result<StorageUsage, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let save_dir = crate::replay::load_save_dir(&app);
    let proxies = app_data.join("proxies");

    let cache_bytes = dir_size_recursive(&proxies);
    let (saved_replays_bytes, saved_replays_count) = saved_replays_stats(&save_dir);

    // `other` = everything else in app-data (diag log, project JSONs, the
    // opened-replays index, save-dir pref). The save_dir lives outside
    // app-data by default (Videos/Clippy Replays) so its bytes are a
    // separate root, not double-counted.
    let app_data_total = dir_size_recursive(&app_data);
    let other_bytes = app_data_total.saturating_sub(cache_bytes);
    let total_bytes = saved_replays_bytes
        .saturating_add(cache_bytes)
        .saturating_add(other_bytes);

    Ok(StorageUsage {
        save_dir: save_dir.to_string_lossy().into_owned(),
        saved_replays_bytes,
        saved_replays_count,
        cache_bytes,
        other_bytes,
        total_bytes,
    })
}

/// Where the opened-replays index lives. JSON array of absolute (canonical
/// where available) paths the user has loaded into the editor at least once.
/// The pruner consults this set so the "delete unkept replays" policy spares
/// anything the user has bothered to look at.
fn opened_index_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("opened-replays.json"))
}

fn read_opened_index(path: &Path) -> HashSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashSet::new();
    };
    let Ok(v) = serde_json::from_str::<Vec<String>>(&text) else {
        return HashSet::new();
    };
    v.into_iter().collect()
}

fn write_opened_index(path: &Path, set: &HashSet<String>) -> Result<(), String> {
    let v: Vec<&String> = set.iter().collect();
    let json = serde_json::to_string(&v).map_err(|e| format!("encode opened-index: {e}"))?;
    // Write to a sibling .tmp then rename — POSIX rename is atomic and
    // Windows' MoveFileEx with default flags is close enough that a partial
    // crash mid-write can never leave the index in a torn state.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write opened-index tmp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename opened-index: {e}"))?;
    Ok(())
}

/// Frontend notifies the backend that the user just loaded a replay into the
/// editor. Used by the prune policy to spare "kept" clips from the
/// unkept-cleanup pass. Idempotent — repeat calls are no-ops.
#[tauri::command]
pub fn storage_mark_opened(app: AppHandle, path: String) -> Result<(), String> {
    let idx_path = opened_index_path(&app)?;
    let mut set = read_opened_index(&idx_path);
    // Canonicalize when possible so paths compare consistently even if the
    // caller passed a relative or symlink'd form. Fall back to the raw
    // string when canonicalize fails (e.g. the file was deleted between
    // open and this call).
    let key = std::fs::canonicalize(&path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(path);
    if set.insert(key) {
        write_opened_index(&idx_path, &set)?;
    }
    Ok(())
}

/// Result payload for `storage_prune`. `dry_run: true` returns the list of
/// paths that *would* be removed without actually removing them, so the
/// frontend can prompt the user before a destructive cap reduction.
#[derive(Serialize, Default)]
pub struct StoragePruneResult {
    pub freed_bytes: u64,
    pub removed_count: u64,
    pub removed_paths: Vec<String>,
    pub dry_run: bool,
}

/// Apply the user's cap + unkept-cleanup policy to the save directory.
///   - `cap_bytes`: if Some(n), saved-replays bytes will be brought ≤ n by
///     deleting oldest-first. None = no cap.
///   - `unkept_max_days`: if Some(d), files not in the opened-index whose
///     mtime is older than d days are deleted regardless of cap. None = skip
///     unkept cleanup.
///   - `dry_run`: when true, returns what would be removed without touching
///     disk. Frontend uses this for the "this will delete N files" prompt.
///
/// Safety: skips video files mtime'd within the last 30 seconds — those may
/// still be in flight from `replay_save`'s mux. (`replay_save` writes through
/// a temp dir, so files in the save dir are normally complete, but the
/// guard rail costs nothing and catches edge cases.)
#[tauri::command]
pub fn storage_prune(
    app: AppHandle,
    cap_bytes: Option<u64>,
    unkept_max_days: Option<u64>,
    dry_run: bool,
) -> Result<StoragePruneResult, String> {
    let save_dir = crate::replay::load_save_dir(&app);
    let idx_path = opened_index_path(&app)?;
    let opened = read_opened_index(&idx_path);
    let now = SystemTime::now();

    // Snapshot candidates with (path, size, mtime). Anything that fails to
    // stat is silently skipped — the user can re-run after fixing perms.
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&save_dir) else {
        return Ok(StoragePruneResult {
            dry_run,
            ..Default::default()
        });
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name();
        if !is_video_file(&name.to_string_lossy()) {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if now
            .duration_since(mtime)
            .map(|d| d < Duration::from_secs(30))
            .unwrap_or(false)
        {
            continue;
        }
        files.push((entry.path(), meta.len(), mtime));
    }

    let mut victims: Vec<(PathBuf, u64)> = Vec::new();

    // Pass 1 — unkept cleanup. Spares anything the user has loaded into the
    // editor at least once (via storage_mark_opened).
    if let Some(days) = unkept_max_days {
        if let Some(cutoff) = now.checked_sub(Duration::from_secs(days.saturating_mul(86_400))) {
            files.retain(|(p, sz, mtime)| {
                let raw = p.to_string_lossy().into_owned();
                let canon = std::fs::canonicalize(p)
                    .ok()
                    .map(|c| c.to_string_lossy().into_owned());
                let was_opened = opened.contains(&raw)
                    || canon.as_ref().map(|c| opened.contains(c)).unwrap_or(false);
                if !was_opened && *mtime < cutoff {
                    victims.push((p.clone(), *sz));
                    false
                } else {
                    true
                }
            });
        }
    }

    // Pass 2 — cap enforcement. Oldest-first from what remains after Pass 1
    // (so unkept-cleanup'd files don't get double-counted toward "still
    // over cap"). Sized comparison is on saved_replays_bytes only — the
    // cap policy can't delete cache or other buckets.
    if let Some(cap) = cap_bytes {
        files.sort_by_key(|(_, _, mtime)| *mtime);
        let mut current_total: u64 = files.iter().map(|(_, sz, _)| *sz).sum();
        for (p, sz, _) in &files {
            if current_total <= cap {
                break;
            }
            victims.push((p.clone(), *sz));
            current_total = current_total.saturating_sub(*sz);
        }
    }

    let mut freed: u64 = 0;
    let mut paths: Vec<String> = Vec::with_capacity(victims.len());
    for (p, sz) in &victims {
        if dry_run {
            freed = freed.saturating_add(*sz);
            paths.push(p.to_string_lossy().into_owned());
            continue;
        }
        if std::fs::remove_file(p).is_ok() {
            freed = freed.saturating_add(*sz);
            paths.push(p.to_string_lossy().into_owned());
            // Best-effort cleanup: drop the entry from the opened index too,
            // so a future file at the same path doesn't inherit "kept" state.
            // We don't fail the whole prune on index-write errors — the file
            // is already gone from disk.
        }
    }

    let removed_count = paths.len() as u64;
    Ok(StoragePruneResult {
        freed_bytes: freed,
        removed_count,
        removed_paths: paths,
        dry_run,
    })
}

/// Auto-prune cache files that haven't been touched in `days` days. Runs on
/// app start in a background thread so a slow disk doesn't block startup.
pub(crate) fn prune_old_cache(dir: PathBuf, days: u64) {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(days * 24 * 60 * 60));
    let Some(cutoff) = cutoff else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
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
