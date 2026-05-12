pub mod replay;

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use axum::body::Body;
use axum::extract::{Query, State as AxumState};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::io::ReaderStream;

// ----- localhost media server -----
//
// Chromium plays HTTP files much more reliably than custom protocols, so we
// stand up a tiny in-process HTTP server on a random localhost port and point
// the <video> element at it. The server:
//   * requires a per-session token (so other origins in the webview can't
//     guess the URL),
//   * only serves files in an allowlist (paths the user has explicitly
//     registered through register_file_url), and
//   * supports byte-range requests, which is what kills the asset:// hitches.

#[derive(Clone)]
struct ServerState {
    token: String,
    port: u16,
    allowlist: Arc<AsyncMutex<HashSet<PathBuf>>>,
}

struct ServerInfo {
    port: u16,
    state: ServerState,
}

// ----- diagnostic log -----

const DIAG_CAP: usize = 200;

/// In-memory ring buffer of timestamped log entries. Bounded so it can't
/// grow unboundedly over a long session. Never written to disk and never sent
/// anywhere — the user copies it explicitly via the "Copy diagnostics" button.
struct DiagLog(Arc<Mutex<VecDeque<String>>>);

impl DiagLog {
    fn new() -> Self {
        DiagLog(Arc::new(Mutex::new(VecDeque::with_capacity(DIAG_CAP))))
    }
}

/// (Re)register the global save-replay hotkey.
///
/// `shortcut_str` is the Tauri `Shortcut::from_str` format ("Alt+F10",
/// "Ctrl+Shift+S", etc.). Any previously-registered global shortcut is removed
/// first so this is safe to call repeatedly when the user rebinds.
fn register_save_hotkey(app: &AppHandle, shortcut_str: &str) -> Result<(), String> {
    use std::str::FromStr;
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

    let parsed = Shortcut::from_str(shortcut_str)
        .map_err(|e| format!("parse '{shortcut_str}': {e}"))?;

    // We only ever have the one global shortcut for now — clear the slate.
    let _ = app.global_shortcut().unregister_all();

    let app_handle = app.clone();
    let shortcut_label = shortcut_str.to_string();
    app.global_shortcut()
        .on_shortcut(parsed, move |_app, _sc, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let handle = app_handle.clone();
            // Surface that the OS actually delivered the keypress so we
            // can tell "hotkey didn't register" apart from "hotkey fired
            // but save flow choked" when triaging silent-failure reports.
            diag(
                &handle,
                format!("[replay] save hotkey FIRED ({shortcut_label}) — invoking save"),
            );
            tauri::async_runtime::spawn(async move {
                match replay::save_active(&handle).await {
                    Ok(result) => {
                        diag(
                            &handle,
                            format!(
                                "replay save OK — window=\"{}\" → {}",
                                result.window_title, result.path
                            ),
                        );
                        let _ = handle.emit("replay://saved", &result.path);
                    }
                    Err(e) => {
                        diag(&handle, format!("replay save FAILED: {e}"));
                        let _ = handle.emit("replay://save-error", e);
                    }
                }
            });
        })
        .map_err(|e| format!("register: {e}"))?;
    Ok(())
}

/// Tauri command: rebind the save-replay global hotkey.
#[tauri::command]
fn replay_set_save_hotkey(app: AppHandle, shortcut: String) -> Result<(), String> {
    register_save_hotkey(&app, &shortcut)?;
    diag(&app, format!("[replay] save hotkey rebound to {shortcut}"));
    Ok(())
}

/// Whether closing the main window should hide to the system tray instead
/// of exiting. Frontend keeps a localStorage mirror; this is the canonical
/// runtime copy the window-close handler reads.
struct HideOnClose(std::sync::atomic::AtomicBool);

#[tauri::command]
fn set_hide_on_close(state: tauri::State<'_, HideOnClose>, enabled: bool) {
    state.0.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

/// Where saved replays land. The coordinator's `finish_save` reads this on
/// every save. Default is `Videos/Clippy Replays` (computed at startup);
/// user can change it via `replay_set_save_dir` and the choice is persisted
/// to `<appdata>/save_dir.txt`.
pub struct ReplaySaveDir(pub Mutex<PathBuf>);

/// Re-export of `epoch_to_ymd` for replay::mod.rs to build filesystem-safe
/// timestamp slugs without duplicating the civil-from-days logic.
pub(crate) fn epoch_to_ymd_for_filename(secs: u64) -> (i32, u32, u32) {
    epoch_to_ymd(secs)
}

/// Opt-in switch for "verbose" diag logging. When OFF (default), the
/// coordinator redacts non-game window titles so a copy-pasted diag report
/// never contains a sensitive browser tab / document name. Enable only when
/// actively reproducing a window-routing bug.
pub(crate) struct DiagVerbose(pub std::sync::atomic::AtomicBool);

impl DiagVerbose {
    pub fn enabled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[tauri::command]
fn set_diag_verbose(state: tauri::State<'_, DiagVerbose>, enabled: bool) {
    state.0.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

/// Convert a Unix epoch (seconds) to a calendar (year, month, day) tuple in
/// UTC. Pure-Rust adaptation of Howard Hinnant's `civil_from_days`. Avoids
/// dragging in `chrono`/`time` for the few timestamps we render.
fn epoch_to_ymd(secs: u64) -> (i32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    (y as i32, m as u32, d as u32)
}

/// Format an absolute UTC `YYYY-MM-DD HH:MM:SS` string from a Unix epoch.
/// Used everywhere diag entries and snapshot headers need a timestamp.
pub(crate) fn fmt_utc(secs: u64) -> String {
    let (y, mo, d) = epoch_to_ymd(secs);
    let h = (secs / 3600) % 24;
    let mi = (secs / 60) % 60;
    let s = secs % 60;
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Snapshot the current diag ring as a single newline-separated string.
/// Used by `persist_diag_log` and `get_diagnostics`.
fn diag_snapshot(app: &AppHandle) -> String {
    let arc = Arc::clone(&app.state::<DiagLog>().0);
    let buf = match arc.lock() {
        Ok(b) => b,
        Err(e) => e.into_inner(),
    };
    let mut out = String::with_capacity(buf.iter().map(|s| s.len() + 1).sum());
    for entry in buf.iter() {
        out.push_str(entry);
        out.push('\n');
    }
    out
}

/// Write the current diag ring to `<appdata>/diagnostics.log`. Called from
/// the `RunEvent::Exit` hook so a graceful quit always preserves the most
/// recent ~200 events for post-mortem inspection. Best-effort; logs to
/// stderr on failure but never panics.
fn persist_diag_log(app: &AppHandle) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let snapshot = diag_snapshot(app);
    if snapshot.is_empty() {
        return;
    }
    let dir = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[clippy] persist_diag_log: app_data_dir: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[clippy] persist_diag_log: create_dir_all: {e}");
        return;
    }
    let path = dir.join("diagnostics.log");
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let header = format!(
        "===== Clippy v{} session ended {} UTC =====\n",
        env!("CARGO_PKG_VERSION"),
        fmt_utc(now_secs)
    );
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            let _ = f.write_all(header.as_bytes());
            let _ = f.write_all(snapshot.as_bytes());
            let _ = f.write_all(b"\n");
        }
        Err(e) => {
            eprintln!("[clippy] persist_diag_log: open {}: {e}", path.display());
        }
    }
}

/// Append a timestamped entry. The lock is held only for a VecDeque push so
/// this is effectively non-blocking in any context.
pub(crate) fn diag(app: &AppHandle, msg: impl std::fmt::Display) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = format!("[{}] {}", fmt_utc(secs), msg);
    let arc = Arc::clone(&app.state::<DiagLog>().0);
    let mut buf = match arc.lock() {
        Ok(b) => b,
        Err(e) => e.into_inner(),
    };
    if buf.len() >= DIAG_CAP {
        buf.pop_front();
    }
    buf.push_back(entry);
}

/// Strip the directory from a path so logs never contain the user's home
/// directory or other path components. Returns the filename only.
fn basename(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// Truncate a string to `max` bytes for log entries, appending "…" if cut.
fn trunc(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

#[derive(Deserialize)]
struct ServeQuery {
    token: String,
    p: String,
}

fn generate_session_token() -> String {
    // 32 bytes from the OS CSPRNG → 64-char hex. Predictable inputs (time, pid)
    // would let any local process guess the token and reach the media server.
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS RNG unavailable");
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

async fn serve_file(
    AxumState(state): AxumState<ServerState>,
    Query(q): Query<ServeQuery>,
    headers: HeaderMap,
) -> Response {
    // Defeat DNS rebinding: only accept Host headers that target our exact
    // loopback port. A remote site that learns the port can still send the
    // request, but the browser-supplied Host will be the attacker's domain.
    let expected_host_v4 = format!("127.0.0.1:{}", state.port);
    let expected_host_localhost = format!("localhost:{}", state.port);
    let host_ok = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h == expected_host_v4 || h == expected_host_localhost)
        .unwrap_or(false);
    if !host_ok {
        return (StatusCode::FORBIDDEN, "bad host").into_response();
    }
    if q.token != state.token {
        return (StatusCode::FORBIDDEN, "bad token").into_response();
    }
    let path = PathBuf::from(&q.p);
    {
        let allow = state.allowlist.lock().await;
        if !allow.contains(&path) {
            return (StatusCode::FORBIDDEN, "not allowed").into_response();
        }
    }
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "open failed").into_response(),
    };
    let total = match file.metadata().await {
        Ok(m) => m.len(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "metadata").into_response(),
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mime_str = mime.essence_str().to_string();

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some(range) = range_header {
        if let Some(rest) = range.strip_prefix("bytes=") {
            let parts: Vec<&str> = rest.split('-').collect();
            if parts.len() == 2 {
                let start: u64 = parts[0].parse().unwrap_or(0);
                let end: u64 = if parts[1].is_empty() {
                    total.saturating_sub(1)
                } else {
                    parts[1]
                        .parse::<u64>()
                        .unwrap_or(total.saturating_sub(1))
                        .min(total.saturating_sub(1))
                };
                if total == 0 || start > end {
                    return Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{}", total))
                        .body(Body::empty())
                        .unwrap();
                }
                let chunk_len = end - start + 1;
                if file.seek(SeekFrom::Start(start)).await.is_err() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "seek").into_response();
                }
                let limited = file.take(chunk_len);
                let stream = ReaderStream::new(limited);
                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, mime_str)
                    .header(header::CONTENT_LENGTH, chunk_len.to_string())
                    .header(
                        header::CONTENT_RANGE,
                        format!("bytes {}-{}/{}", start, end, total),
                    )
                    .header(header::ACCEPT_RANGES, "bytes")
                    // CORS so MediaElementSource can read samples for WebAudio.
                    // Token + Host already gate access; CORS is the browser's.
                    .header("access-control-allow-origin", "*")
                    .header("access-control-expose-headers", "Content-Range, Content-Length, Accept-Ranges")
                    .body(Body::from_stream(stream))
                    .unwrap();
            }
        }
    }

    let stream = ReaderStream::new(file);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_str)
        .header(header::CONTENT_LENGTH, total.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header("access-control-allow-origin", "*")
        .header("access-control-expose-headers", "Content-Range, Content-Length, Accept-Ranges")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// CORS preflight handler. Browsers send OPTIONS with `Access-Control-Request-*`
/// headers before media element fetches that have a non-simple Origin/cred
/// configuration. Respond with permissive headers (token + Host already gate
/// real access).
async fn serve_options() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-methods", "GET, OPTIONS")
        .header("access-control-allow-headers", "Range, Content-Type")
        .header("access-control-max-age", "86400")
        .body(Body::empty())
        .unwrap()
}

/// Return the size of a file in bytes. Used by the post-export toast.
#[tauri::command]
fn file_size(path: String) -> Result<u64, String> {
    std::fs::metadata(&path)
        .map(|m| m.len())
        .map_err(|e| format!("file_size {}: {}", path, e))
}

