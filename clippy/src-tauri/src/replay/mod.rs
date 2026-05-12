pub mod audio;
pub mod buffer;
#[cfg(windows)]
pub mod process_loopback;
pub mod capture;
pub mod convert;
pub mod coordinator;
pub mod encoder;
pub mod focus;
pub mod games;
pub mod save;
pub mod sysinfo;
pub mod vproc;
pub mod worker;

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

// ---------- settings & status ----------

/// How to choose the encoder when multiple hardware MFTs are present.
/// `Auto` lets MF's `MFT_ENUM_FLAG_SORTANDFILTER` pick (its preference
/// matches the active GPU vendor in practice). The other variants force
/// a friendly-name substring match in `MFTEnumEx`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EncoderPreference {
    Auto,
    Nvenc,
    Amf,
    Qsv,
    Software,
}

impl Default for EncoderPreference {
    fn default() -> Self { EncoderPreference::Auto }
}

/// Output resolution selector. Source keeps the captured surface's pixels
/// (modulo the 16-pixel macroblock alignment). Half halves both axes
/// (still 16-aligned). Custom takes explicit width/height from the UI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResolutionMode {
    Source,
    Half,
    Custom { width: u32, height: u32 },
}

impl Default for ResolutionMode {
    fn default() -> Self { ResolutionMode::Source }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaySettings {
    pub duration_secs: u32,
    /// User-selected output devices to capture as separate tracks. Empty
    /// means the worker falls back to the default render endpoint.
    pub audio_device_ids: Vec<String>,
    /// Optional friendly track names for each audio_device_ids entry, parallel
    /// by index. Empty string means "no custom name; use system device name".
    pub audio_device_names: Vec<String>,
    pub video_bitrate_kbps: u32,
    /// Try Process Loopback (Win11 22H2+) for the focused window so the
    /// saved file has only the game's own audio. Falls back silently to
    /// system loopback on Win10 or activation failure.
    pub use_process_loopback: bool,
    /// Frame rate the encoder runs at. Pacing duplicates the last NV12
    /// frame when WGC has nothing new so playback timing stays correct
    /// even for capped/static games.
    pub fps: u32,
    pub resolution_mode: ResolutionMode,
    pub encoder_preference: EncoderPreference,
    /// Distance between H.264 keyframes in seconds. Smaller GOPs let the
    /// save trim closer to the user's chosen window; larger GOPs are
    /// slightly cheaper to encode. `None` lets the encoder pick.
    pub keyframe_interval_secs: Option<u32>,
    /// Per-window mode only: ceiling on simultaneously-captured games.
    /// On focus change at the cap, the LRU worker is evicted.
    pub max_concurrent_workers: u32,
}

impl Default for ReplaySettings {
    fn default() -> Self {
        Self {
            duration_secs: 300,
            audio_device_ids: Vec::new(),
            audio_device_names: Vec::new(),
            video_bitrate_kbps: 25_000,
            use_process_loopback: true,
            fps: 60,
            resolution_mode: ResolutionMode::Source,
            encoder_preference: EncoderPreference::Auto,
            keyframe_interval_secs: Some(2),
            max_concurrent_workers: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum ReplayStatus {
    /// Replay buffer is off — `replay_start` hasn't been called.
    Idle,
    /// Buffer is running but nothing is being captured right now: the user
    /// is focused on a window that isn't in the game allowlist (Clippy
    /// itself, Discord, a browser, etc.). The buffer wakes up the moment
    /// the user focuses a game.
    Watching,
    /// Capturing a game window or a chosen monitor.
    Active {
        window_title: String,
        buffered_secs: u32,
        vram_mb: u32,
    },
    Saving,
}

// ---------- managed state ----------

/// Holds the coordinator while a replay session is active. None when stopped.
/// The game allowlist is shared across sessions (lives on app startup).
pub struct ReplayState {
    pub coord: Arc<Mutex<Option<coordinator::Coordinator>>>,
    pub allowlist: Arc<Mutex<games::GameAllowlist>>,
    /// Device-id → friendly-name map kept fresh by the frontend via
    /// `replay_set_audio_names`. Consulted at save time to populate MP4
    /// stream metadata, so renaming a device in Settings during a buffer
    /// session takes effect on the *next* save without needing to restart
    /// the running workers.
    pub audio_names: Arc<Mutex<std::collections::HashMap<String, String>>>,
}

impl ReplayState {
    pub fn new() -> Self {
        ReplayState {
            coord: Arc::new(Mutex::new(None)),
            allowlist: Arc::new(Mutex::new(games::GameAllowlist::new())),
            audio_names: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }
}

/// If `state.coord` holds a coordinator whose thread has already exited,
/// drop it so subsequent calls behave as if the buffer is stopped. Called
/// from every entry point that consults the coord so a dead slot can't
/// strand `replay_start` or hang `replay_save`.
fn prune_dead_coord(state: &ReplayState) {
    let mut guard = match state.coord.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if let Some(c) = guard.as_ref() {
        if !c.is_alive() {
            *guard = None;
        }
    }
}

// ---------- Tauri commands ----------

#[tauri::command]
pub fn get_replay_status(state: tauri::State<'_, ReplayState>) -> ReplayStatus {
    // Discard a dead coordinator so the UI reports Idle and `replay_start`
    // can spawn a fresh one.
    prune_dead_coord(&state);
    let guard = match state.coord.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    match guard.as_ref() {
        Some(c) => match c.status() {
            // The coordinator's inner status is `Idle` whenever there is no
            // worker actively capturing (e.g. user is on a non-game window).
            // Override with `Watching` so the UI can tell that the buffer
            // itself IS running — just waiting for a game to focus.
            ReplayStatus::Idle => ReplayStatus::Watching,
            other => other,
        },
        None => ReplayStatus::Idle,
    }
}

/// Capture mode for `replay_start`. Defaults to per-window when omitted.
/// JSON shape:
///   { "kind": "perWindow" }
///   { "kind": "monitor", "hmonitor": "12345678" }
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CaptureModeArg {
    PerWindow,
    Monitor { hmonitor: String },
}

/// Start the replay buffer. Defaults to per-window mode with focus tracking
/// and the game allowlist; `captureMode: { kind: "monitor", hmonitor }`
/// switches to full-screen mode on the chosen display.
#[tauri::command]
pub fn replay_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, ReplayState>,
    duration_secs: Option<u32>,
    bitrate_kbps: Option<u32>,
    capture_mode: Option<CaptureModeArg>,
    audio_device_ids: Option<Vec<String>>,
    audio_device_names: Option<Vec<String>>,
    use_process_loopback: Option<bool>,
    fps: Option<u32>,
    resolution_mode: Option<ResolutionMode>,
    encoder_preference: Option<EncoderPreference>,
    keyframe_interval_secs: Option<Option<u32>>,
    max_concurrent_workers: Option<u32>,
) -> Result<(), String> {
    {
        // Log dead-coord eviction here so the user gets a clear breadcrumb
        // even though `prune_dead_coord` itself is silent.
        let mut guard = state.coord.lock().map_err(|e| e.to_string())?;
        if let Some(c) = guard.as_ref() {
            if !c.is_alive() {
                crate::diag(
                    &app,
                    "[replay] previous coordinator is dead — discarding before restart",
                );
                *guard = None;
            }
        }
    }
    let mut guard = state.coord.lock().map_err(|e| e.to_string())?;
    if guard.is_some() {
        return Err("replay buffer is already running".into());
    }

    let mode = match capture_mode.unwrap_or(CaptureModeArg::PerWindow) {
        CaptureModeArg::PerWindow => {
            // Refresh allowlist only for per-window mode.
            let mut list = state.allowlist.lock().map_err(|e| e.to_string())?;
            *list = games::GameAllowlist::new();
            list.extend(games::scan_launchers());
            list.extend(games::load_manual_entries(&allowlist_file(&app)?));
            coordinator::CaptureMode::PerWindow
        }
        CaptureModeArg::Monitor { hmonitor } => {
            let h: isize = hmonitor.parse().map_err(|e| format!("bad hmonitor: {e}"))?;
            coordinator::CaptureMode::Monitor(h)
        }
    };

    let settings = ReplaySettings {
        duration_secs: duration_secs.unwrap_or(300).clamp(10, 600),
        audio_device_ids: audio_device_ids.unwrap_or_default(),
        audio_device_names: audio_device_names.unwrap_or_default(),
        video_bitrate_kbps: bitrate_kbps.unwrap_or(25_000).clamp(1_000, 200_000),
        use_process_loopback: use_process_loopback.unwrap_or(true),
        fps: fps.unwrap_or(60).clamp(15, 240),
        resolution_mode: resolution_mode.unwrap_or(ResolutionMode::Source),
        encoder_preference: encoder_preference.unwrap_or(EncoderPreference::Auto),
        keyframe_interval_secs: keyframe_interval_secs.unwrap_or(Some(2)),
        max_concurrent_workers: max_concurrent_workers.unwrap_or(3).clamp(1, 10),
    };

    let coord = coordinator::Coordinator::start(
        settings.clone(),
        mode,
        Arc::clone(&state.allowlist),
        app.clone(),
    )?;
    *guard = Some(coord);
    crate::diag(
        &app,
        format!(
            "[replay] replay_start invoked · duration={}s bitrate={}kbps audio_devices={}",
            settings.duration_secs,
            settings.video_bitrate_kbps,
            settings.audio_device_ids.len()
        ),
    );
    Ok(())
}

/// Enumerate monitors so the frontend can populate a "capture display"
/// dropdown in settings.
#[tauri::command]
pub fn replay_list_monitors() -> Vec<capture::MonitorInfo> {
    capture::list_monitors()
}

// ---------- allowlist management ----------

/// Path where manual game-allowlist additions are persisted.
fn allowlist_file(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?;
    Ok(dir.join("game_allowlist.json"))
}

/// Sidecar file holding the last N games added to the allowlist (newest
/// first). Independent of the main allowlist — exists purely to power the
/// "Recently added" quick-glance UI in Settings.
const RECENT_ADDS_LIMIT: usize = 5;
fn recent_adds_file(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?;
    Ok(dir.join("recent_adds.json"))
}

fn load_recent_adds(app: &tauri::AppHandle) -> Vec<String> {
    let Ok(path) = recent_adds_file(app) else { return Vec::new() };
    let Ok(s) = std::fs::read_to_string(&path) else { return Vec::new() };
    serde_json::from_str::<Vec<String>>(&s).unwrap_or_default()
}

fn save_recent_adds(app: &tauri::AppHandle, entries: &[String]) {
    let Ok(path) = recent_adds_file(app) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(&path, json);
    }
}

/// Push `path` to the front of the recents list, dedupe, cap at 5.
fn track_recent_add(app: &tauri::AppHandle, path: &str) {
    let mut entries = load_recent_adds(app);
    // Compare case-insensitively; the allowlist already normalizes paths so
    // two different cases of the same file shouldn't both occupy slots.
    let lower = path.to_lowercase();
    entries.retain(|e| e.to_lowercase() != lower);
    entries.insert(0, path.to_string());
    entries.truncate(RECENT_ADDS_LIMIT);
    save_recent_adds(app, &entries);
}

/// Remove `path` from the recents list (called on game removal so the
/// quick-glance list doesn't dangle entries you've just deleted).
fn untrack_recent_add(app: &tauri::AppHandle, path: &str) {
    let mut entries = load_recent_adds(app);
    let lower = path.to_lowercase();
    let before = entries.len();
    entries.retain(|e| e.to_lowercase() != lower);
    if entries.len() != before {
        save_recent_adds(app, &entries);
    }
}

#[tauri::command]
pub fn replay_recent_games(app: tauri::AppHandle) -> Vec<String> {
    load_recent_adds(&app)
}

#[tauri::command]
pub fn replay_list_games(state: tauri::State<'_, ReplayState>) -> Vec<String> {
    match state.allowlist.lock() {
        Ok(g) => g.entries(),
        Err(e) => e.into_inner().entries(),
    }
}

/// Force a re-scan of game launcher install dirs and merge with manual
/// entries. Returns the new entry count.
#[tauri::command]
pub fn replay_rescan_games(
    app: tauri::AppHandle,
    state: tauri::State<'_, ReplayState>,
) -> Result<usize, String> {
    let manual_path = allowlist_file(&app)?;
    let mut list = state.allowlist.lock().map_err(|e| e.to_string())?;
    *list = games::GameAllowlist::new();
    list.extend(games::scan_launchers());
    list.extend(games::load_manual_entries(&manual_path));
    Ok(list.len())
}

/// Add an executable path to the allowlist and persist it.
#[tauri::command]
pub fn replay_add_game(
    app: tauri::AppHandle,
    state: tauri::State<'_, ReplayState>,
    exe_path: String,
) -> Result<(), String> {
    let path = std::path::PathBuf::from(&exe_path);
    if !path.exists() {
        return Err(format!("path does not exist: {exe_path}"));
    }
    {
        let mut list = state.allowlist.lock().map_err(|e| e.to_string())?;
        list.add(&path);
    }
    persist_manual(&app, &state)?;
    track_recent_add(&app, &exe_path);
    Ok(())
}

/// Add the executable of the currently-focused window. Useful when the user
/// triggers this from a tray icon / global hotkey while a game is focused.
/// Returns the path that was added.
#[tauri::command]
pub fn replay_add_current_game(
    app: tauri::AppHandle,
    state: tauri::State<'_, ReplayState>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        let h = unsafe { GetForegroundWindow() };
        if h.0.is_null() {
            return Err("no foreground window".into());
        }
        let exe = games::resolve_window_exe(h.0 as isize)
            .ok_or_else(|| "could not resolve foreground window's process".to_string())?;
        {
            let mut list = state.allowlist.lock().map_err(|e| e.to_string())?;
            list.add(&exe);
        }
        persist_manual(&app, &state)?;
        let exe_str = exe.to_string_lossy().into_owned();
        track_recent_add(&app, &exe_str);
        Ok(exe_str)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, state);
        Err("Windows only".into())
    }
}

#[tauri::command]
pub fn replay_remove_game(
    app: tauri::AppHandle,
    state: tauri::State<'_, ReplayState>,
    exe_path: String,
) -> Result<bool, String> {
    let removed = {
        let mut list = state.allowlist.lock().map_err(|e| e.to_string())?;
        list.remove(&std::path::PathBuf::from(&exe_path))
    };
    persist_manual(&app, &state)?;
    untrack_recent_add(&app, &exe_path);
    Ok(removed)
}

/// Persist only manual entries (not launcher-scanned ones — those re-derive
/// from disk every session so they stay accurate as games install/uninstall).
fn persist_manual(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, ReplayState>,
) -> Result<(), String> {
    let path = allowlist_file(app)?;
    let scanned: std::collections::HashSet<String> = games::scan_launchers()
        .iter()
        .map(|p| p.to_string_lossy().to_lowercase().replace('/', "\\"))
        .collect();
    let manual: Vec<std::path::PathBuf> = {
        let list = state.allowlist.lock().map_err(|e| e.to_string())?;
        list.entries()
            .into_iter()
            .filter(|s| !scanned.contains(s))
            .map(std::path::PathBuf::from)
            .collect()
    };
    games::save_manual_entries(&path, &manual).map_err(|e| format!("persist allowlist: {e}"))
}

#[tauri::command]
pub fn replay_stop(
    app: tauri::AppHandle,
    state: tauri::State<'_, ReplayState>,
) -> Result<(), String> {
    let mut guard = state.coord.lock().map_err(|e| e.to_string())?;
    if let Some(c) = guard.take() {
        c.stop()?;
        crate::diag(&app, "[replay] replay_stop invoked");
    }
    Ok(())
}

/// Result of a successful save. The path is the only thing the frontend
/// needs to load the clip; `window_title` is purely informational (used in
/// diagnostics + toast labels).
#[derive(Debug, Clone, Serialize)]
pub struct ReplaySaveResult {
    pub path: String,
    pub window_title: String,
}

/// Snapshot the buffer for whichever window is currently focused, then mux
/// to an MP4 in the temp directory.
#[tauri::command]
pub async fn replay_save(
    app: tauri::AppHandle,
    state: tauri::State<'_, ReplayState>,
) -> Result<ReplaySaveResult, String> {
    let snap = take_snapshot(&state)?;
    finish_save(&app, snap).await
}

/// Same flow as `replay_save`, but callable from non-command contexts (e.g.
/// the global hotkey handler in `lib.rs`).
pub async fn save_active(app: &tauri::AppHandle) -> Result<ReplaySaveResult, String> {
    use tauri::Manager;
    let state = app.state::<ReplayState>();
    let snap = take_snapshot(&state)?;
    finish_save(app, snap).await
}

fn take_snapshot(state: &ReplayState) -> Result<coordinator::SaveSnapshot, String> {
    prune_dead_coord(state);
    let guard = state.coord.lock().map_err(|e| e.to_string())?;
    let c = guard.as_ref().ok_or("replay buffer is not running")?;
    let snap = c.snapshot()?;
    if snap.packets.is_empty() {
        return Err("active window's buffer is empty — give it a few seconds of capture first".into());
    }
    Ok(snap)
}

async fn finish_save(
    app: &tauri::AppHandle,
    mut snap: coordinator::SaveSnapshot,
) -> Result<ReplaySaveResult, String> {
    use tauri::Manager;

    // Refresh each captured-track's friendly name from the live audio_names
    // map before muxing. This is what makes "rename a device after Start"
    // work — the worker's spawn-time names may be stale, but the map gets
    // updated on every frontend rename, so the saved MP4's stream metadata
    // always reflects the user's latest choice. Tracks with no device_id
    // (process-loopback "Game audio", fallback "Default output") keep
    // their spawn-time labels untouched.
    if let Some(rs) = app.try_state::<ReplayState>() {
        if let Ok(map) = rs.audio_names.lock() {
            for track in snap.audio_tracks.iter_mut() {
                if track.device_id.is_empty() {
                    continue;
                }
                if let Some(current) = map.get(&track.device_id) {
                    if !current.is_empty() {
                        track.name = current.clone();
                    }
                }
            }
        }
    }

    let dir = match app.try_state::<crate::ReplaySaveDir>() {
        Some(s) => s.0.lock().map_err(|e| e.to_string())?.clone(),
        None => default_save_dir(app),
    };
    // Best-effort: create the directory if it disappeared since startup
    // (user deleted it, network drive offline, etc.).
    std::fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "couldn't create replay save folder {}: {e}",
            dir.display()
        )
    })?;

    let ts_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamp = filename_stamp(ts_secs);
    let title_slug = safe_filename_slug(&snap.window_title);
    let filename = if title_slug.is_empty() {
        format!("Clippy_Replay_{stamp}.mp4")
    } else {
        format!("Clippy_Replay_{title_slug}_{stamp}.mp4")
    };
    let out = dir.join(filename);

    // Surface dropped audio tracks in the diag log. write_and_mux silently
    // skips tracks that captured zero packets (e.g. an idle Sonar Mic / Aux
    // output that wasn't producing audio during the buffer window) — without
    // a log entry the user just sees "I picked 4 devices but only got 2
    // tracks in my clip" with no explanation. Listing the skipped ones by
    // name + index gives them a hint to either remove them from the
    // selection or actually route audio through them.
    let total = snap.audio_tracks.len();
    let dropped: Vec<String> = snap
        .audio_tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.packets.is_empty())
        .map(|(i, t)| {
            if t.name.is_empty() {
                format!("track {i}")
            } else {
                format!("\"{}\" (track {i})", t.name)
            }
        })
        .collect();
    if !dropped.is_empty() {
        crate::diag(
            app,
            format!(
                "[replay] save · {} of {total} audio track(s) dropped (no captured packets): {}",
                dropped.len(),
                dropped.join(", ")
            ),
        );
    }

    // Save context line: what we're about to mux. Pinpoints "saved file
    // says X" vs "buffer contained Y" mismatches without needing to
    // reproduce the bug.
    let video_pkt_count = snap.packets.len();
    let audio_pkt_summary: String = snap
        .audio_tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let label = if t.name.is_empty() {
                format!("a{i}")
            } else {
                format!("a{i}=\"{}\"", t.name)
            };
            format!("{label}:{}pkts", t.packets.len())
        })
        .collect::<Vec<_>>()
        .join(" ");
    crate::diag(
        app,
        format!(
            "[replay] save · target=\"{}\" video={video_pkt_count}pkts @ {}fps · audio: {audio_pkt_summary}",
            snap.window_title, snap.fps
        ),
    );

    let timings = save::write_and_mux(&snap.packets, &snap.audio_tracks, snap.fps, &out).await?;

    // Save timing breakdown: lets us see *where* a slow save is slow
    // (snapshot copy / disk write / ffmpeg mux) without guessing.
    crate::diag(
        app,
        format!(
            "[replay] save · h264 {}ms ({:.1}MB) · pcm {}ms ({:.1}MB) · bsf {}ms · ffmpeg {}ms · total {}ms",
            timings.h264_write_ms,
            timings.h264_bytes as f64 / (1024.0 * 1024.0),
            timings.pcm_write_ms,
            timings.pcm_bytes as f64 / (1024.0 * 1024.0),
            timings.bsf_pass_ms,
            timings.ffmpeg_mux_ms,
            timings.total_ms,
        ),
    );

    Ok(ReplaySaveResult {
        path: out.to_string_lossy().into_owned(),
        window_title: snap.window_title,
    })
}

