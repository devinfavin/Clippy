//! Tauri command surface for the replay buffer.
//!
//! Public commands are wired into `tauri::generate_handler![]` in
//! `lib.rs` as `replay::commands::<name>`. Each command is a thin
//! wrapper that translates Tauri-managed state + IPC args into a call
//! against the orchestration helpers in [`super`] (`replay/mod.rs`).
//!
//! See [`super::finish_save`] and [`super::take_snapshot`] for the
//! actual save pipeline.

use std::sync::Arc;

use super::{
    allowlist_file, audio, capture, coordinator, games,
    load_recent_adds, persist_manual, prune_dead_coord, save_dir_pref_file, sysinfo,
    track_recent_add, untrack_recent_add,
};
use super::{
    CaptureModeArg, EncoderPreference, ReplaySaveResult, ReplaySettings, ReplayState,
    ReplayStatus, ResolutionMode,
};

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

/// Snapshot the buffer for whichever window is currently focused, then mux
/// to an MP4 in the temp directory.
#[tauri::command]
pub async fn replay_save(
    app: tauri::AppHandle,
    _state: tauri::State<'_, ReplayState>,
) -> Result<ReplaySaveResult, String> {
    // Funnel through save_active so the same overlay events (save-started,
    // save-progress, saved/save-error) fire for the frontend "Save now"
    // button and the global hotkey alike. The unused state param is kept
    // for API stability; save_active re-fetches it.
    super::save_active(&app).await
}

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
    let def = super::default_save_dir(&app);
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