/// Sum the bytes used by every cached proxy/remux/waveform/project file.
#[tauri::command]
fn cache_size(app: AppHandle) -> Result<u64, String> {
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
fn clear_cache(app: AppHandle) -> Result<u64, String> {
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
struct StorageSummary {
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
fn storage_summary(app: AppHandle) -> Result<StorageSummary, String> {
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

/// Delete the persisted diagnostics.log. The in-memory ring buffer is
/// untouched — it'll be re-flushed on the next graceful exit. Useful when
/// a user wants to reset state before reproducing a bug.
#[tauri::command]
fn clear_diagnostics_log(app: AppHandle) -> Result<u64, String> {
    let path = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("diagnostics.log");
    if !path.exists() {
        return Ok(0);
    }
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    Ok(size)
}

/// Auto-prune cache files that haven't been touched in `days` days. Runs on
/// app start in a background thread so a slow disk doesn't block startup.
fn prune_old_cache(dir: PathBuf, days: u64) {
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

/// File path passed on the command line (Windows "Open with" or drag-on-exe
/// invokes us as `Clippy.exe "C:\path\video.mp4"`). Parsed once at startup,
/// then taken (cleared) by the frontend on first mount.
struct InitialPath(Mutex<Option<String>>);

fn parse_initial_path() -> Option<String> {
    const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "webm", "m4v", "avi"];
    for arg in std::env::args().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        let lower = arg.to_lowercase();
        let ext_ok = VIDEO_EXTS
            .iter()
            .any(|e| lower.ends_with(&format!(".{}", e)));
        if ext_ok && std::path::Path::new(&arg).is_file() {
            return Some(arg);
        }
    }
    None
}

#[tauri::command]
fn get_initial_path(state: State<'_, InitialPath>) -> Option<String> {
    state.0.lock().ok().and_then(|mut g| g.take())
}

/// Open the OS file manager with the given file selected. Windows-specific:
/// uses explorer.exe with /select,. On other platforms we'd fall back to opening
/// the parent directory.
#[tauri::command]
fn reveal_in_folder(path: String) -> Result<(), String> {
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

#[tauri::command]
async fn register_file_url(
    app: AppHandle,
    state: State<'_, ServerInfo>,
    path: String,
) -> Result<String, String> {
    // Scoped allowlist: a renderer-side bug (or a future XSS) must not be able
    // to coerce the backend into serving arbitrary local files like
    // NTUSER.DAT or SSH keys. Two cases are legitimate:
    //   1. Files we generated under the proxy cache dir (remux / encode /
    //      extracted track outputs).
    //   2. Files generate_proxy already trusted on this session — for the
    //      Direct strategy, that's the original source MP4.
    // Anything else is rejected outright.
    let canonical = std::fs::canonicalize(&path)
        .map_err(|e| format!("canonicalize failed: {}", e))?;
    let proxies = std::fs::canonicalize(proxy_dir(&app)?)
        .map_err(|e| format!("proxy dir canonicalize failed: {}", e))?;
    let in_proxies = canonical.starts_with(&proxies);
    let already_trusted = state.state.allowlist.lock().await.contains(&canonical);
    if !in_proxies && !already_trusted {
        return Err("path not allowed".into());
    }
    state.state.allowlist.lock().await.insert(canonical.clone());
    let encoded = urlencoding::encode(&canonical.to_string_lossy()).into_owned();
    Ok(format!(
        "http://127.0.0.1:{}/vid?token={}&p={}",
        state.port, state.state.token, encoded
    ))
}

/// Insert a path into the media-server allowlist directly. Used by backend
/// commands (generate_proxy's Direct strategy) to pre-trust a source path
/// that register_file_url would otherwise reject for being outside proxy_dir.
async fn allowlist_trust(state: &ServerState, path: &std::path::Path) -> Result<(), String> {
    let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    state.allowlist.lock().await.insert(canonical);
    Ok(())
}

// Cached encoder that we've actually verified works on this machine (set after a successful pass).
static WORKING_ENCODER: Mutex<Option<&'static str>> = Mutex::new(None);

async fn encoder_chain(app: &AppHandle) -> Vec<&'static str> {
    // If we already know what works on this box, skip the others.
    if let Some(working) = *WORKING_ENCODER.lock().unwrap() {
        if working == "libx264" {
            return vec!["libx264"];
        }
        return vec![working, "libx264"];
    }
    // First time — detect what's in the ffmpeg build, then try them in priority order.
    let priority: [&'static str; 3] = ["h264_nvenc", "h264_amf", "h264_qsv"];
    let mut chain: Vec<&'static str> = vec![];
    if let Ok(sidecar) = app.shell().sidecar("ffmpeg") {
        if let Ok(out) = sidecar.args(["-hide_banner", "-encoders"]).output().await {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).to_string();
                for enc in priority.iter() {
                    if s.contains(*enc) {
                        chain.push(*enc);
                    }
                }
            }
        }
    }
    chain.push("libx264");
    chain
}

/// Audio bitrate (in bps) used for size-targeted re-encoded exports. Subtracted
/// from the size budget when calculating target video bitrate.
const SIZED_AUDIO_BPS: u64 = 96_000;

/// Encoder args for a fixed-bitrate (CBR-ish) re-encode targeting a specific
/// video kbps, used by the Discord-size export path.
fn encoder_args_sized(encoder: &str, video_kbps: u64) -> Vec<String> {
    let bv = format!("{}k", video_kbps);
    let maxrate = format!("{}k", video_kbps);
    let bufsize = format!("{}k", video_kbps * 2);
    match encoder {
        "h264_nvenc" => vec![
            "-c:v".into(), "h264_nvenc".into(),
            "-preset".into(), "p4".into(),
            "-tune".into(), "ll".into(),
            "-rc".into(), "cbr".into(),
            "-b:v".into(), bv,
            "-maxrate".into(), maxrate,
            "-bufsize".into(), bufsize,
        ],
        "h264_amf" => vec![
            "-c:v".into(), "h264_amf".into(),
            "-quality".into(), "speed".into(),
            "-rc".into(), "cbr".into(),
            "-b:v".into(), bv,
            "-maxrate".into(), maxrate,
            "-bufsize".into(), bufsize,
        ],
        "h264_qsv" => vec![
            "-c:v".into(), "h264_qsv".into(),
            "-preset".into(), "veryfast".into(),
            "-b:v".into(), bv,
            "-maxrate".into(), maxrate,
            "-bufsize".into(), bufsize,
        ],
        _ => vec![
            "-c:v".into(), "libx264".into(),
            "-preset".into(), "veryfast".into(),
            "-b:v".into(), bv,
            "-maxrate".into(), maxrate,
            "-bufsize".into(), bufsize,
        ],
    }
}

/// Compute the video bitrate (bps) that should hit a target size in MB for a
/// given clip duration, leaving a small safety margin and reserving the audio
/// budget. Floors at 200 kbps to avoid outputs that look like a smear.
fn target_video_bitrate_bps(target_mb: f64, duration_secs: f64) -> u64 {
    if duration_secs <= 0.0 {
        return 200_000;
    }
    let safety = 0.95_f64;
    let target_bytes = target_mb * 1024.0 * 1024.0 * safety;
    let audio_bytes = (SIZED_AUDIO_BPS as f64) / 8.0 * duration_secs;
    let video_bytes = (target_bytes - audio_bytes).max(25_000.0);
    let bps = (video_bytes * 8.0 / duration_secs) as u64;
    bps.max(200_000)
}

fn encoder_args(encoder: &str) -> Vec<&'static str> {
    match encoder {
        "h264_nvenc" => vec![
            "-c:v", "h264_nvenc",
            "-preset", "p4",
            "-tune", "ll",
            "-rc", "vbr",
            "-cq", "28",
            "-b:v", "0",
        ],
        "h264_amf" => vec![
            "-c:v", "h264_amf",
            "-quality", "speed",
            "-rc", "cqp",
            "-qp_i", "28",
            "-qp_p", "28",
        ],
        "h264_qsv" => vec![
            "-c:v", "h264_qsv",
            "-preset", "veryfast",
            "-global_quality", "28",
        ],
        _ => vec![
            "-c:v", "libx264",
            "-preset", "veryfast",
            "-crf", "28",
        ],
    }
}

/// High-quality re-encode (visually lossless-ish). Used for crop+no-limit
/// exports where the user expects the cropped output to look the same as the
/// original frame. CQ/CRF ~20 is the conventional "indistinguishable from
/// source" setting at typical screen-recording bitrates.
fn encoder_args_high_quality(encoder: &str) -> Vec<&'static str> {
    match encoder {
        "h264_nvenc" => vec![
            "-c:v", "h264_nvenc",
            "-preset", "p5",
            "-rc", "vbr",
            "-cq", "20",
            "-b:v", "0",
        ],
        "h264_amf" => vec![
            "-c:v", "h264_amf",
            "-quality", "balanced",
            "-rc", "cqp",
            "-qp_i", "20",
            "-qp_p", "20",
        ],
        "h264_qsv" => vec![
            "-c:v", "h264_qsv",
            "-preset", "medium",
            "-global_quality", "20",
        ],
        _ => vec![
            "-c:v", "libx264",
            "-preset", "medium",
            "-crf", "20",
        ],
    }
}

/// Source-pixel crop rectangle. Frontend supplies these in source coordinates;
/// backend just feeds them to the ffmpeg `crop` filter.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
struct Crop {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Crop {
    fn to_filter(self) -> String {
        format!("crop={}:{}:{}:{}", self.w, self.h, self.x, self.y)
    }
}

/// Per-region export descriptor for the concat commands. Each region carries
/// its own optional crop + speed + audio mix, applied in stage 1 (cut).
/// Normalize is a per-export setting handled at the concat level.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct RegionExport {
    in_secs: f64,
    out_secs: f64,
    crop: Option<Crop>,
    speed: Option<f64>,
    /// Per-region audio mix override (track gains). When None, the function-
    /// level track_mix is used as the fallback for this region.
    #[serde(default)]
    mix: Option<Vec<TrackGain>>,
}

impl RegionExport {
    /// Effective duration after speed change. Used for progress + size math.
    fn effective_duration(&self) -> f64 {
        let raw = (self.out_secs - self.in_secs).max(0.0);
        let s = self.speed.unwrap_or(1.0);
        if s <= 0.0 { raw } else { raw / s }
    }
}

/// `atempo` only supports 0.5..2.0 per filter instance. Chain multiple to
/// reach the requested speed (0.25 → 0.5,0.5; 4.0 → 2.0,2.0).
fn atempo_chain(speed: f64) -> String {
    if (speed - 1.0).abs() < 1e-6 {
        return "atempo=1.0".to_string();
    }
    let mut s = speed.max(0.0001);
    let mut parts: Vec<String> = Vec::new();
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s *= 2.0;
    }
    while s > 2.0 {
        parts.push("atempo=2.0".to_string());
        s /= 2.0;
    }
    parts.push(format!("atempo={:.4}", s));
    parts.join(",")
}

/// loudnorm target chosen to match conversational/streaming loudness (≈ -16
/// LUFS, -1.5 dBTP). Single-pass; not as precise as 2-pass but plenty for
/// "make it actually audible on Discord".
const LOUDNORM_FILTER: &str = "loudnorm=I=-16:TP=-1.5:LRA=11";

/// Per-track gain in the user's audio mix. `volume` is a linear multiplier:
/// 0.0 = muted, 1.0 = source level, 2.0 = +6 dB. Exports drop tracks whose
/// effective volume rounds to zero, so muting a track is free.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct TrackGain {
    index: u32,
    volume: f64,
}

// (build_track_mix_filter removed — its logic now lives inline inside
// build_audio_filter_complex below for cleaner composition with the
// post-mix audio filter chain.)

/// Build the `-vf` chain (crop + speed). Returns "" when there's nothing to do.
fn build_video_filter(crop: Option<Crop>, speed: Option<f64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = crop {
        parts.push(c.to_filter());
    }
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 && s > 0.0 {
            parts.push(format!("setpts={:.6}*PTS", 1.0 / s));
        }
    }
    parts.join(",")
}

/// Build the audio post-mix filter chain (speed atempo + normalize loudnorm).
/// Applied AFTER track mixing, so the user's volume sliders drive the input
/// to loudnorm rather than fighting it.
fn build_audio_post_mix_filters(speed: Option<f64>, normalize: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 && s > 0.0 {
            parts.push(atempo_chain(s));
        }
    }
    if normalize {
        parts.push(LOUDNORM_FILTER.to_string());
    }
    parts.join(",")
}

/// Compose the audio half of `-filter_complex` from a track mix + post-mix
/// filters. Returns `(Some(graph), "[aout]")` when audio needs processing or
/// `(None, "0:a:0?")` when caller can use the source's first audio track
/// straight through (the existing fast-path).
///
/// `total_tracks` validates indices coming from the frontend.
fn build_audio_filter_complex(
    track_mix: &[TrackGain],
    total_tracks: usize,
    post_mix_filters: &str,
) -> (Option<String>, String) {
    let is_default_mix = track_mix.is_empty()
        || (track_mix.len() == total_tracks
            && track_mix.iter().all(|t| (t.volume - 1.0).abs() < 1e-6));

    // Fast path only for single-stream sources with nothing to process.
    // Multi-track sources must still amix all streams even at default gain —
    // returning "0:a:0?" would silently drop every track past the first.
    if is_default_mix && post_mix_filters.is_empty() && total_tracks <= 1 {
        return (None, "0:a:0?".to_string());
    }

    // Build the active track list. For a default mix with multiple source
    // tracks, all are active at unity; the explicit mix controls the rest.
    let active: Vec<(usize, f64)> = if is_default_mix {
        (0..total_tracks).map(|i| (i, 1.0)).collect()
    } else {
        track_mix
            .iter()
            .filter(|t| t.volume > 0.001 && (t.index as usize) < total_tracks)
            .map(|t| (t.index as usize, t.volume))
            .collect()
    };

    let mix_output = if post_mix_filters.is_empty() { "[aout]" } else { "[m]" };
    let mut parts: Vec<String> = Vec::new();

    let mix_tag: String = if active.is_empty() {
        // All tracks muted — synthesize silence so the muxer still has audio.
        parts.push(format!(
            "anullsrc=channel_layout=stereo:sample_rate=48000{}",
            mix_output
        ));
        mix_output.to_string()
    } else if active.len() == 1 {
        let (idx, vol) = active[0];
        if (vol - 1.0).abs() < 1e-6 {
            // Unity gain, single active stream — route directly into post-mix.
            // When there's ALSO no post-mix work to do, short-circuit to a
            // direct stream map: no filter graph at all. Previously we
            // returned `Some("")` + `"[aout]"` here, which ffmpeg correctly
            // rejected with "No filters specified in the graph description"
            // (the [aout] label was never defined). Reproduced when a user
            // muted all but one track or had a single source stream + non-
            // default mix metadata.
            if post_mix_filters.is_empty() {
                return (None, format!("0:a:{}?", idx));
            }
            format!("[0:a:{}]", idx)
        } else {
            parts.push(format!("[0:a:{}]volume={:.4}{}", idx, vol, mix_output));
            mix_output.to_string()
        }
    } else {
        let mut tags = Vec::new();
        for (idx, vol) in &active {
            let tag = format!("a{}", idx);
            parts.push(format!("[0:a:{}]volume={:.4}[{}]", idx, vol, tag));
            tags.push(format!("[{}]", tag));
        }
        parts.push(format!(
            "{}amix=inputs={}:duration=longest:dropout_transition=0:normalize=0{}",
            tags.join(""),
            active.len(),
            mix_output
        ));
        mix_output.to_string()
    };

    if !post_mix_filters.is_empty() {
        parts.push(format!("{}{}[aout]", mix_tag, post_mix_filters));
    }

    (Some(parts.join(";")), "[aout]".to_string())
}