/// Default save dir for replay clips. Matches the ShadowPlay convention of
/// `Videos/<AppName>` so users find their clips alongside their other
/// gameplay captures. Falls back to the app data dir if Videos isn't
/// resolvable (rare — usually means a locked-down kiosk profile).
pub(crate) fn default_save_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    use tauri::Manager;
    if let Ok(videos) = app.path().video_dir() {
        return videos.join("Clippy Replays");
    }
    if let Ok(data) = app.path().app_data_dir() {
        return data.join("replays");
    }
    std::env::temp_dir().join("clippy-replays")
}

/// Where the chosen save dir is persisted across sessions.
fn save_dir_pref_file(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?;
    Ok(dir.join("save_dir.txt"))
}

/// Load the persisted save dir, falling back to the default. Called once
/// at startup to initialize `ReplaySaveDir`.
pub(crate) fn load_save_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    if let Ok(pref) = save_dir_pref_file(app) {
        if let Ok(s) = std::fs::read_to_string(&pref) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                let p = std::path::PathBuf::from(trimmed);
                return p;
            }
        }
    }
    default_save_dir(app)
}

/// Strip / replace characters that aren't valid in Windows filenames so
/// game-window titles can be embedded directly into the saved MP4 name.
/// Result is also truncated to 60 chars to keep file paths reasonable.
fn safe_filename_slug(input: &str) -> String {
    // Forbidden on Windows: < > : " / \ | ? *  and control chars 0..31
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => out.push('-'),
            c if (c as u32) < 32 => {}
            c => out.push(c),
        }
    }
    // Collapse whitespace runs to a single underscore for readability.
    let collapsed: String = out
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_");
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c == '-' || c == '_');
    let mut s = trimmed.to_string();
    if s.chars().count() > 60 {
        s = s.chars().take(60).collect();
    }
    s
}

