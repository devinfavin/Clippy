use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

const DIAG_CAP: usize = 1000;

/// In-memory ring buffer of timestamped log entries. Bounded so it can't
/// grow unboundedly over a long session. Never written to disk and never sent
/// anywhere — the user copies it explicitly via the "Copy diagnostics" button.
pub struct DiagLog(pub Arc<Mutex<VecDeque<String>>>);

impl Default for DiagLog {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagLog {
    pub fn new() -> Self {
        DiagLog(Arc::new(Mutex::new(VecDeque::with_capacity(DIAG_CAP))))
    }
}

/// Opt-in switch for "verbose" diag logging. When OFF (default), the
/// coordinator redacts non-game window titles so a copy-pasted diag report
/// never contains a sensitive browser tab / document name. Enable only when
/// actively reproducing a window-routing bug.
pub struct DiagVerbose(pub std::sync::atomic::AtomicBool);

impl DiagVerbose {
    pub fn enabled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[tauri::command]
pub fn set_diag_verbose(state: tauri::State<'_, DiagVerbose>, enabled: bool) {
    state.0.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

/// Frontend-callable diag entry. Lets any renderer-side hook (notably the
/// in-game overlay window, which can't easily share console output) emit a
/// line into the canonical diag log so the user can copy-paste it for
/// post-mortem debugging. Caps the message at 512 chars to bound RAM
/// pressure on the ring buffer.
#[tauri::command]
pub fn frontend_diag(app: AppHandle, msg: String) {
    let truncated = if msg.chars().count() > 512 {
        msg.chars().take(512).collect::<String>()
    } else {
        msg
    };
    diag(&app, format!("[fe] {truncated}"));
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

/// Re-export of `epoch_to_ymd` for replay::mod.rs to build filesystem-safe
/// timestamp slugs without duplicating the civil-from-days logic.
pub fn epoch_to_ymd_for_filename(secs: u64) -> (i32, u32, u32) {
    epoch_to_ymd(secs)
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
pub(crate) fn persist_diag_log(app: &AppHandle) {
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
pub fn diag(app: &AppHandle, msg: impl std::fmt::Display) {
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

/// Open the OS file manager with the persisted `diagnostics.log` selected.
/// Lets a user attach prior-session logs to a bug report without having to
/// hunt down the app data dir. The in-memory ring buffer's contents are
/// only flushed to this file on graceful exit, so the *current* session's
/// log lives in memory (use the "Copy log" affordance for that one).
#[tauri::command]
pub fn reveal_diagnostics_log(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let path = dir.join("diagnostics.log");
    if !path.exists() {
        // Nothing persisted yet — reveal the containing folder so the user
        // sees where the file will land once a session exits gracefully.
        let _ = std::fs::create_dir_all(&dir);
        return crate::storage::reveal_in_folder(dir.to_string_lossy().into_owned());
    }
    crate::storage::reveal_in_folder(path.to_string_lossy().into_owned())
}

/// Delete the persisted diagnostics.log. The in-memory ring buffer is
/// untouched — it'll be re-flushed on the next graceful exit. Useful when
/// a user wants to reset state before reproducing a bug.
#[tauri::command]
pub fn clear_diagnostics_log(app: AppHandle) -> Result<u64, String> {
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

/// Return the in-memory diagnostic log as a plain-text string. Called only
/// when the user explicitly clicks "Copy diagnostics" — never sent anywhere
/// automatically. Full file paths are never logged; only basenames are used.
#[tauri::command]
pub fn get_diagnostics(app: AppHandle) -> String {
    use crate::replay;
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut out = String::new();
    out.push_str(&format!(
        "Clippy v{} ({}) — diagnostic snapshot\n",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "dev build"
        } else {
            "release"
        }
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
    let allowlist_size = replay_state.allowlist.lock().map(|g| g.len()).unwrap_or(0);
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
        guard
            .as_ref()
            .map(|c| c.perf_snapshot())
            .unwrap_or_default()
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