/// Returns true if any filter in the export forces a video re-encode. Crop
/// and speed change pixels/timestamps; normalize alone only touches audio.
fn forces_video_reencode(crop: Option<Crop>, speed: Option<f64>) -> bool {
    if crop.is_some() {
        return true;
    }
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 {
            return true;
        }
    }
    false
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct AudioTrack {
    /// Stream index *within the audio streams only* (0 = first audio track).
    /// This is what ffmpeg's `0:a:N` selector wants, NOT the absolute stream
    /// index, which differs across containers.
    index: usize,
    codec: String,
    channels: u32,
    /// Channel layout string from ffprobe (e.g. "stereo", "5.1"). Optional —
    /// some containers don't report it.
    layout: Option<String>,
    /// Title from stream metadata. SteelSeries Sonar / OBS often set this to
    /// "Game" / "Mic" / "Discord" etc; we surface it verbatim. None → fall
    /// back to "Track N+1" in the UI.
    title: Option<String>,
    language: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct VideoInfo {
    duration_secs: f64,
    width: u32,
    height: u32,
    fps: f64,
    video_codec: String,
    /// First audio codec (kept for back-compat). Use `audio_tracks` for the
    /// full list when handling multi-track sources.
    audio_codec: Option<String>,
    audio_tracks: Vec<AudioTrack>,
    container: String,
    bit_rate_bps: Option<u64>,
}

fn parse_rate(s: &str) -> f64 {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() == 2 {
        let num: f64 = parts[0].parse().unwrap_or(0.0);
        let den: f64 = parts[1].parse().unwrap_or(1.0);
        if den == 0.0 { 0.0 } else { num / den }
    } else {
        s.parse().unwrap_or(0.0)
    }
}

async fn probe_video_inner(app: &AppHandle, path: &str) -> Result<VideoInfo, String> {
    let output = app
        .shell()
        .sidecar("ffprobe")
        .map_err(|e| e.to_string())?
        .args([
            "-v", "error",
            "-print_format", "json",
            "-show_format",
            "-show_streams",
            path,
        ])
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;

    let format = json.get("format").ok_or("no format section")?;
    let duration_secs: f64 = format
        .get("duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let container = format
        .get("format_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let bit_rate_bps = format
        .get("bit_rate")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok());

    let streams = json
        .get("streams")
        .and_then(|v| v.as_array())
        .ok_or("no streams")?;

    let video_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"))
        .ok_or("no video stream")?;

    // Walk every audio stream and build the per-track list. Index here is
    // a-stream-relative (0..N), matching ffmpeg's `0:a:N` selector.
    let mut audio_tracks: Vec<AudioTrack> = Vec::new();
    for s in streams.iter() {
        if s.get("codec_type").and_then(|v| v.as_str()) != Some("audio") {
            continue;
        }
        let idx = audio_tracks.len();
        let codec = s.get("codec_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let channels = s.get("channels").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let layout = s.get("channel_layout").and_then(|v| v.as_str()).map(String::from);
        let tags = s.get("tags");
        let title = tags
            .and_then(|t| t.get("title"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let language = tags
            .and_then(|t| t.get("language"))
            .and_then(|v| v.as_str())
            .map(String::from);
        audio_tracks.push(AudioTrack {
            index: idx,
            codec,
            channels,
            layout,
            title,
            language,
        });
    }

    let width = video_stream
        .get("width")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let height = video_stream
        .get("height")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let video_codec = video_stream
        .get("codec_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let r_frame_rate = video_stream
        .get("r_frame_rate")
        .and_then(|v| v.as_str())
        .unwrap_or("0/1");
    let fps = parse_rate(r_frame_rate);
    let audio_codec = audio_tracks.first().map(|t| t.codec.clone());
    let info = VideoInfo {
        duration_secs,
        width,
        height,
        fps,
        video_codec: video_codec.clone(),
        audio_codec: audio_codec.clone(),
        audio_tracks: audio_tracks.clone(),
        container: container.clone(),
        bit_rate_bps,
    };
    diag(app, format!(
        "probe: {} → {}/{}, {}×{} @ {:.2}fps, {} audio track(s), {:.1}s",
        basename(path),
        video_codec,
        audio_codec.as_deref().unwrap_or("none"),
        width, height, fps,
        audio_tracks.len(),
        duration_secs,
    ));
    Ok(info)
}

#[tauri::command]
async fn probe_video(app: AppHandle, path: String) -> Result<VideoInfo, String> {
    probe_video_inner(&app, &path).await
}

#[derive(Serialize, Clone, Debug)]
struct ProxyProgress {
    progress: f64,
    elapsed_secs: f64,
    eta_secs: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
struct ProxyResult {
    play_path: String,
    cached: bool,
    strategy: String,
}

#[derive(Clone, Debug)]
enum Strategy {
    Direct,
    Remux,
    Encode,
}

fn classify_strategy(info: &VideoInfo) -> Strategy {
    let video_ok = matches!(info.video_codec.as_str(), "h264" | "hevc");
    let audio_ok = match &info.audio_codec {
        None => true,
        Some(c) => matches!(c.as_str(), "aac"),
    };
    if !video_ok || !audio_ok {
        return Strategy::Encode;
    }
    let lower = info.container.to_lowercase();
    let mp4_native = lower.split(',').any(|x| matches!(x.trim(), "mp4" | "mov" | "m4v" | "3gp" | "3g2"));
    if mp4_native {
        Strategy::Direct
    } else {
        Strategy::Remux
    }
}

fn proxy_cache_key(src_path: &str) -> Result<String, String> {
    let metadata = std::fs::metadata(src_path).map_err(|e| e.to_string())?;
    let mtime = metadata.modified().map_err(|e| e.to_string())?;
    let mtime_secs = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let size = metadata.len();
    let mut hasher = Sha256::new();
    hasher.update(src_path.as_bytes());
    hasher.update(mtime_secs.to_le_bytes());
    hasher.update(size.to_le_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn proxy_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("proxies");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Escape a path for ffmpeg's concat-demuxer text format ("file '...'").
/// Rejects paths containing newline/CR — without this, a crafted filename
/// like `evil\nfile '/etc/passwd'` would let the attacker inject additional
/// concat directives and exfiltrate or substitute files into the output.
fn escape_concat_path(p: &str) -> Result<String, String> {
    if p.contains('\n') || p.contains('\r') {
        return Err("path contains newline/CR characters".into());
    }
    Ok(p.replace('\\', "/").replace('\'', "'\\''"))
}

async fn run_proxy_pass(
    app: &AppHandle,
    src_path: &str,
    out_path: &str,
    encoder: &str,
    duration_secs: f64,
    start: std::time::Instant,
    event_name: &str,
) -> Result<(), String> {
    let mut args: Vec<&str> = vec![
        "-y",
        "-hide_banner",
        "-loglevel", "error",
        "-progress", "pipe:1",
        "-nostats",
        "-i", src_path,
        "-vf", "scale='min(1280,iw)':-2",
    ];
    args.extend(encoder_args(encoder));
    args.extend([
        "-g", "15",
        "-keyint_min", "15",
        "-sc_threshold", "0",
        "-c:a", "aac",
        "-b:a", "96k",
        "-movflags", "+faststart",
        out_path,
    ]);

    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar.args(args).spawn().map_err(|e| e.to_string())?;

    let mut last_emit = std::time::Instant::now();
    let total_us = duration_secs * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            // Encoder pipelining can report out-of-order timestamps;
                            // clamp to monotonic so the displayed % doesn't bounce.
                            if us > latest_us { latest_us = us; }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 200 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let elapsed = start.elapsed().as_secs_f64();
                    let eta = if progress > 0.01 {
                        Some((elapsed / progress) - elapsed)
                    } else {
                        None
                    };
                    let _ = app.emit(
                        event_name,
                        ProxyProgress { progress, elapsed_secs: elapsed, eta_secs: eta },
                    );
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "ffmpeg ({}) exited with code {:?}: {}",
                        encoder, payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

async fn run_remux_pass(
    app: &AppHandle,
    src_path: &str,
    out_path: &str,
    duration_secs: f64,
    start: std::time::Instant,
) -> Result<(), String> {
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-progress", "pipe:1",
            "-nostats",
            "-i", src_path,
            "-map", "0:v:0?",
            "-map", "0:a:0?",
            "-c", "copy",
            "-map_chapters", "-1",
            out_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    let mut last_emit = std::time::Instant::now();
    let total_us = duration_secs * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            // Encoder pipelining can report out-of-order timestamps;
                            // clamp to monotonic so the displayed % doesn't bounce.
                            if us > latest_us { latest_us = us; }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 100 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let elapsed = start.elapsed().as_secs_f64();
                    let eta = if progress > 0.01 {
                        Some((elapsed / progress) - elapsed)
                    } else {
                        None
                    };
                    let _ = app.emit(
                        "proxy:progress",
                        ProxyProgress { progress, elapsed_secs: elapsed, eta_secs: eta },
                    );
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "ffmpeg (remux) exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

#[tauri::command]
async fn generate_proxy(
    app: AppHandle,
    state: State<'_, ServerInfo>,
    path: String,
    info: VideoInfo,
) -> Result<ProxyResult, String> {
    // Caller has already probed; reuse the result so we don't pay another
    // ffprobe spawn (~300-500 ms on big MKVs).
    let duration_secs = info.duration_secs;
    let strategy = classify_strategy(&info);
    let start = std::time::Instant::now();

    match strategy {
        Strategy::Direct => {
            // Direct path returns the source MP4 unchanged. It's outside
            // proxy_dir, so register_file_url's scope check would otherwise
            // reject it — pre-trust it here since the user explicitly
            // selected this file.
            allowlist_trust(&state.state, std::path::Path::new(&path)).await?;
            let _ = app.emit(
                "proxy:progress",
                ProxyProgress { progress: 1.0, elapsed_secs: 0.0, eta_secs: Some(0.0) },
            );
            diag(&app, format!("proxy: Direct — {}/{} in {} container, played as-is",
                info.video_codec, info.audio_codec.as_deref().unwrap_or("none"), info.container));
            Ok(ProxyResult {
                play_path: path,
                cached: true,
                strategy: "direct".to_string(),
            })
        }
        Strategy::Remux => {
            let key = proxy_cache_key(&path)?;
            let out_path = proxy_dir(&app)?.join(format!("{}.remux.mp4", &key[..32]));
            if out_path.exists() {
                diag(&app, "proxy: Remux — cache hit");
                return Ok(ProxyResult {
                    play_path: out_path.to_string_lossy().to_string(),
                    cached: true,
                    strategy: "remux".to_string(),
                });
            }
            diag(&app, format!("proxy: Remux — {} container, remuxing to MP4", info.container));
            let out_str = out_path.to_string_lossy().to_string();
            let temp_str = format!("{}.tmp.mp4", out_str);
            let result = run_remux_pass(&app, &path, &temp_str, duration_secs, start).await;
            if let Err(e) = result {
                let _ = std::fs::remove_file(&temp_str);
                diag(&app, format!("proxy: Remux failed ({}), falling back to encode", trunc(&e, 120)));
                eprintln!("[clippy] remux failed: {} — falling back to encode", e);
                return encode_fallback(&app, &path, duration_secs, start, "proxy:progress").await;
            }
            std::fs::rename(&temp_str, &out_path).map_err(|e| e.to_string())?;
            let _ = app.emit(
                "proxy:progress",
                ProxyProgress {
                    progress: 1.0,
                    elapsed_secs: start.elapsed().as_secs_f64(),
                    eta_secs: Some(0.0),
                },
            );
            diag(&app, format!("proxy: Remux done in {:.1}s", start.elapsed().as_secs_f64()));
            Ok(ProxyResult {
                play_path: out_str,
                cached: false,
                strategy: "remux".to_string(),
            })
        }
        Strategy::Encode => encode_fallback(&app, &path, duration_secs, start, "proxy:progress").await,
    }
}

async fn encode_fallback(
    app: &AppHandle,
    path: &str,
    duration_secs: f64,
    start: std::time::Instant,
    event_name: &str,
) -> Result<ProxyResult, String> {
    let key = proxy_cache_key(path)?;
    let out_path = proxy_dir(app)?.join(format!("{}.proxy.mp4", &key[..32]));
    if out_path.exists() {
        diag(app, "proxy: Encode — cache hit");
        return Ok(ProxyResult {
            play_path: out_path.to_string_lossy().to_string(),
            cached: true,
            strategy: "encode (cached)".to_string(),
        });
    }
    diag(app, "proxy: Encode — codec/container needs re-encode for playback");
    let out_str = out_path.to_string_lossy().to_string();
    let temp_str = format!("{}.tmp.mp4", out_str);

    let chain = encoder_chain(app).await;
    let mut used: Option<&'static str> = None;
    let mut last_err = String::from("no encoders available");

    for enc in chain.iter() {
        let _ = std::fs::remove_file(&temp_str);
        match run_proxy_pass(app, path, &temp_str, enc, duration_secs, start, event_name).await {
            Ok(()) => {
                used = Some(*enc);
                *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                break;
            }
            Err(e) => {
                diag(app, format!("proxy: encoder {} failed — trying next", enc));
                eprintln!("[clippy] encoder {} failed: {}", enc, e);
                last_err = e;
            }
        }
    }

    let used = match used {
        Some(e) => e,
        None => {
            let _ = std::fs::remove_file(&temp_str);
            return Err(last_err);
        }
    };

    std::fs::rename(&temp_str, &out_path).map_err(|e| e.to_string())?;
    diag(app, format!("proxy: Encode done via {} in {:.1}s", used, start.elapsed().as_secs_f64()));
    let _ = app.emit(
        event_name,
        ProxyProgress {
            progress: 1.0,
            elapsed_secs: start.elapsed().as_secs_f64(),
            eta_secs: Some(0.0),
        },
    );
    Ok(ProxyResult {
        play_path: out_str,
        cached: false,
        strategy: format!("encode ({})", used),
    })
}

const WAVEFORM_BINS: usize = 4000;

/// List the timestamps (in seconds) of every video keyframe in the source.
/// Cached per source-fingerprint as a binary f32 blob; second-open is free.
/// Frontend uses these to draw faint tick marks on the timeline so the user
/// can see where stream-copy cuts will actually snap.
#[tauri::command]
async fn probe_keyframes(app: AppHandle, path: String) -> Result<Vec<f32>, String> {
    let key = proxy_cache_key(&path)?;
    let cache_path = proxy_dir(&app)?.join(format!("{}.kf.f32", &key[..32]));
    if cache_path.exists() {
        if let Ok(bytes) = std::fs::read(&cache_path) {
            if bytes.len() % 4 == 0 {
                let mut out = Vec::with_capacity(bytes.len() / 4);
                for i in (0..bytes.len()).step_by(4) {
                    let arr: [u8; 4] = bytes[i..i + 4]
                        .try_into()
                        .map_err(|_| "bad cache slice".to_string())?;
                    out.push(f32::from_le_bytes(arr));
                }
                return Ok(out);
            }
        }
    }

    // ffprobe: walk video packets, keep only those with the keyframe flag
    // (`pict_type=I`). Stream as CSV for compactness.
    let output = app
        .shell()
        .sidecar("ffprobe")
        .map_err(|e| e.to_string())?
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-skip_frame", "nokey",
            "-show_entries", "frame=pkt_pts_time",
            "-of", "csv=print_section=0",
            &path,
        ])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe (keyframes) failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut keyframes: Vec<f32> = Vec::new();
    for line in stdout.lines() {
        let s = line.trim();
        if s.is_empty() || s == "N/A" { continue; }
        if let Ok(v) = s.parse::<f32>() {
            keyframes.push(v);
        }
    }

    // Cache as raw little-endian f32s.
    let mut buf = Vec::with_capacity(keyframes.len() * 4);
    for &v in &keyframes {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let _ = std::fs::write(&cache_path, &buf);

    Ok(keyframes)
}

/// Extract a peak-amplitude waveform from one audio track. Returns a vector
/// of WAVEFORM_BINS f32 values in [0, 1] where each bin is the max sample
/// magnitude over its slice of the timeline. Cached per (source, track) on
/// disk so reopening the file is instant.
#[tauri::command]
async fn extract_waveform(
    app: AppHandle,
    path: String,
    info: VideoInfo,
    track_index: Option<u32>,
) -> Result<Vec<f32>, String> {
    let track_idx = track_index.unwrap_or(0);
    let key = proxy_cache_key(&path)?;
    // Track-indexed cache name. Single-track sources end up with .wave-0.f32
    // (was .wave.f32 in v1; old caches will simply re-extract once).
    let cache_path = proxy_dir(&app)?.join(format!("{}.wave-{}.f32", &key[..32], track_idx));
    if cache_path.exists() {
        if let Ok(bytes) = std::fs::read(&cache_path) {
            if bytes.len() == WAVEFORM_BINS * 4 {
                let mut bins = Vec::with_capacity(WAVEFORM_BINS);
                for i in 0..WAVEFORM_BINS {
                    let arr: [u8; 4] = bytes[i * 4..i * 4 + 4]
                        .try_into()
                        .map_err(|_| "bad cache slice".to_string())?;
                    bins.push(f32::from_le_bytes(arr));
                }
                return Ok(bins);
            }
        }
    }

    if info.audio_tracks.is_empty()
        || (track_idx as usize) >= info.audio_tracks.len()
        || info.duration_secs <= 0.0
    {
        return Ok(vec![0.0; WAVEFORM_BINS]);
    }

    // Stream raw mono 8kHz s16le PCM from ffmpeg's stdout for the target track.
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-i", &path,
            "-map", &format!("0:a:{}?", track_idx),
            "-vn",
            "-ac", "1",
            "-ar", "8000",
            "-f", "s16le",
            "-",
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    // Stream-compute peaks per bin without buffering all PCM. A 60-min source
    // at 8 kHz mono s16 would otherwise hold ~57 MB in RAM.
    let total_expected_samples = (info.duration_secs * 8000.0).max(1.0);
    let mut bins = vec![0.0f32; WAVEFORM_BINS];
    let mut leftover: Option<u8> = None;
    let mut samples_seen: u64 = 0;
    let mut current_bin: usize = 0;
    let mut current_max: f32 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(bytes) => {
                let len = bytes.len();
                let mut idx = 0;
                while idx < len {
                    let (lo, hi) = if let Some(prev) = leftover.take() {
                        let h = bytes[idx];
                        idx += 1;
                        (prev, h)
                    } else if idx + 1 >= len {
                        leftover = Some(bytes[idx]);
                        break;
                    } else {
                        let l = bytes[idx];
                        let h = bytes[idx + 1];
                        idx += 2;
                        (l, h)
                    };
                    let sample = i16::from_le_bytes([lo, hi]);
                    let amp = (sample.unsigned_abs() as f32) / 32768.0;
                    let bin_idx = (((samples_seen as f64) * (WAVEFORM_BINS as f64))
                        / total_expected_samples)
                        .floor() as usize;
                    let bin_idx = bin_idx.min(WAVEFORM_BINS - 1);
                    if bin_idx != current_bin {
                        if current_max > bins[current_bin] {
                            bins[current_bin] = current_max;
                        }
                        current_bin = bin_idx;
                        current_max = 0.0;
                    }
                    if amp > current_max {
                        current_max = amp;
                    }
                    samples_seen += 1;
                }
            }
            CommandEvent::Stderr(bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&bytes));
            }
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "waveform extract failed (code {:?}): {}",
                        payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    // Flush the final in-progress bin.
    if current_max > bins[current_bin] {
        bins[current_bin] = current_max;
    }

    // Cache
    let mut buf = Vec::with_capacity(WAVEFORM_BINS * 4);
    for &v in &bins {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let _ = std::fs::write(&cache_path, &buf);

    Ok(bins)
}

#[derive(Serialize, Clone, Debug)]
struct ExportProgress {
    progress: f64,
    elapsed_secs: f64,
}

/// Run ffmpeg with the given args and report progress on the given event
/// channel until termination. Used by both the size-targeted clip and stitch
/// exporters.
async fn run_ffmpeg_with_progress(
    app: &AppHandle,
    args: Vec<String>,
    duration_secs: f64,
    event_name: &str,
) -> Result<(), String> {
    // Summarize the invocation for the diag log so support reports can see
    // *what was attempted* even when the only visible symptom is "export
    // failed". Logs output basename + codec flags + whether a filter graph
    // was used — never the full file path (privacy).
    let summary = summarize_ffmpeg_invocation(&args);
    diag(app, format!("[export] START · {summary} · {duration_secs:.2}s"));

    let sidecar = match app.shell().sidecar("ffmpeg") {
        Ok(s) => s,
        Err(e) => {
            diag(app, format!("[export] FAILED · {summary} · sidecar lookup: {e}"));
            return Err(e.to_string());
        }
    };
    let (mut rx, _child) = match sidecar.args(args).spawn() {
        Ok(t) => t,
        Err(e) => {
            diag(app, format!("[export] FAILED · {summary} · spawn: {e}"));
            return Err(e.to_string());
        }
    };
    let start = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();
    let total_us = duration_secs * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            if us > latest_us {
                                latest_us = us;
                            }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 150 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let _ = app.emit(
                        event_name,
                        ExportProgress {
                            progress,
                            elapsed_secs: start.elapsed().as_secs_f64(),
                        },
                    );
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "ffmpeg exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                let elapsed = start.elapsed().as_secs_f64();
                if payload.code != Some(0) {
                    // Failure path: log every stderr line we got so support
                    // reports include the exact ffmpeg error chain. Truncate
                    // each line to 240 chars to stay sane in the ring buffer.
                    diag(
                        app,
                        format!(
                            "[export] FAILED · {summary} · exit={:?} after {elapsed:.2}s · stderr:\n{}",
                            payload.code,
                            stderr_buf
                                .lines()
                                .map(|l| format!("    {}", trunc(l, 240)))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                    );
                    return Err(format!(
                        "ffmpeg exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                // Success path: log warnings if any (ffmpeg can succeed but
                // still emit useful notes — codec deprecations, container
                // hints, etc.).
                if stderr_buf.trim().is_empty() {
                    diag(app, format!("[export] OK · {summary} · {elapsed:.2}s"));
                } else {
                    diag(
                        app,
                        format!(
                            "[export] OK · {summary} · {elapsed:.2}s · stderr notes:\n{}",
                            stderr_buf
                                .lines()
                                .take(8)
                                .map(|l| format!("    {}", trunc(l, 240)))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                    );
                }
                let _ = app.emit(
                    event_name,
                    ExportProgress {
                        progress: 1.0,
                        elapsed_secs: elapsed,
                    },
                );
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

/// One-liner summary of an ffmpeg sidecar invocation for diag entries.
/// Picks out the bits a support reader cares about: output filename
/// (basename only — never the full path, privacy), video + audio codec
/// flags, target bitrates if set, and whether a `-filter_complex` graph
/// was used (signals "multi-track audio mix" / "speed filter" path).
fn summarize_ffmpeg_invocation(args: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Output path is the last arg in every export-style invocation we
    // build. Show only the basename so the log doesn't leak the user's
    // home directory.
    if let Some(last) = args.last() {
        parts.push(format!("out={}", basename(last)));
    }
    // Pick up -c:v / -c:a / -b:v / -b:a / -preset / -crf flags by walking
    // pairs. Flags without a paired value are skipped (e.g. -vn).
    let mut i = 0;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "-c:v" | "-c:a" | "-b:v" | "-b:a" | "-preset" | "-crf" | "-f" | "-pix_fmt" => {
                let key = args[i].trim_start_matches('-');
                parts.push(format!("{key}={}", args[i + 1]));
                i += 2;
            }
            "-vn" => {
                parts.push("audio-only".into());
                i += 1;
            }
            "-filter_complex" => {
                // Don't dump the whole graph (it can be long); record that
                // a filter graph was used + its byte length so an outlier
                // graph is at least visible.
                parts.push(format!("filter_complex({}B)", args[i + 1].len()));
                i += 2;
            }
            _ => i += 1,
        }
    }
    if parts.is_empty() {
        "ffmpeg".into()
    } else {
        parts.join(" ")
    }
}

/// Re-encode a single region from src_path to fit within target_size_mb.
/// Cascades through the available hardware encoders and falls back to libx264.
#[tauri::command]
async fn export_clip_sized(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    target_size_mb: f64,
    crop: Option<Crop>,
    speed: Option<f64>,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip_sized invoked · src={} in={:.3} out={:.3} dur={:.3} \
             target_mb={} crop={:?} speed={:?} normalize={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            target_size_mb,
            crop.is_some(),
            speed,
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if duration < 0.05 {
        diag(&app, "[export] export_clip_sized REJECTED · selection too short");
        return Err("selection too short".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    // Effective output duration drives the bitrate calc — a 4× speed clip is a
    // quarter as long, so the same byte budget gives 4× the per-second bitrate.
    let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
    let video_bps = target_video_bitrate_bps(target_size_mb, effective_dur);
    let video_kbps = video_bps / 1000;
    let vf = build_video_filter(crop, speed);
    let post_filters = build_audio_post_mix_filters(speed, normalize);
    let (audio_fc, audio_map) = build_audio_filter_complex(&mix, total_tracks, &post_filters);

    let chain = encoder_chain(&app).await;
    let mut last_err = String::from("no encoders available");
    for enc in chain.iter() {
        let mut args: Vec<String> = vec![
            "-y".into(), "-hide_banner".into(),
            "-loglevel".into(), "error".into(),
            "-progress".into(), "pipe:1".into(), "-nostats".into(),
            "-ss".into(), format!("{:.6}", in_secs),
            "-to".into(), format!("{:.6}", out_secs),
            "-i".into(), src_path.clone(),
            "-map".into(), "0:v:0?".into(),
            "-map".into(), audio_map.clone(),
        ];
        if !vf.is_empty() {
            args.push("-vf".into()); args.push(vf.clone());
        }
        if let Some(fc) = &audio_fc {
            args.push("-filter_complex".into()); args.push(fc.clone());
        }
        args.extend(encoder_args_sized(enc, video_kbps));
        args.extend([
            "-c:a".into(), "aac".into(),
            "-b:a".into(), format!("{}k", SIZED_AUDIO_BPS / 1000),
            "-movflags".into(), "+faststart".into(),
            "-map_chapters".into(), "-1".into(),
            output_path.clone(),
        ]);
        match run_ffmpeg_with_progress(&app, args, effective_dur, "export:progress").await {
            Ok(()) => {
                *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                return Ok(());
            }
            Err(e) => {
                eprintln!("[clippy] sized export {} failed: {}", enc, e);
                let _ = std::fs::remove_file(&output_path);
                last_err = e;
            }
        }
    }
    Err(last_err)
}

/// Re-encode a stitched concat of N regions from src_path to fit within
/// target_size_mb. Same encoder cascade as export_clip_sized.
#[tauri::command]
async fn export_concat_sized(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    target_size_mb: f64,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    diag(
        &app,
        format!(
            "[export] export_concat_sized invoked · src={} regions={} target_mb={} \
             normalize={:?} tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            target_size_mb,
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat_sized REJECTED · no regions");
        return Err("no regions to concat".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    // Sized export is single-pass concat-demuxer + encoder, so per-region
    // filters aren't supported. Frontend should gate this, but verify.
    let first = regions[0].clone();
    for (i, r) in regions.iter().enumerate().skip(1) {
        if r.crop != first.crop || r.speed != first.speed || r.mix != first.mix {
            diag(
                &app,
                format!(
                    "[export] export_concat_sized REJECTED · region {} differs in crop/speed/mix from region 1",
                    i + 1
                ),
            );
            return Err(format!(
                "size-targeted stitched export needs uniform crop, speed, and audio mix across regions (region {} differs)",
                i + 1
            ));
        }
    }
    let total_duration: f64 = regions.iter().map(|r| r.effective_duration()).sum();
    if total_duration < 0.05 {
        return Err("total duration too short".into());
    }
    let video_bps = target_video_bitrate_bps(target_size_mb, total_duration);
    let video_kbps = video_bps / 1000;
    let vf = build_video_filter(first.crop, first.speed);
    let post_filters = build_audio_post_mix_filters(first.speed, normalize);
    // Per-region mix wins; falls back to function-level mix.
    let active_mix: &[TrackGain] = first.mix.as_deref().unwrap_or(&mix);
    let (audio_fc, audio_map) = build_audio_filter_complex(active_mix, total_tracks, &post_filters);

    // Write a concat list (forward slashes + escaped quotes for ffmpeg).
    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let list_file = temp_dir.join(format!("concat-sized-{}.txt", stamp));
    let escaped = escape_concat_path(&src_path)?;
    let mut content = String::new();
    for r in &regions {
        content.push_str(&format!("file '{}'\n", escaped));
        content.push_str(&format!("inpoint {:.6}\n", r.in_secs));
        content.push_str(&format!("outpoint {:.6}\n", r.out_secs));
    }
    std::fs::write(&list_file, &content).map_err(|e| e.to_string())?;
    let list_str = list_file.to_string_lossy().to_string();

    let chain = encoder_chain(&app).await;
    let mut last_err = String::from("no encoders available");
    for enc in chain.iter() {
        let mut args: Vec<String> = vec![
            "-y".into(), "-hide_banner".into(),
            "-loglevel".into(), "error".into(),
            "-progress".into(), "pipe:1".into(), "-nostats".into(),
            "-f".into(), "concat".into(),
            "-safe".into(), "0".into(),
            "-i".into(), list_str.clone(),
            "-map".into(), "0:v:0?".into(),
            "-map".into(), audio_map.clone(),
            "-fflags".into(), "+genpts".into(),
        ];
        if !vf.is_empty() {
            args.push("-vf".into()); args.push(vf.clone());
        }
        if let Some(fc) = &audio_fc {
            args.push("-filter_complex".into()); args.push(fc.clone());
        }
        args.extend(encoder_args_sized(enc, video_kbps));
        args.extend([
            "-c:a".into(), "aac".into(),
            "-b:a".into(), format!("{}k", SIZED_AUDIO_BPS / 1000),
            "-movflags".into(), "+faststart".into(),
            "-map_chapters".into(), "-1".into(),
            output_path.clone(),
        ]);
        match run_ffmpeg_with_progress(&app, args, total_duration, "export:progress").await {
            Ok(()) => {
                *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                let _ = std::fs::remove_file(&list_file);
                return Ok(());
            }
            Err(e) => {
                eprintln!("[clippy] sized concat export {} failed: {}", enc, e);
                let _ = std::fs::remove_file(&output_path);
                last_err = e;
            }
        }
    }
    let _ = std::fs::remove_file(&list_file);
    Err(last_err)
}

/// Cut a single region (with its own crop + speed + the export-wide track mix)
/// into `out_path`. Three paths from fastest to slowest:
///   * pure stream-copy (no filters at all)
///   * audio-only re-encode (track mix only, video stream-copies)
///   * full re-encode (crop/speed + maybe audio mix)
/// Audio normalize is per-export and applied at the concat stage, never here.
async fn cut_segment(
    app: &AppHandle,
    src_path: &str,
    region: RegionExport,
    out_path: &str,
    track_mix: &[TrackGain],
    total_audio_tracks: usize,
) -> Result<(), String> {
    let post_filters = build_audio_post_mix_filters(region.speed, false);
    // Per-region mix override wins; fall back to the export-wide mix.
    let active_mix: &[TrackGain] = region.mix.as_deref().unwrap_or(track_mix);
    let (audio_fc, audio_map) =
        build_audio_filter_complex(active_mix, total_audio_tracks, &post_filters);
    let needs_video_reencode = forces_video_reencode(region.crop, region.speed);
    let needs_audio_reencode = audio_fc.is_some();

    if needs_video_reencode {
        let vf = build_video_filter(region.crop, region.speed);
        if let Some(ref fc) = audio_fc {
            diag(app, format!("export: full re-encode — vf=[{}] fc=[{}]", trunc(&vf, 80), trunc(fc, 120)));
        } else {
            diag(app, format!("export: full re-encode — vf=[{}]", trunc(&vf, 80)));
        }
        let chain = encoder_chain(app).await;
        let mut last_err = String::from("no encoders available");
        for enc in chain.iter() {
            let mut args: Vec<String> = vec![
                "-y".into(), "-hide_banner".into(),
                "-loglevel".into(), "error".into(),
                "-ss".into(), format!("{:.6}", region.in_secs),
                "-to".into(), format!("{:.6}", region.out_secs),
                "-i".into(), src_path.into(),
                "-map".into(), "0:v:0?".into(),
                "-map".into(), audio_map.clone(),
            ];
            if !vf.is_empty() {
                args.push("-vf".into()); args.push(vf.clone());
            }
            if let Some(fc) = &audio_fc {
                args.push("-filter_complex".into()); args.push(fc.clone());
            }
            args.extend(encoder_args_high_quality(enc).into_iter().map(String::from));
            args.extend([
                "-c:a".into(), "aac".into(),
                "-b:a".into(), "160k".into(),
                "-map_chapters".into(), "-1".into(),
                out_path.into(),
            ]);
            let t0 = std::time::Instant::now();
            let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
            let (mut rx, _child) = sidecar.args(args).spawn().map_err(|e| e.to_string())?;
            let mut stderr_buf = String::new();
            let mut ok = false;
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
                    CommandEvent::Terminated(payload) => {
                        if payload.code == Some(0) { ok = true; }
                        break;
                    }
                    _ => {}
                }
            }
            if ok {
                diag(app, format!("ffmpeg: exit 0 via {} in {:.1}s", enc, t0.elapsed().as_secs_f64()));
                *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                return Ok(());
            }
            diag(app, format!("ffmpeg: exit non-0 via {} — {}", enc, trunc(&stderr_buf, 200)));
            eprintln!("[clippy] crop/speed cut {} failed: {}", enc, stderr_buf);
            let _ = std::fs::remove_file(out_path);
            last_err = stderr_buf;
        }
        return Err(last_err);
    }

    if needs_audio_reencode {
        // Track mix changed but no crop/speed — video stream-copies, audio
        // gets the filter_complex treatment. Same speed as a normalize-only export.
        if let Some(ref fc) = audio_fc {
            diag(app, format!("export: audio re-encode — fc=[{}]", trunc(fc, 160)));
        }
        let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
        let mut args: Vec<String> = vec![
            "-y".into(), "-hide_banner".into(),
            "-loglevel".into(), "error".into(),
            "-ss".into(), format!("{:.6}", region.in_secs),
            "-to".into(), format!("{:.6}", region.out_secs),
            "-i".into(), src_path.into(),
            "-map".into(), "0:v:0?".into(),
            "-map".into(), audio_map.clone(),
            "-c:v".into(), "copy".into(),
        ];
        if let Some(fc) = &audio_fc {
            args.push("-filter_complex".into()); args.push(fc.clone());
        }
        args.extend([
            "-c:a".into(), "aac".into(),
            "-b:a".into(), "160k".into(),
            "-avoid_negative_ts".into(), "make_zero".into(),
            "-map_chapters".into(), "-1".into(),
            out_path.into(),
        ]);
        let t0 = std::time::Instant::now();
        let (mut rx, _child) = sidecar.args(args).spawn().map_err(|e| e.to_string())?;
        let mut stderr_buf = String::new();
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
                CommandEvent::Terminated(payload) => {
                    if payload.code != Some(0) {
                        diag(app, format!("ffmpeg: exit non-0 (audio mix) — {}", trunc(&stderr_buf, 200)));
                        return Err(format!(
                            "segment cut (audio mix) exited with code {:?}: {}",
                            payload.code, stderr_buf
                        ));
                    }
                    break;
                }
                _ => {}
            }
        }
        diag(app, format!("ffmpeg: exit 0 (audio re-encode) in {:.1}s", t0.elapsed().as_secs_f64()));
        return Ok(());
    }

    diag(app, "export: stream-copy (no crop/speed/mix change)");

    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-ss", &format!("{:.6}", region.in_secs),
            "-to", &format!("{:.6}", region.out_secs),
            "-i", src_path,
            "-map", "0:v:0?",
            "-map", "0:a:0?",
            "-c", "copy",
            "-avoid_negative_ts", "make_zero",
            "-map_chapters", "-1",
            out_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "segment cut exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Concatenate N regions from the same source into a single output file.
///
/// Two-stage stream-copy: cut each region into a clean intermediate MP4 first
/// (keyframe-aligned input seek, self-contained file), then concat the
/// intermediates with `-c copy`. Earlier versions fed `inpoint`/`outpoint` of
/// the same source straight into the concat demuxer, which left mid-GOP frags
/// at boundaries and caused frozen video while audio continued. Boundaries
/// still snap to source keyframes (~1 s with OBS keyframe=1). No re-encode.
#[tauri::command]
async fn export_concat(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    diag(
        &app,
        format!(
            "[export] export_concat invoked · src={} regions={} normalize={:?} \
             tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat REJECTED · no regions");
        return Err("no regions to concat".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let mix_active = !mix.is_empty()
        && !(mix.len() == total_tracks && mix.iter().all(|t| (t.volume - 1.0).abs() < 1e-6));
    // Sum of post-speed durations — what the final output will actually be.
    let total_duration: f64 = regions.iter().map(|r| r.effective_duration()).sum();
    if total_duration < 0.05 {
        diag(&app, "[export] export_concat REJECTED · total duration too short");
        return Err("total duration is too short to export".into());
    }
    // Stage 1 dominates wall-clock when any region needs re-encode (crop/speed).
    let any_reencode = regions.iter().any(|r| forces_video_reencode(r.crop, r.speed));

    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let cleanup = |segs: &[PathBuf], list: Option<&PathBuf>| {
        for s in segs {
            let _ = std::fs::remove_file(s);
        }
        if let Some(l) = list {
            let _ = std::fs::remove_file(l);
        }
    };

    // Stage 1: cut each region into its own clean MP4 intermediate. Per-region
    // crop + speed are applied here; segments may have different dimensions if
    // crops differ — the frontend gates that case for stitched mode.
    let start = std::time::Instant::now();
    let mut temp_segments: Vec<PathBuf> = Vec::with_capacity(regions.len());
    let mut produced_secs: f64 = 0.0;
    for (idx, region) in regions.iter().enumerate() {
        let seg_path = temp_dir.join(format!("seg-{}-{}.mp4", stamp, idx));
        let seg_str = seg_path.to_string_lossy().to_string();
        if let Err(e) = cut_segment(&app, &src_path, region.clone(), &seg_str, &mix, total_tracks).await {
            cleanup(&temp_segments, None);
            return Err(format!("region {} cut failed: {}", idx + 1, e));
        }
        temp_segments.push(seg_path);
        produced_secs += region.effective_duration();
        // With re-encodes (video filter or audio mix), stage 1 dominates.
        let stage1_share = if any_reencode || mix_active { 0.85 } else { 0.4 };
        let progress = (produced_secs / total_duration * stage1_share).clamp(0.0, stage1_share);
        let _ = app.emit(
            "export:progress",
            ExportProgress {
                progress,
                elapsed_secs: start.elapsed().as_secs_f64(),
            },
        );
    }

    // Stage 2: concat the intermediates with stream copy. They're all from the
    // same source, so codec/timescale match — the demuxer just glues them.
    let list_file = temp_dir.join(format!("concat-{}.txt", stamp));
    let mut content = String::new();
    for seg in &temp_segments {
        let escaped = escape_concat_path(&seg.to_string_lossy())?;
        content.push_str(&format!("file '{}'\n", escaped));
    }
    if let Err(e) = std::fs::write(&list_file, &content) {
        cleanup(&temp_segments, None);
        return Err(e.to_string());
    }
    let list_str = list_file.to_string_lossy().to_string();

    let sidecar = match app.shell().sidecar("ffmpeg") {
        Ok(s) => s,
        Err(e) => {
            cleanup(&temp_segments, Some(&list_file));
            return Err(e.to_string());
        }
    };
    // Normalize forces an audio re-encode at concat time (filter incompatible
    // with -c copy on the audio stream). Video can still stream-copy.
    let mut concat_args: Vec<String> = vec![
        "-y".into(), "-hide_banner".into(),
        "-loglevel".into(), "error".into(),
        "-progress".into(), "pipe:1".into(), "-nostats".into(),
        "-f".into(), "concat".into(),
        "-safe".into(), "0".into(),
        "-i".into(), list_str.clone(),
        "-map".into(), "0:v:0?".into(),
        "-map".into(), "0:a:0?".into(),
    ];
    if normalize {
        concat_args.extend([
            "-c:v".into(), "copy".into(),
            "-af".into(), LOUDNORM_FILTER.into(),
            "-c:a".into(), "aac".into(),
            "-b:a".into(), "160k".into(),
        ]);
    } else {
        concat_args.extend(["-c".into(), "copy".into()]);
    }
    concat_args.extend([
        "-movflags".into(), "+faststart".into(),
        "-map_chapters".into(), "-1".into(),
        output_path.clone(),
    ]);
    let (mut rx, _child) = match sidecar.args(concat_args).spawn() {
        Ok(p) => p,
        Err(e) => {
            cleanup(&temp_segments, Some(&list_file));
            return Err(e.to_string());
        }
    };

    let mut last_emit = std::time::Instant::now();
    let total_us = total_duration * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            if us > latest_us {
                                latest_us = us;
                            }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 150 {
                    let stage2 = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let stage1_share = if any_reencode || mix_active { 0.85 } else { 0.4 };
                    let progress = (stage1_share + stage2 * (1.0 - stage1_share)).min(1.0);
                    let _ = app.emit(
                        "export:progress",
                        ExportProgress {
                            progress,
                            elapsed_secs: start.elapsed().as_secs_f64(),
                        },
                    );
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                cleanup(&temp_segments, Some(&list_file));
                if payload.code != Some(0) {
                    diag(&app, format!("ffmpeg: concat failed — {}", trunc(&stderr_buf, 200)));
                    return Err(format!(
                        "ffmpeg concat exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                diag(&app, format!("export_concat: done in {:.1}s", start.elapsed().as_secs_f64()));
                let _ = app.emit(
                    "export:progress",
                    ExportProgress {
                        progress: 1.0,
                        elapsed_secs: start.elapsed().as_secs_f64(),
                    },
                );
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

#[tauri::command]
async fn export_clip(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    crop: Option<Crop>,
    speed: Option<f64>,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip invoked · src={} in={:.3} out={:.3} dur={:.3} \
             crop={:?} speed={:?} normalize={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            crop.is_some(),
            speed,
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if duration < 0.05 {
        diag(&app, "[export] export_clip REJECTED · selection too short");
        return Err("selection too short".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let post_filters = build_audio_post_mix_filters(speed, normalize);
    let (audio_fc, audio_map) = build_audio_filter_complex(&mix, total_tracks, &post_filters);
    let needs_audio_reencode = audio_fc.is_some();

    // Four paths, fastest to slowest:
    //   * pure stream-copy (no mix, no normalize, no crop/speed)
    //   * audio re-encode only (track mix or normalize but no video filter)
    //   * full re-encode (crop/speed + maybe audio mix/normalize)
    if forces_video_reencode(crop, speed) {
        let vf = build_video_filter(crop, speed);
        let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
        let chain = encoder_chain(&app).await;
        let mut last_err = String::from("no encoders available");
        for enc in chain.iter() {
            let mut args: Vec<String> = vec![
                "-y".into(), "-hide_banner".into(),
                "-loglevel".into(), "error".into(),
                "-progress".into(), "pipe:1".into(), "-nostats".into(),
                "-ss".into(), format!("{:.6}", in_secs),
                "-to".into(), format!("{:.6}", out_secs),
                "-i".into(), src_path.clone(),
                "-map".into(), "0:v:0?".into(),
                "-map".into(), audio_map.clone(),
            ];
            if !vf.is_empty() {
                args.push("-vf".into()); args.push(vf.clone());
            }
            if let Some(fc) = &audio_fc {
                args.push("-filter_complex".into()); args.push(fc.clone());
            }
            args.extend(encoder_args_high_quality(enc).into_iter().map(String::from));
            args.extend([
                "-c:a".into(), "aac".into(),
                "-b:a".into(), "160k".into(),
                "-movflags".into(), "+faststart".into(),
                "-map_chapters".into(), "-1".into(),
                output_path.clone(),
            ]);
            match run_ffmpeg_with_progress(&app, args, effective_dur, "export:progress").await {
                Ok(()) => {
                    *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("[clippy] crop/speed export {} failed: {}", enc, e);
                    let _ = std::fs::remove_file(&output_path);
                    last_err = e;
                }
            }
        }
        return Err(last_err);
    }

    if needs_audio_reencode {
        // Video stream-copy, audio runs through filter_complex (track mix +
        // optional normalize). Cheap — only the audio track is touched.
        let mut args: Vec<String> = vec![
            "-y".into(), "-hide_banner".into(),
            "-loglevel".into(), "error".into(),
            "-progress".into(), "pipe:1".into(), "-nostats".into(),
            "-ss".into(), format!("{:.6}", in_secs),
            "-to".into(), format!("{:.6}", out_secs),
            "-i".into(), src_path.clone(),
            "-map".into(), "0:v:0?".into(),
            "-map".into(), audio_map.clone(),
            "-c:v".into(), "copy".into(),
        ];
        if let Some(fc) = &audio_fc {
            args.push("-filter_complex".into()); args.push(fc.clone());
        }
        args.extend([
            "-c:a".into(), "aac".into(),
            "-b:a".into(), "160k".into(),
            "-avoid_negative_ts".into(), "make_zero".into(),
            "-movflags".into(), "+faststart".into(),
            "-map_chapters".into(), "-1".into(),
            output_path.clone(),
        ]);
        return run_ffmpeg_with_progress(&app, args, duration, "export:progress").await;
    }

    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-progress", "pipe:1",
            "-nostats",
            "-ss", &format!("{:.6}", in_secs),
            "-to", &format!("{:.6}", out_secs),
            "-i", &src_path,
            "-c", "copy",
            "-avoid_negative_ts", "make_zero",
            "-map", "0",
            "-map_chapters", "-1",
            &output_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    let start = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();
    let total_us = duration * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            // Encoder pipelining can report out-of-order timestamps;
                            // clamp to monotonic so the displayed % doesn't bounce.
                            if us > latest_us { latest_us = us; }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 150 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let _ = app.emit(
                        "export:progress",
                        ExportProgress {
                            progress,
                            elapsed_secs: start.elapsed().as_secs_f64(),
                        },
                    );
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "ffmpeg exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                let _ = app.emit(
                    "export:progress",
                    ExportProgress {
                        progress: 1.0,
                        elapsed_secs: start.elapsed().as_secs_f64(),
                    },
                );
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// MP3 audio bitrate used for all audio-only exports. 192k is the conventional
/// "small but sounds fine for music + voice" sweet spot.
const MP3_BITRATE: &str = "192k";

/// Export the audio of a single [in,out] slice as MP3. Always re-encodes via
/// libmp3lame (source audio is almost always AAC, not MP3). Crops are ignored
/// — there's no video stream in the output. Speed + normalize are honored
/// because both are audio-only filters (atempo + loudnorm).
#[tauri::command]
async fn export_clip_audio(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    speed: Option<f64>,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip_audio invoked · src={} in={:.3} out={:.3} dur={:.3} \
             speed={:?} normalize={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            speed,
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if duration < 0.05 {
        diag(&app, "[export] export_clip_audio REJECTED · selection too short");
        return Err("selection too short".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
    let post_filters = build_audio_post_mix_filters(speed, normalize);
    let (audio_fc, audio_map) = build_audio_filter_complex(&mix, total_tracks, &post_filters);

    let mut args: Vec<String> = vec![
        "-y".into(), "-hide_banner".into(),
        "-loglevel".into(), "error".into(),
        "-progress".into(), "pipe:1".into(), "-nostats".into(),
        "-ss".into(), format!("{:.6}", in_secs),
        "-to".into(), format!("{:.6}", out_secs),
        "-i".into(), src_path,
        "-vn".into(),
        "-map".into(), audio_map,
    ];
    if let Some(fc) = audio_fc {
        args.push("-filter_complex".into()); args.push(fc);
    }
    args.extend([
        "-c:a".into(), "libmp3lame".into(),
        "-b:a".into(), MP3_BITRATE.into(),
        "-map_chapters".into(), "-1".into(),
        output_path,
    ]);
    run_ffmpeg_with_progress(&app, args, effective_dur, "export:progress").await
}

/// Export N regions concatenated as a single MP3. Single-pass: ffmpeg's concat
/// demuxer feeds the regions straight into libmp3lame. Speed/normalize apply
/// uniformly across all regions (single-pass means we can't do per-region
/// filters). Frontend should gate mixed-speed cases.
#[tauri::command]
async fn export_concat_audio(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    diag(
        &app,
        format!(
            "[export] export_concat_audio invoked · src={} regions={} normalize={:?} \
             tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat_audio REJECTED · no regions");
        return Err("no regions to concat".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let first = regions[0].clone();
    for (i, r) in regions.iter().enumerate().skip(1) {
        if r.speed != first.speed || r.mix != first.mix {
            diag(
                &app,
                format!(
                    "[export] export_concat_audio REJECTED · region {} differs in speed/mix from region 1",
                    i + 1
                ),
            );
            return Err(format!(
                "stitched MP3 export needs uniform speed and audio mix across regions (region {} differs)",
                i + 1
            ));
        }
    }
    let total_duration: f64 = regions.iter().map(|r| r.effective_duration()).sum();
    if total_duration < 0.05 {
        diag(&app, "[export] export_concat_audio REJECTED · total duration too short");
        return Err("total duration too short".into());
    }
    let post_filters = build_audio_post_mix_filters(first.speed, normalize);
    let active_mix: &[TrackGain] = first.mix.as_deref().unwrap_or(&mix);
    let (audio_fc, audio_map) = build_audio_filter_complex(active_mix, total_tracks, &post_filters);

    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let list_file = temp_dir.join(format!("concat-mp3-{}.txt", stamp));
    let escaped = escape_concat_path(&src_path)?;
    let mut content = String::new();
    for r in &regions {
        content.push_str(&format!("file '{}'\n", escaped));
        content.push_str(&format!("inpoint {:.6}\n", r.in_secs));
        content.push_str(&format!("outpoint {:.6}\n", r.out_secs));
    }
    std::fs::write(&list_file, &content).map_err(|e| e.to_string())?;
    let list_str = list_file.to_string_lossy().to_string();

    let mut args: Vec<String> = vec![
        "-y".into(), "-hide_banner".into(),
        "-loglevel".into(), "error".into(),
        "-progress".into(), "pipe:1".into(), "-nostats".into(),
        "-f".into(), "concat".into(),
        "-safe".into(), "0".into(),
        "-i".into(), list_str,
        "-vn".into(),
        "-map".into(), audio_map,
    ];
    if let Some(fc) = audio_fc {
        args.push("-filter_complex".into()); args.push(fc);
    }
    args.extend([
        "-c:a".into(), "libmp3lame".into(),
        "-b:a".into(), MP3_BITRATE.into(),
        "-map_chapters".into(), "-1".into(),
        output_path,
    ]);
    let result = run_ffmpeg_with_progress(&app, args, total_duration, "export:progress").await;
    let _ = std::fs::remove_file(&list_file);
    result
}

// ---- GIF + PNG export ----

/// GIF target framerate. 15 fps is the conventional reaction-clip cadence —
/// smooth enough to look intentional, small enough to keep file sizes sane.
const GIF_FPS: u32 = 15;
/// Default GIF width if the caller doesn't specify (≈ 720p widescreen height).
const GIF_DEFAULT_WIDTH: u32 = 1280;

/// Build the `-vf` filter for a GIF export. Combines optional crop + speed +
/// the standard fps/scale/palettegen pipeline. The scale filter caps to
/// `min(target, iw)` so requesting Source / over-resolution never upscales.
fn gif_filter_chain(crop: Option<Crop>, speed: Option<f64>, target_width: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = crop {
        parts.push(c.to_filter());
    }
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 && s > 0.0 {
            parts.push(format!("setpts={:.6}*PTS", 1.0 / s));
        }
    }
    parts.push(format!("fps={}", GIF_FPS));
    parts.push(format!(
        "scale='min({},iw)':-2:flags=lanczos",
        target_width
    ));
    let pre = parts.join(",");
    format!("{},split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5", pre)
}

/// Single-region GIF export. Crop + speed honored; no audio. `target_width`
/// caps the long edge — `min(target_width, iw)` so over-spec never upscales.
#[tauri::command]
async fn export_clip_gif(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    crop: Option<Crop>,
    speed: Option<f64>,
    target_width: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    if duration < 0.05 {
        return Err("selection too short".into());
    }
    let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
    let filter = gif_filter_chain(crop, speed, target_width.unwrap_or(GIF_DEFAULT_WIDTH));
    let args: Vec<String> = vec![
        "-y".into(), "-hide_banner".into(),
        "-loglevel".into(), "error".into(),
        "-progress".into(), "pipe:1".into(), "-nostats".into(),
        "-ss".into(), format!("{:.6}", in_secs),
        "-to".into(), format!("{:.6}", out_secs),
        "-i".into(), src_path,
        "-an".into(),
        "-filter_complex".into(), filter,
        "-loop".into(), "0".into(),
        output_path,
    ];
    run_ffmpeg_with_progress(&app, args, effective_dur, "export:progress").await
}

/// Stitched GIF: concat regions then apply the GIF pipeline. Two-stage like
/// the video concat — stage 1 cuts each region (with per-region crop+speed
/// pre-applied), stage 2 concatenates and runs through palettegen.
#[tauri::command]
async fn export_concat_gif(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    target_width: Option<u32>,
) -> Result<(), String> {
    if regions.is_empty() {
        return Err("no regions to concat".into());
    }
    let total_duration: f64 = regions.iter().map(|r| r.effective_duration()).sum();
    if total_duration < 0.05 {
        return Err("total duration too short".into());
    }

    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    // Stage 1: cut each region (per-region filters baked in).
    let start = std::time::Instant::now();
    let mut temp_segments: Vec<PathBuf> = Vec::with_capacity(regions.len());
    let mut produced_secs: f64 = 0.0;
    for (idx, region) in regions.iter().enumerate() {
        let seg_path = temp_dir.join(format!("seg-gif-{}-{}.mp4", stamp, idx));
        let seg_str = seg_path.to_string_lossy().to_string();
        // GIF drops audio in stage 2, so the mix is irrelevant here.
        if let Err(e) = cut_segment(&app, &src_path, region.clone(), &seg_str, &[], 0).await {
            for s in &temp_segments { let _ = std::fs::remove_file(s); }
            return Err(format!("region {} cut failed: {}", idx + 1, e));
        }
        temp_segments.push(seg_path);
        produced_secs += region.effective_duration();
        let progress = (produced_secs / total_duration * 0.7).clamp(0.0, 0.7);
        let _ = app.emit("export:progress", ExportProgress {
            progress,
            elapsed_secs: start.elapsed().as_secs_f64(),
        });
    }

    // Stage 2: build a concat list, run through palette pipeline.
    let list_file = temp_dir.join(format!("concat-gif-{}.txt", stamp));
    let mut content = String::new();
    for seg in &temp_segments {
        let escaped = escape_concat_path(&seg.to_string_lossy())?;
        content.push_str(&format!("file '{}'\n", escaped));
    }
    if let Err(e) = std::fs::write(&list_file, &content) {
        for s in &temp_segments { let _ = std::fs::remove_file(s); }
        return Err(e.to_string());
    }
    let list_str = list_file.to_string_lossy().to_string();
    // Stage 2 filter: only fps + scale + palette (per-region crop/speed already
    // applied in stage 1).
    let target = target_width.unwrap_or(GIF_DEFAULT_WIDTH);
    let filter = format!(
        "fps={},scale='min({},iw)':-2:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5",
        GIF_FPS, target
    );
    let args: Vec<String> = vec![
        "-y".into(), "-hide_banner".into(),
        "-loglevel".into(), "error".into(),
        "-progress".into(), "pipe:1".into(), "-nostats".into(),
        "-f".into(), "concat".into(),
        "-safe".into(), "0".into(),
        "-i".into(), list_str,
        "-an".into(),
        "-filter_complex".into(), filter,
        "-loop".into(), "0".into(),
        output_path,
    ];
    let result = run_ffmpeg_with_progress(&app, args, total_duration, "export:progress").await;
    let _ = std::fs::remove_file(&list_file);
    for s in &temp_segments { let _ = std::fs::remove_file(s); }
    result
}

// ---- Per-track audio extraction (for WebAudio multi-track preview) ----
//
// SteelSeries Sonar / OBS produce MP4s with separate audio tracks for game,
// mic, Discord, etc. The HTML5 video element only plays one track at a time,
// so to give the user real per-track mute/volume sliders we extract each
// audio stream into its own playable file and feed them through WebAudio.
// The same fingerprint-keyed cache as proxies/waveforms.

#[tauri::command]
async fn extract_track(
    app: AppHandle,
    state: State<'_, ServerInfo>,
    src_path: String,
    track_index: u32,
) -> Result<String, String> {
    let key = proxy_cache_key(&src_path)?;
    let cache_path = proxy_dir(&app)?.join(format!(
        "{}.track-{}.m4a",
        &key[..32],
        track_index
    ));
    if !cache_path.exists() {
        let cache_str = cache_path.to_string_lossy().to_string();
        let temp_str = format!("{}.tmp.m4a", cache_str);
        // Stream-copy the requested audio track into an MP4-in-M4A container.
        // -bsf:a aac_adtstoasc handles the rare AAC-in-MPEG-TS case; harmless
        // for already-clean AAC.
        let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
        let (mut rx, _child) = sidecar
            .args([
                "-y",
                "-hide_banner",
                "-loglevel", "error",
                "-i", &src_path,
                "-map", &format!("0:a:{}?", track_index),
                "-vn",
                "-c:a", "copy",
                "-bsf:a", "aac_adtstoasc",
                "-map_chapters", "-1",
                &temp_str,
            ])
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut stderr_buf = String::new();
        let mut ok = false;
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
                CommandEvent::Terminated(payload) => {
                    if payload.code == Some(0) { ok = true; }
                    break;
                }
                _ => {}
            }
        }
        if !ok {
            // Fallback: re-encode to AAC. Source might be a codec we can't
            // copy into M4A (e.g. opus). Slow but always works.
            let _ = std::fs::remove_file(&temp_str);
            let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
            let (mut rx, _child) = sidecar
                .args([
                    "-y",
                    "-hide_banner",
                    "-loglevel", "error",
                    "-i", &src_path,
                    "-map", &format!("0:a:{}?", track_index),
                    "-vn",
                    "-c:a", "aac",
                    "-b:a", "192k",
                    "-map_chapters", "-1",
                    &temp_str,
                ])
                .spawn()
                .map_err(|e| e.to_string())?;
            stderr_buf.clear();
            let mut ok2 = false;
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
                    CommandEvent::Terminated(payload) => {
                        if payload.code == Some(0) { ok2 = true; }
                        break;
                    }
                    _ => {}
                }
            }
            if !ok2 {
                let _ = std::fs::remove_file(&temp_str);
                return Err(format!("track extract failed: {}", stderr_buf));
            }
        }
        std::fs::rename(&temp_str, &cache_path).map_err(|e| e.to_string())?;
    }
    // Register with the media server and return the playable URL.
    let path_str = cache_path.to_string_lossy().to_string();
    state.state.allowlist.lock().await.insert(cache_path);
    let encoded = urlencoding::encode(&path_str).into_owned();
    Ok(format!(
        "http://127.0.0.1:{}/vid?token={}&p={}",
        state.port, state.state.token, encoded
    ))
}

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
fn load_project(app: AppHandle, src_path: String) -> Result<Option<serde_json::Value>, String> {
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
fn save_project(
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

/// Copy a single frame at `time_secs` to the OS clipboard as a raster image.
/// Uses ffmpeg's rawvideo+rgba pipe so we don't need a PNG decoder in-process —
/// arboard takes raw RGBA bytes directly.
#[tauri::command]
async fn copy_frame_to_clipboard(
    app: AppHandle,
    src_path: String,
    time_secs: f64,
    width: u32,
    height: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("invalid source dimensions".into());
    }
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-ss", &format!("{:.6}", time_secs),
            "-i", &src_path,
            "-frames:v", "1",
            "-vsync", "0",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "pipe:1",
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    let expected_bytes = (width as usize) * (height as usize) * 4;
    let mut buf: Vec<u8> = Vec::with_capacity(expected_bytes);
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(b) => buf.extend_from_slice(&b),
            CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "frame extract failed (code {:?}): {}",
                        payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    if buf.len() != expected_bytes {
        return Err(format!(
            "raw RGBA pipe returned {} bytes, expected {}",
            buf.len(),
            expected_bytes
        ));
    }
    // arboard's set_image consumes the buffer. Done on a blocking task because
    // Windows clipboard APIs aren't async-friendly.
    let w = width as usize;
    let h = height as usize;
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        let img = arboard::ImageData {
            width: w,
            height: h,
            bytes: std::borrow::Cow::Owned(buf),
        };
        cb.set_image(img).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Save a single frame at `time_secs` as a PNG at source resolution.
#[tauri::command]
async fn export_frame_png(
    app: AppHandle,
    src_path: String,
    time_secs: f64,
    output_path: String,
) -> Result<(), String> {
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-ss", &format!("{:.6}", time_secs),
            "-i", &src_path,
            "-frames:v", "1",
            "-vsync", "0",
            "-q:v", "1",
            &output_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "frame export failed (code {:?}): {}",
                        payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Return the in-memory diagnostic log as a plain-text string. Called only
/// when the user explicitly clicks "Copy diagnostics" — never sent anywhere
/// automatically. Full file paths are never logged; only basenames are used.
#[tauri::command]
fn get_diagnostics(app: AppHandle) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut out = String::new();
    out.push_str(&format!(
        "Clippy v{} ({}) — diagnostic snapshot\n",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { "dev build" } else { "release" }
    ));

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    out.push_str(&format!(
        "Captured: {} UTC (epoch {})\n",
        fmt_utc(now_secs),
        now_secs
    ));
    out.push_str(&format!(
        "Target: {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    out.push_str("\n--- Replay buffer ---\n");

    let replay_state = app.state::<replay::ReplayState>();
    let coord_running = replay_state
        .coord
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false);
    let allowlist_size = replay_state
        .allowlist
        .lock()
        .map(|g| g.len())
        .unwrap_or(0);
    out.push_str(&format!(
        "Coordinator running: {coord_running}\n\
         Game allowlist size: {allowlist_size}\n",
    ));
    let monitors = replay::capture::list_monitors();
    out.push_str(&format!("Monitors detected: {}\n", monitors.len()));
    for m in &monitors {
        out.push_str(&format!(
            "  · {} ({}x{}{})\n",
            m.label,
            m.width,
            m.height,
            if m.primary { ", primary" } else { "" }
        ));
    }
    let audio_devices = replay::audio::enumerate_render_devices();
    out.push_str(&format!("Audio render devices: {}\n", audio_devices.len()));
    for d in &audio_devices {
        out.push_str(&format!(
            "  · {}{}\n",
            d.name,
            if d.is_default { " (default)" } else { "" }
        ));
    }

    // ----- Performance (most-recent ~30s rollup per worker) -----
    out.push_str("\n--- Performance (last rollup) ---\n");
    let perf_rows: Vec<replay::coordinator::WorkerPerfRow> = {
        let guard = match replay_state.coord.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        guard.as_ref().map(|c| c.perf_snapshot()).unwrap_or_default()
    };
    if perf_rows.is_empty() {
        out.push_str("(no active workers — no perf data yet)\n");
    } else {
        for row in perf_rows {
            let p = &row.perf;
            let secs = p.window_secs.max(0.001);
            let cap_fps = p.captured_frames as f32 / secs;
            let sub_fps = p.submitted_frames as f32 / secs;
            let kbps = (p.encoded_bytes as f32 * 8.0 / secs / 1024.0) as u64;
            let age = if p.published_epoch == 0 {
                "no rollup yet".to_string()
            } else {
                format!("rollup at {} UTC", fmt_utc(p.published_epoch))
            };
            out.push_str(&format!(
                "· {label} — {w}x{h}@{fps} via \"{enc}\"\n  window={secs:.1}s cap={cap}({cap_fps:.1}fps) sub={sub}({sub_fps:.1}fps) dup={dup} pkts={pkts} bitrate≈{kbps}kbps · rss={rss}MB · {age}\n",
                label = row.label,
                w = row.enc_width,
                h = row.enc_height,
                fps = row.fps,
                enc = if row.encoder_name.is_empty() { "<unnamed>" } else { &row.encoder_name },
                cap = p.captured_frames,
                sub = p.submitted_frames,
                dup = p.duplicated_frames,
                pkts = p.encoded_packets,
                rss = p.rss_mb,
            ));
        }
    }

    out.push_str("\n--- Event log ---\n");
    let arc = Arc::clone(&app.state::<DiagLog>().0);
    let buf = match arc.lock() {
        Ok(b) => b,
        Err(e) => e.into_inner(),
    };
    out.push_str(&format!("({} entries)\n", buf.len()));
    for entry in buf.iter() {
        out.push_str(entry);
        out.push('\n');
    }
    out
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // Intercept the X button so we can hide-to-tray instead of exit when
        // the user opted in (keeps the replay buffer running in background).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let hide = app
                    .state::<HideOnClose>()
                    .0
                    .load(std::sync::atomic::Ordering::SeqCst);
                if hide {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // No CLI args — Clippy autodetects state from saved settings.
            None,
        ))
        // Self-updating: checks the configured endpoint for a newer signed
        // installer; the frontend drives check/download/install via the JS
        // plugin. `process` is needed so the app can restart itself after
        // installing the update.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            app.manage(DiagLog::new());
            app.manage(InitialPath(Mutex::new(parse_initial_path())));
            app.manage(replay::ReplayState::new());
            app.manage(HideOnClose(std::sync::atomic::AtomicBool::new(false)));
            app.manage(DiagVerbose(std::sync::atomic::AtomicBool::new(false)));

            // Replay save dir — load persisted preference if present, otherwise
            // compute the default and create it on disk so the user's first
            // save doesn't fail on a missing folder.
            {
                let handle = app.handle();
                let save_dir = replay::load_save_dir(&handle.clone());
                if let Err(e) = std::fs::create_dir_all(&save_dir) {
                    eprintln!(
                        "[clippy] couldn't create replay save dir {}: {e}",
                        save_dir.display()
                    );
                }
                app.manage(ReplaySaveDir(Mutex::new(save_dir)));
            }

            // System tray icon. Left-click shows the window; menu has Show + Quit.
            // Combined with the close-to-tray setting, lets the user keep the
            // replay buffer running in the background after closing the window.
            {
                use tauri::menu::{Menu, MenuEvent, MenuItem};
                use tauri::tray::{
                    MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
                };

                let show_item = MenuItem::with_id(app, "tray-show", "Show Clippy", true, None::<&str>)?;
                let quit_item = MenuItem::with_id(app, "tray-quit", "Quit Clippy", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

                let on_menu = |app: &AppHandle, event: MenuEvent| {
                    match event.id().as_ref() {
                        "tray-show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                            }
                        }
                        "tray-quit" => app.exit(0),
                        _ => {}
                    }
                };
                let on_icon = |tray: &tauri::tray::TrayIcon, event: TrayIconEvent| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    }
                };

                if let Some(icon) = app.default_window_icon() {
                    let _ = TrayIconBuilder::new()
                        .icon(icon.clone())
                        .tooltip("Clippy")
                        .menu(&menu)
                        .show_menu_on_left_click(false)
                        .on_menu_event(on_menu)
                        .on_tray_icon_event(on_icon)
                        .build(app);
                }
            }

            #[cfg(debug_assertions)]
            if let Some(win) = app.get_webview_window("main") {
                win.open_devtools();
            }

            // Global hotkey for "save replay buffer" — defaults to Alt+F10.
            // Re-registered at runtime by `replay_set_save_hotkey` when the
            // user rebinds via the keybind editor.
            if let Err(e) = register_save_hotkey(&app.handle().clone(), "Alt+F10") {
                eprintln!("[clippy] save hotkey init failed: {e}");
            }

            // Auto-prune cache files >30 days untouched. Background thread so a
            // slow disk doesn't block startup. Manual "Clear cache" is exposed
            // via the clear_cache command for users who want it now.
            if let Ok(data_dir) = app.path().app_data_dir() {
                let proxies = data_dir.join("proxies");
                std::thread::spawn(move || prune_old_cache(proxies, 30));
            }

            // Bind the listener synchronously so the port is known before any
            // frontend command can fire, then drive accept/serve on tokio.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.set_nonblocking(true)?;
            let port = listener.local_addr()?.port();
            let token = generate_session_token();
            let state = ServerState {
                token,
                port,
                allowlist: Arc::new(AsyncMutex::new(HashSet::new())),
            };
            app.manage(ServerInfo {
                port,
                state: state.clone(),
            });
            tauri::async_runtime::spawn(async move {
                let tokio_listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("[clippy] failed to convert listener: {}", e);
                        return;
                    }
                };
                let app = Router::new()
                    .route("/vid", get(serve_file).options(serve_options))
                    .with_state(state);
                if let Err(e) = axum::serve(tokio_listener, app).await {
                    eprintln!("[clippy] media server stopped: {}", e);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            probe_video,
            generate_proxy,
            export_clip,
            export_clip_sized,
            export_concat,
            export_concat_sized,
            export_clip_audio,
            export_concat_audio,
            register_file_url,
            extract_waveform,
            probe_keyframes,
            file_size,
            reveal_in_folder,
            get_initial_path,
            cache_size,
            clear_cache,
            export_clip_gif,
            export_concat_gif,
            export_frame_png,
            copy_frame_to_clipboard,
            load_project,
            save_project,
            extract_track,
            get_diagnostics,
            replay::get_replay_status,
            replay::replay_start,
            replay::replay_stop,
            replay::replay_save,
            replay::replay_list_monitors,
            replay::replay_list_audio_devices,
            replay::replay_set_audio_names,
            replay::replay_get_system_info,
            replay::replay_list_games,
            replay::replay_rescan_games,
            replay::replay_add_game,
            replay::replay_add_current_game,
            replay::replay_remove_game,
            replay::replay_recent_games,
            replay::replay_get_save_dir,
            replay::replay_set_save_dir,
            replay::replay_reset_save_dir,
            storage_summary,
            clear_diagnostics_log,
            replay_set_save_hotkey,
            set_hide_on_close,
            set_diag_verbose,
            // PoC pipeline-validation commands are dev-only — gated behind
            // the `poc` cargo feature so a default release build doesn't
            // expose them. Build `cargo build --features poc` (or invoke
            // the matching `pnpm tauri dev` recipe) to enable them.
            #[cfg(feature = "poc")]
            replay::replay_poc_test,
            #[cfg(feature = "poc")]
            replay::replay_poc_gpu_convert,
            #[cfg(feature = "poc")]
            replay::replay_poc_gpu_full
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Persist the in-memory diag log on graceful exit so a problem
            // report after the fact still has the most-recent ~200 events.
            // Hard crashes that bypass RunEvent::Exit will lose this round.
            if let tauri::RunEvent::Exit = event {
                persist_diag_log(app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- build_audio_filter_complex coverage ----------
    //
    // Lock in every branch — the bug fixed 2026-05-11 was a missing
    // short-circuit for "single active track at unity gain with no
    // post-mix filters" that produced `Some("")` + `[aout]` and ffmpeg
    // rejected the empty graph with code -22. These tests pin the
    // contract per-branch so a future refactor can't silently regress.

    fn gain(index: u32, volume: f64) -> TrackGain {
        TrackGain { index, volume }
    }

    #[test]
    fn fc_single_source_default_mix_takes_fast_path() {
        // Default mix, single source, no post-filters → direct map, no graph.
        let (fc, map) = build_audio_filter_complex(&[], 1, "");
        assert!(fc.is_none(), "single-track default mix should not build a graph");
        assert_eq!(map, "0:a:0?");
    }

    #[test]
    fn fc_multi_source_default_mix_builds_amix() {
        // 3 tracks, no explicit mix entries → still amix all of them at unity.
        let (fc, map) = build_audio_filter_complex(&[], 3, "");
        let fc = fc.expect("multi-track default mix needs an amix graph");
        assert!(!fc.is_empty(), "graph must not be empty");
        assert!(fc.contains("amix=inputs=3"), "graph: {fc}");
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_single_active_unity_no_post_filters_short_circuits() {
        // The regression: user has 1 track at unity + 0 post filters → must
        // return None + direct stream map. Previously returned Some("") +
        // "[aout]" which crashed ffmpeg with code -22.
        let mix = vec![gain(0, 1.0)];
        let (fc, map) = build_audio_filter_complex(&mix, 1, "");
        assert!(
            fc.is_none(),
            "single unity-gain active track + no post-filters must NOT build a graph (got {fc:?})"
        );
        assert_eq!(map, "0:a:0?");
    }

    #[test]
    fn fc_single_active_unity_with_post_filters_builds_graph() {
        // Same active set but WITH post-mix filters (e.g. normalize) → we
        // need a graph that routes the lone stream through them.
        let mix = vec![gain(0, 1.0)];
        let (fc, map) = build_audio_filter_complex(&mix, 1, ",loudnorm");
        let fc = fc.expect("post-filters force a graph");
        assert!(fc.contains("[0:a:0]"), "graph must reference the active stream: {fc}");
        assert!(fc.contains("loudnorm"), "graph must include the post-filter: {fc}");
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_all_muted_synthesizes_silence() {
        // Volumes all 0 → no active tracks → anullsrc fallback so the muxer
        // still has an audio stream to attach.
        let mix = vec![gain(0, 0.0), gain(1, 0.0)];
        let (fc, map) = build_audio_filter_complex(&mix, 2, "");
        let fc = fc.expect("muted-all path still needs a graph (anullsrc)");
        assert!(fc.contains("anullsrc"), "graph: {fc}");
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_single_track_non_unity_emits_volume_filter() {
        let mix = vec![gain(0, 0.5)];
        let (fc, map) = build_audio_filter_complex(&mix, 1, "");
        let fc = fc.expect("non-unity volume requires a filter");
        assert!(fc.contains("volume=0.5000"), "graph: {fc}");
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_multi_track_explicit_mix_amixes_active() {
        let mix = vec![gain(0, 1.0), gain(1, 0.5), gain(2, 0.0)];
        let (fc, map) = build_audio_filter_complex(&mix, 3, "");
        let fc = fc.expect("multi-track mix requires a graph");
        // track 2 was muted → only tracks 0 + 1 should be in the amix.
        assert!(fc.contains("amix=inputs=2"), "expected 2-input amix, graph: {fc}");
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_returned_graph_is_never_empty_when_some() {
        // Property: if the function returns Some(graph), the graph string
        // must be non-empty. (The bug was Some("") sneaking through.)
        let cases: Vec<(Vec<TrackGain>, usize, &str)> = vec![
            (vec![], 1, ""),
            (vec![], 3, ""),
            (vec![gain(0, 1.0)], 1, ""),
            (vec![gain(0, 1.0)], 1, ",loudnorm"),
            (vec![gain(0, 0.5)], 1, ""),
            (vec![gain(0, 0.0)], 1, ""),
            (vec![gain(0, 1.0), gain(1, 1.0)], 2, ""),
            (vec![gain(0, 1.0), gain(1, 0.5), gain(2, 0.0)], 3, ",atempo=2.0"),
        ];
        for (mix, total, post) in cases {
            let (fc, _map) = build_audio_filter_complex(&mix, total, post);
            if let Some(g) = fc {
                assert!(
                    !g.is_empty(),
                    "build_audio_filter_complex returned Some(\"\") for mix={mix:?} total={total} post={post:?}"
                );
            }
        }
    }
}