/// Filesystem-safe `YYYY-MM-DD_HH-MM-SS` stamp built from a Unix epoch.
fn filename_stamp(secs: u64) -> String {
    let (y, mo, d) = crate::epoch_to_ymd_for_filename(secs);
    let h = (secs / 3600) % 24;
    let mi = (secs / 60) % 60;
    let s = secs % 60;
    format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}

// ----- save dir Tauri commands -----

#[tauri::command]
pub fn replay_get_save_dir(
    state: tauri::State<'_, crate::ReplaySaveDir>,
) -> Result<String, String> {
    let g = state.0.lock().map_err(|e| e.to_string())?;
    Ok(g.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn replay_set_save_dir(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::ReplaySaveDir>,
    path: String,
) -> Result<(), String> {
    let p = std::path::PathBuf::from(path.trim());
    if p.as_os_str().is_empty() {
        return Err("path must not be empty".into());
    }
    std::fs::create_dir_all(&p)
        .map_err(|e| format!("can't create {}: {e}", p.display()))?;
    {
        let mut g = state.0.lock().map_err(|e| e.to_string())?;
        *g = p.clone();
    }
    if let Ok(pref) = save_dir_pref_file(&app) {
        if let Some(parent) = pref.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&pref, p.to_string_lossy().as_bytes());
    }
    crate::diag(&app, format!("[replay] save dir set to {}", p.display()));
    Ok(())
}

#[tauri::command]
pub fn replay_reset_save_dir(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::ReplaySaveDir>,
) -> Result<String, String> {
    let def = default_save_dir(&app);
    std::fs::create_dir_all(&def)
        .map_err(|e| format!("can't create default {}: {e}", def.display()))?;
    {
        let mut g = state.0.lock().map_err(|e| e.to_string())?;
        *g = def.clone();
    }
    if let Ok(pref) = save_dir_pref_file(&app) {
        let _ = std::fs::remove_file(&pref);
    }
    crate::diag(
        &app,
        format!("[replay] save dir reset to default {}", def.display()),
    );
    Ok(def.to_string_lossy().into_owned())
}

/// Push the current device-id → friendly-name map down to the backend so a
/// rename in Settings reaches the next save without restarting the buffer.
/// The frontend calls this on first mount and after any device-name edit.
#[tauri::command]
pub fn replay_set_audio_names(
    state: tauri::State<'_, ReplayState>,
    names: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let mut g = state.audio_names.lock().map_err(|e| e.to_string())?;
    *g = names;
    Ok(())
}

/// Enumerate WASAPI render endpoints. Used by the settings UI to pick which
/// audio devices to capture as separate tracks.
#[tauri::command]
pub fn replay_list_audio_devices() -> Vec<audio::AudioDevice> {
    audio::enumerate_render_devices()
}

/// One-shot probe of the system's GPU + RAM + available HW H.264 encoders.
/// The frontend's resource-impact panel calls this once on mount.
#[tauri::command]
pub fn replay_get_system_info() -> sysinfo::SystemInfo {
    sysinfo::collect()
}

/// Phase 1 end-to-end pipeline test.
///
/// Captures ~90 frames from the current foreground window, converts BGRA→NV12
/// in software, encodes with Media Foundation H.264, muxes with FFmpeg, and
/// returns the path to the output MP4. Invoke from Tauri devtools:
///
///   await window.__TAURI__.core.invoke('replay_poc_test')
///
/// Open the returned path in Clippy to verify the video plays.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn replay_poc_test() -> Result<String, String> {
    // All COM / D3D11 / MF objects must live and die on a single thread.
    // spawn_blocking gives us that guarantee. The closure captures nothing,
    // so the Send bound on the closure is trivially satisfied.
    let (h264_bytes, frame_count, width, height) =
        tokio::task::spawn_blocking(poc_capture_and_encode)
            .await
            .map_err(|e| format!("task join: {e}"))??;

    // --- async I/O: write raw H.264 then mux to MP4 ---
    let tmp = std::env::temp_dir();
    let h264_path = tmp.join("clippy_poc.h264");
    let mp4_path = tmp.join("clippy_poc.mp4");

    tokio::fs::write(&h264_path, &h264_bytes)
        .await
        .map_err(|e| format!("write h264: {e}"))?;

    let ffmpeg = save::ffmpeg_path()?;

    let out = tokio::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-framerate",
            "60",
            "-f",
            "h264",
            "-i",
            h264_path.to_str().unwrap(),
            "-c:v",
            "copy",
            mp4_path.to_str().unwrap(),
        ])
        .output()
        .await
        .map_err(|e| format!("ffmpeg spawn ({ffmpeg:?}): {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    Ok(format!(
        "OK — {frame_count} frames ({}×{}) → {}",
        width,
        height,
        mp4_path.display()
    ))
}

