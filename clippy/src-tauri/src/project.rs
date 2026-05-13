use std::path::PathBuf;
use tauri::AppHandle;

use crate::proxy::{proxy_cache_key, proxy_dir};

// ---- Project state persistence ----
//
// Each source video gets a sidecar JSON in the proxy cache dir, named by the
// same SHA-256(path+mtime+size) fingerprint as its proxy/waveform. Stores the
// regions array verbatim; that's the only thing that meaningfully survives
// across sessions (everything else is either app-global or transient).

fn project_path(app: &AppHandle, src_path: &str) -> Result<PathBuf, String> {
    let key = proxy_cache_key(src_path)?;
    Ok(proxy_dir(app)?.join(format!("{}.project.json", &key[..32])))
}

/// Cap on project sidecar size. The state we save is regions + track mix +
/// colors + names — for any realistic session this is well under 50 KB.
/// Hard cap at 1 MB so a tampered or corrupt sidecar can't OOM the renderer
/// when we round-trip it back through IPC. (serde_json's default recursion
/// limit of 128 already prevents a deep-nesting stack-blow on its own.)
const PROJECT_FILE_MAX_BYTES: u64 = 1_000_000;

#[tauri::command]
pub fn load_project(app: AppHandle, src_path: String) -> Result<Option<serde_json::Value>, String> {
    let path = project_path(&app, &src_path)?;
    if !path.exists() {
        return Ok(None);
    }
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if meta.len() > PROJECT_FILE_MAX_BYTES {
        return Err(format!(
            "project file unreasonably large ({} bytes); refusing to load",
            meta.len()
        ));
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok(Some(v))
}

#[tauri::command]
pub fn save_project(
    app: AppHandle,
    src_path: String,
    state: serde_json::Value,
) -> Result<(), String> {
    let path = project_path(&app, &src_path)?;
    let bytes = serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?;
    // Atomic write: serialize to a sibling tempfile, then rename over the
    // target. std::fs::rename on Windows resolves to MoveFileExW with
    // REPLACE_EXISTING + WRITE_THROUGH, and on POSIX rename is atomic — either
    // way, a crash mid-write leaves the previous sidecar intact rather than
    // truncating it. Without this, a power loss between truncating `path`
    // and finishing the write loses every region, color, mix override, and
    // track name for that source on next load.
    let tmp_path: PathBuf = {
        let mut s = path.clone().into_os_string();
        s.push(".tmp");
        s.into()
    };
    std::fs::write(&tmp_path, &bytes)
        .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        // Best-effort: drop the tempfile so the next save isn't blocked by
        // a stale `<...>.json.tmp` lying around. The next save will retry.
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("rename to {}: {e}", path.display()));
    }
    Ok(())
}