/// Synchronous capture + encode. Runs inside `spawn_blocking` so COM objects
/// stay on one thread for their entire lifetime.
#[cfg(all(debug_assertions, windows))]
fn poc_capture_and_encode() -> Result<(Vec<u8>, usize, u32, u32), String> {
    use capture::windows_impl::*;
    use convert::windows_impl::*;
    use encoder::windows_impl::*;
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return Err("no foreground window".into());
    }

    let bundle = create_d3d11_device().map_err(|e| format!("D3D11: {e}"))?;
    let item = capture_item_for_hwnd(hwnd).map_err(|e| format!("WGC item: {e}"))?;
    let size = item.Size().map_err(|e| format!("item size: {e}"))?;
    let (width, height) = (size.Width as u32, size.Height as u32);

    // H.264 requires dimensions aligned to 16-pixel macroblock boundaries.
    let enc_width = (width + 15) & !15u32;
    let enc_height = (height + 15) & !15u32;

    let session = open_capture_session(hwnd, &bundle.device)
        .map_err(|e| format!("WGC session: {e}"))?;

    mf_startup().map_err(|e| format!("MFStartup: {e}"))?;

    let encoder = create_h264_encoder_simple(enc_width, enc_height, 10_000, 60, 1)
        .map_err(|e| format!("encoder: {e}"))?;

    const TARGET_FRAMES: usize = 90;
    const MAX_ATTEMPTS: usize = 600;

    let frame_duration_pts: i64 = 10_000_000 / 60;
    let mut all_h264: Vec<u8> = Vec::with_capacity(4 * 1024 * 1024);
    let mut frame_count = 0usize;
    let mut attempts = 0usize;

    while frame_count < TARGET_FRAMES && attempts < MAX_ATTEMPTS {
        if let Ok(frame) = session.frame_pool.TryGetNextFrame() {
            let texture = extract_texture_from_frame(&frame)
                .map_err(|e| format!("extract f{frame_count}: {e}"))?;

            let bgra = readback_bgra_texture(
                &bundle.device,
                &bundle.context,
                &texture,
                width,
                height,
            )
            .map_err(|e| format!("readback f{frame_count}: {e}"))?;

            let nv12 = bgra_to_nv12(&bgra, width as usize, height as usize, enc_width as usize, enc_height as usize);
            let pts = frame_count as i64 * frame_duration_pts;

            submit_nv12_frame(&encoder, &nv12, pts, frame_duration_pts)
                .map_err(|e| format!("submit f{frame_count}: {e}"))?;

            let packets =
                drain_encoder(&encoder).map_err(|e| format!("drain f{frame_count}: {e}"))?;
            for (data, _, _) in packets {
                all_h264.extend_from_slice(&data);
            }

            frame_count += 1;
        }

        std::thread::sleep(std::time::Duration::from_millis(16));
        attempts += 1;
    }

    let final_packets = flush_encoder(&encoder).map_err(|e| format!("flush: {e}"))?;
    for (data, _, _) in final_packets {
        all_h264.extend_from_slice(&data);
    }

    session.session.Close().ok();
    mf_shutdown();

    if all_h264.is_empty() {
        return Err(format!(
            "no encoded output after {frame_count} frames — encoder rejected input"
        ));
    }

    Ok((all_h264, frame_count, enc_width, enc_height))
}

#[cfg(all(debug_assertions, not(windows)))]
fn poc_capture_and_encode() -> Result<(Vec<u8>, usize, u32, u32), String> {
    Err("replay buffer is Windows-only".into())
}

// ---------- Stage A: GPU color conversion validation ----------

/// Stage A PoC: validates the D3D11 video processor (BGRA→NV12 on GPU)
/// while still using the software encoder for the encode step. NV12 is read
/// back from GPU just for the encoder hand-off — once Stage B lands, the
/// readback goes away entirely. Output should look identical to the CPU PoC.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn replay_poc_gpu_convert() -> Result<String, String> {
    let (h264_bytes, frame_count, width, height) =
        tokio::task::spawn_blocking(poc_gpu_convert_and_encode)
            .await
            .map_err(|e| format!("task join: {e}"))??;

    let tmp = std::env::temp_dir();
    let h264_path = tmp.join("clippy_poc_gpu.h264");
    let mp4_path = tmp.join("clippy_poc_gpu.mp4");

    tokio::fs::write(&h264_path, &h264_bytes)
        .await
        .map_err(|e| format!("write h264: {e}"))?;

    let ffmpeg = save::ffmpeg_path()?;

    let out = tokio::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-framerate",
            "60",
            "-f",
            "h264",
            "-i",
            h264_path.to_str().unwrap(),
            "-c:v",
            "copy",
            mp4_path.to_str().unwrap(),
        ])
        .output()
        .await
        .map_err(|e| format!("ffmpeg spawn ({ffmpeg:?}): {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    Ok(format!(
        "GPU-convert OK — {frame_count} frames ({width}×{height}) → {}",
        mp4_path.display()
    ))
}

#[cfg(all(debug_assertions, windows))]
fn poc_gpu_convert_and_encode() -> Result<(Vec<u8>, usize, u32, u32), String> {
    use capture::windows_impl::*;
    use encoder::windows_impl::*;
    use vproc::windows_impl::VideoProcessor;
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return Err("no foreground window".into());
    }

    let bundle = create_d3d11_device().map_err(|e| format!("D3D11: {e}"))?;
    let item = capture_item_for_hwnd(hwnd).map_err(|e| format!("WGC item: {e}"))?;
    let size = item.Size().map_err(|e| format!("item size: {e}"))?;
    let (width, height) = (size.Width as u32, size.Height as u32);

    let enc_width = (width + 15) & !15u32;
    let enc_height = (height + 15) & !15u32;

    let session = open_capture_session(hwnd, &bundle.device)
        .map_err(|e| format!("WGC session: {e}"))?;

    // Build the GPU video processor (BGRA src → NV12 dst, 16-aligned).
    let vp = VideoProcessor::new(
        &bundle.device,
        &bundle.context,
        width,
        height,
        enc_width,
        enc_height,
        60,
    )
    .map_err(|e| format!("video processor: {e}"))?;

    mf_startup().map_err(|e| format!("MFStartup: {e}"))?;
    let encoder = create_h264_encoder_simple(enc_width, enc_height, 10_000, 60, 1)
        .map_err(|e| format!("encoder: {e}"))?;

    const TARGET_FRAMES: usize = 90;
    const MAX_ATTEMPTS: usize = 600;

    let frame_duration_pts: i64 = 10_000_000 / 60;
    let mut all_h264: Vec<u8> = Vec::with_capacity(4 * 1024 * 1024);
    let mut frame_count = 0usize;
    let mut attempts = 0usize;

    while frame_count < TARGET_FRAMES && attempts < MAX_ATTEMPTS {
        if let Ok(frame) = session.frame_pool.TryGetNextFrame() {
            let bgra_tex = capture::windows_impl::extract_texture_from_frame(&frame)
                .map_err(|e| format!("extract f{frame_count}: {e}"))?;

            // GPU color conversion — no CPU readback of BGRA.
            vp.convert(&bgra_tex)
                .map_err(|e| format!("vproc f{frame_count}: {e}"))?;

            // Read back NV12 (Stage A only — Stage B feeds GPU texture directly).
            let nv12 = vp
                .readback_nv12(&bundle.context)
                .map_err(|e| format!("nv12 readback f{frame_count}: {e}"))?;

            let pts = frame_count as i64 * frame_duration_pts;
            submit_nv12_frame(&encoder, &nv12, pts, frame_duration_pts)
                .map_err(|e| format!("submit f{frame_count}: {e}"))?;

            let packets =
                drain_encoder(&encoder).map_err(|e| format!("drain f{frame_count}: {e}"))?;
            for (data, _, _) in packets {
                all_h264.extend_from_slice(&data);
            }

            frame_count += 1;
        }

        std::thread::sleep(std::time::Duration::from_millis(16));
        attempts += 1;
    }

    let final_packets = flush_encoder(&encoder).map_err(|e| format!("flush: {e}"))?;
    for (data, _, _) in final_packets {
        all_h264.extend_from_slice(&data);
    }

    session.session.Close().ok();
    mf_shutdown();

    if all_h264.is_empty() {
        return Err(format!(
            "no encoded output after {frame_count} frames"
        ));
    }

    Ok((all_h264, frame_count, enc_width, enc_height))
}

#[cfg(all(debug_assertions, not(windows)))]
fn poc_gpu_convert_and_encode() -> Result<(Vec<u8>, usize, u32, u32), String> {
    Err("Windows only".into())
}

// ---------- Stage B: full GPU + async hardware encoder ----------

/// Stage B PoC: production path. WGC → D3D11 BGRA → video processor NV12 →
/// async hardware H.264 encoder via DXGI surface buffer. Zero CPU readback.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn replay_poc_gpu_full() -> Result<String, String> {
    let (h264_bytes, frame_count, width, height, encoder_name) =
        tokio::task::spawn_blocking(poc_gpu_full)
            .await
            .map_err(|e| format!("task join: {e}"))??;

    let tmp = std::env::temp_dir();
    let h264_path = tmp.join("clippy_poc_full.h264");
    let mp4_path = tmp.join("clippy_poc_full.mp4");

    tokio::fs::write(&h264_path, &h264_bytes)
        .await
        .map_err(|e| format!("write h264: {e}"))?;

    let ffmpeg = save::ffmpeg_path()?;

    let out = tokio::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-framerate",
            "60",
            "-f",
            "h264",
            "-i",
            h264_path.to_str().unwrap(),
            "-c:v",
            "copy",
            mp4_path.to_str().unwrap(),
        ])
        .output()
        .await
        .map_err(|e| format!("ffmpeg spawn ({ffmpeg:?}): {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    Ok(format!(
        "Full GPU OK — {frame_count} frames ({width}×{height}) via {encoder_name} → {}",
        mp4_path.display()
    ))
}

#[cfg(all(debug_assertions, windows))]
fn poc_gpu_full() -> Result<(Vec<u8>, usize, u32, u32, String), String> {
    use capture::windows_impl::*;
    use encoder::windows_impl::*;
    use vproc::windows_impl::VideoProcessor;
    use windows::core::Interface;
    use windows::Win32::Media::MediaFoundation::{
        IMFMediaEventGenerator, METransformHaveOutput, METransformNeedInput,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return Err("no foreground window".into());
    }

    let bundle = create_d3d11_device().map_err(|e| format!("D3D11: {e}"))?;
    let item = capture_item_for_hwnd(hwnd).map_err(|e| format!("WGC item: {e}"))?;
    let size = item.Size().map_err(|e| format!("item size: {e}"))?;
    let (width, height) = (size.Width as u32, size.Height as u32);
    let enc_width = (width + 15) & !15u32;
    let enc_height = (height + 15) & !15u32;

    let session = open_capture_session(hwnd, &bundle.device)
        .map_err(|e| format!("WGC session: {e}"))?;

    let vp = VideoProcessor::new(
        &bundle.device,
        &bundle.context,
        width,
        height,
        enc_width,
        enc_height,
        60,
    )
    .map_err(|e| format!("video processor: {e}"))?;

    mf_startup().map_err(|e| format!("MFStartup: {e}"))?;

    // _device_manager must be kept alive for the encode session.
    let (encoder, _device_manager, _encoder_name) = create_h264_encoder_hw_async(
        &bundle.device,
        enc_width,
        enc_height,
        50_000,
        60,
        1,
        EncoderPreference::Auto,
        Some(2),
    )
    .map_err(|e| format!("HW encoder: {e}"))?;
    let event_gen: IMFMediaEventGenerator = encoder
        .cast()
        .map_err(|e| format!("event generator cast: {e}"))?;

    // Identify the encoder for the success message (best effort).
    let encoder_name = "Hardware MFT".to_string();

    const TARGET_FRAMES: usize = 90;
    const MAX_ATTEMPTS: usize = 600;
    let frame_duration_pts: i64 = 10_000_000 / 60;

    let mut all_h264: Vec<u8> = Vec::with_capacity(4 * 1024 * 1024);
    let mut frame_count = 0usize;
    let mut attempts = 0usize;

    // Outer loop: capture next frame, then submit it on the next NeedInput event
    // (draining HaveOutput events as they arrive between).
    while frame_count < TARGET_FRAMES && attempts < MAX_ATTEMPTS {
        // Pull a captured frame and convert it to NV12 on the GPU.
        let frame_opt = session.frame_pool.TryGetNextFrame().ok();
        if frame_opt.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(8));
            attempts += 1;
            continue;
        }
        let frame = frame_opt.unwrap();
        let bgra = extract_texture_from_frame(&frame)
            .map_err(|e| format!("extract f{frame_count}: {e}"))?;
        let nv12 = vp
            .convert(&bgra)
            .map_err(|e| format!("vproc f{frame_count}: {e}"))?;

        // Wait for NeedInput (draining HaveOutput along the way), then submit.
        loop {
            let event = unsafe { event_gen.GetEvent(windows::Win32::Media::MediaFoundation::MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) }
                .map_err(|e| format!("GetEvent: {e}"))?;
            let etype = unsafe { event.GetType() }
                .map_err(|e| format!("event type: {e}"))?;

            if etype == METransformHaveOutput.0 as u32 {
                if let Some((data, _, _)) = process_output_async(&encoder)
                    .map_err(|e| format!("process_output f{frame_count}: {e}"))?
                {
                    all_h264.extend_from_slice(&data);
                }
                continue;
            }
            if etype == METransformNeedInput.0 as u32 {
                let pts = frame_count as i64 * frame_duration_pts;
                submit_nv12_texture(&encoder, nv12, pts, frame_duration_pts)
                    .map_err(|e| format!("submit f{frame_count}: {e}"))?;
                break;
            }
            // Ignore other event types.
        }

        frame_count += 1;
        attempts += 1;
    }

    // Drain: tell encoder to flush, then keep pulling HaveOutput until empty.
    use windows::Win32::Media::MediaFoundation::{
        MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    };
    unsafe {
        let _ = encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        let _ = encoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
    }

    // After DRAIN, encoder fires HaveOutput events for remaining frames, then
    // METransformDrainComplete. Cap iterations to avoid infinite waits.
    use windows::Win32::Media::MediaFoundation::METransformDrainComplete;
    for _ in 0..(TARGET_FRAMES + 32) {
        let event = match unsafe { event_gen.GetEvent(windows::Win32::Media::MediaFoundation::MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) } {
            Ok(e) => e,
            Err(_) => break,
        };
        let etype = unsafe { event.GetType() }.unwrap_or(0);
        if etype == METransformHaveOutput.0 as u32 {
            if let Some((data, _, _)) = process_output_async(&encoder)
                .map_err(|e| format!("final drain: {e}"))?
            {
                all_h264.extend_from_slice(&data);
            }
        } else if etype == METransformDrainComplete.0 as u32 {
            break;
        }
    }

    session.session.Close().ok();
    mf_shutdown();

    if all_h264.is_empty() {
        return Err(format!("no encoded output after {frame_count} frames"));
    }

    Ok((all_h264, frame_count, enc_width, enc_height, encoder_name))
}

#[cfg(all(debug_assertions, not(windows)))]
fn poc_gpu_full() -> Result<(Vec<u8>, usize, u32, u32, String), String> {
    Err("Windows only".into())
}
