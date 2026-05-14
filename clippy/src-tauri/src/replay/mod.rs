pub mod audio;
pub mod buffer;
#[cfg(windows)]
pub mod process_loopback;
pub mod capture;
pub mod commands;
#[cfg(feature = "poc")]
pub mod convert;
pub mod coordinator;
pub mod encoder;
pub mod focus;
pub mod games;
#[cfg(feature = "poc")]
pub mod poc;
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
pub(super) fn prune_dead_coord(state: &ReplayState) {
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

// ---------- allowlist persistence ----------

/// Path where manual game-allowlist additions are persisted.
pub(super) fn allowlist_file(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
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

pub(super) fn load_recent_adds(app: &tauri::AppHandle) -> Vec<String> {
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
pub(super) fn track_recent_add(app: &tauri::AppHandle, path: &str) {
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
pub(super) fn untrack_recent_add(app: &tauri::AppHandle, path: &str) {
    let mut entries = load_recent_adds(app);
    let lower = path.to_lowercase();
    let before = entries.len();
    entries.retain(|e| e.to_lowercase() != lower);
    if entries.len() != before {
        save_recent_adds(app, &entries);
    }
}

/// Persist only manual entries (not launcher-scanned ones — those re-derive
/// from disk every session so they stay accurate as games install/uninstall).
pub(super) fn persist_manual(
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

// ---------- save pipeline ----------

/// Result of a successful save. The path is the only thing the frontend
/// needs to load the clip; `window_title` is purely informational (used in
/// diagnostics + toast labels).
#[derive(Debug, Clone, Serialize)]
pub struct ReplaySaveResult {
    pub path: String,
    pub window_title: String,
}

// ---------- save-progress event payloads ----------
//
// Drives the in-game save overlay. Every save flow emits four kinds of
// events keyed by a per-save `id` (Unix nanos at hotkey press): a `started`
// event fired the moment the save begins, zero or more `progress` events as
// the pipeline transitions stages, then exactly one terminal event
// (`saved` on success, `save-error` on failure). The overlay window keys
// off `id` so concurrent saves don't blend together in the UI.

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveStartedPayload {
    pub id: u64,
    /// Game window name from the snapshot, or empty if the snapshot couldn't
    /// be taken (e.g. buffer not running). The overlay shows "Saving…" while
    /// this is empty.
    pub window_title: String,
    /// Approximate captured buffer length, computed from packet count and
    /// effective fps. Used for the overlay's "Saving 60s of MyGame…" line.
    pub duration_secs: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProgressPayload {
    pub id: u64,
    /// One of: "writing" | "bsf" | "muxing" | "finalizing". The overlay maps
    /// each to a human-readable line; unknown stages render verbatim.
    pub stage: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedPayload {
    pub id: u64,
    pub path: String,
    pub size_mb: f64,
    pub window_title: String,
    /// Number of audio tracks that captured non-empty packets (made it into
    /// the saved MP4). When less than `tracks_total`, the overlay surfaces
    /// "Saved with N of M audio tracks" so the user notices missing tracks
    /// instead of finding them gone hours later.
    pub tracks_saved: u32,
    pub tracks_total: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveErrorPayload {
    pub id: u64,
    /// Coarse machine-readable bucket so the frontend can map to friendly
    /// text. One of: "buffer_empty" | "not_running" | "io" | "ffmpeg" | "generic".
    pub kind: String,
    pub msg: String,
}

/// Emitted when a per-window worker fails to start (encoder init error, MFT
/// activation failure, NVENC concurrent-session cap). Surfaced via the
/// in-game overlay's spawn-error phase so the user sees in-game feedback
/// when a buffer silently fails to start for a focused game — otherwise the
/// hotkey just does nothing later and the user has no idea why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnFailedPayload {
    pub id: u64,
    pub window_title: String,
    /// One of: "nvenc_ceiling" | "encoder_init" | "generic".
    pub kind: String,
    pub msg: String,
}

fn classify_save_error(msg: &str) -> &'static str {
    let l = msg.to_ascii_lowercase();
    if l.contains("buffer is empty") { return "buffer_empty"; }
    if l.contains("not running") { return "not_running"; }
    if l.contains("ffmpeg") { return "ffmpeg"; }
    if l.contains("write h264")
        || l.contains("create save")
        || l.contains("permission denied")
        || l.contains("no space")
        || l.contains("disk full")
        || l.contains("audio track")
    {
        return "io";
    }
    "generic"
}

pub(crate) fn unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Same flow as `replay_save`, but callable from non-command contexts (e.g.
/// the global hotkey handler in `lib.rs`). Emits `replay://save-started`
/// immediately, `replay://save-progress` at each pipeline stage, and exactly
/// one of `replay://saved` / `replay://save-error` at completion so the
/// in-game overlay window can surface the save state without the renderer
/// being foreground.
pub async fn save_active(app: &tauri::AppHandle) -> Result<ReplaySaveResult, String> {
    use tauri::{Emitter, Manager};
    let id = unix_nanos();
    // BEGIN marker so any single save attempt is grep-able in a busy log
    // (`[save id=…] BEGIN` … `[save id=…] END`). Pairs with the END marker
    // at every exit point in `finish_save`.
    crate::diag(app, format!("[save id={id}] BEGIN"));
    let state = app.state::<ReplayState>();

    // Snapshot first so save-started can include window title + duration.
    // Snapshot is fast (clones Arc-shared packet buffers — ~tens of ms),
    // well below human perception of "instant feedback on hotkey".
    let snap = match take_snapshot(&state) {
        Ok(s) => s,
        Err(e) => {
            let _ = app.emit("replay://save-started", &SaveStartedPayload {
                id,
                window_title: String::new(),
                duration_secs: 0,
            });
            let _ = app.emit("replay://save-error", &SaveErrorPayload {
                id,
                kind: classify_save_error(&e).into(),
                msg: e.clone(),
            });
            crate::diag(app, format!("[save id={id}] END · ERROR (snapshot) · {e}"));
            return Err(e);
        }
    };

    // Estimate display duration from packet count + fps. PTS-span would be
    // more accurate but write_and_mux does that math too; for an overlay
    // header line "Saving Ns of MyGame…", packets/fps is fine.
    let approx_duration_secs =
        (snap.packets.len() as f64 / (snap.fps.max(1) as f64)).round() as u32;
    let _ = app.emit("replay://save-started", &SaveStartedPayload {
        id,
        window_title: snap.window_title.clone(),
        duration_secs: approx_duration_secs,
    });

    finish_save(app, snap, id).await
}

pub(super) fn take_snapshot(state: &ReplayState) -> Result<coordinator::SaveSnapshot, String> {
    prune_dead_coord(state);
    let guard = state.coord.lock().map_err(|e| e.to_string())?;
    let c = guard.as_ref().ok_or("replay buffer is not running")?;
    let snap = c.snapshot()?;
    if snap.packets.is_empty() {
        return Err("active window's buffer is empty — give it a few seconds of capture first".into());
    }
    Ok(snap)
}

pub(super) async fn finish_save(
    app: &tauri::AppHandle,
    mut snap: coordinator::SaveSnapshot,
    save_id: u64,
) -> Result<ReplaySaveResult, String> {
    use tauri::{Emitter, Manager};

    // Helper: emit a `replay://save-error` event + bail. Closes over `app` and
    // `save_id` so call sites read as a single `?`-style early return.
    // Also writes the matching END marker so every BEGIN has a partner.
    let emit_error = |msg: &str| {
        let _ = app.emit("replay://save-error", &SaveErrorPayload {
            id: save_id,
            kind: classify_save_error(msg).into(),
            msg: msg.to_string(),
        });
        crate::diag(app, format!("[save id={save_id}] END · ERROR · {msg}"));
    };

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
        Some(s) => match s.0.lock().map_err(|e| e.to_string()) {
            Ok(g) => g.clone(),
            Err(e) => { emit_error(&e); return Err(e); }
        },
        None => default_save_dir(app),
    };
    // Best-effort: create the directory if it disappeared since startup
    // (user deleted it, network drive offline, etc.).
    if let Err(e) = std::fs::create_dir_all(&dir) {
        let msg = format!("couldn't create replay save folder {}: {e}", dir.display());
        emit_error(&msg);
        return Err(msg);
    }

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
            let safe = save::truncate_for_metadata(&t.name, 64);
            if safe.is_empty() {
                format!("track {i}")
            } else {
                format!("\"{safe}\" (track {i})")
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
            let safe = save::truncate_for_metadata(&t.name, 64);
            let label = if safe.is_empty() {
                format!("a{i}")
            } else {
                format!("a{i}=\"{safe}\"")
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

    // Emit the first stage event. By the time we reach write_and_mux, the
    // snapshot + path resolution + audio summary diag have already run; from
    // the user's perspective the save is now "writing to disk".
    let _ = app.emit("replay://save-progress", &SaveProgressPayload {
        id: save_id,
        stage: "writing".into(),
    });

    let timings = match save::write_and_mux(
        &snap.packets,
        &snap.audio_tracks,
        snap.fps,
        &snap.encoder_name,
        &out,
        Some((app, save_id)),
    )
    .await {
        Ok(t) => t,
        Err(e) => { emit_error(&e); return Err(e); }
    };

    // Save timing breakdown: lets us see *where* a slow save is slow
    // (snapshot copy / disk write / ffmpeg mux / post-probe) without guessing.
    // `eff_fps` is the framerate the mux ran at — equals the configured fps
    // when the encoder kept up, lower when it back-pressured. Surfacing it
    // here makes encoder shortfall visible at a glance: a long capture
    // session showing eff_fps consistently below target (e.g. 56.5 vs 60)
    // means the encoder is the bottleneck, not WGC or pacing.
    crate::diag(
        app,
        format!(
            "[replay] save · h264 {}ms ({:.1}MB) · pcm {}ms ({:.1}MB) · bsf {}ms · ffmpeg {}ms · probe {}ms · total {}ms · eff_fps={:.2}/cfg={}",
            timings.h264_write_ms,
            timings.h264_bytes as f64 / (1024.0 * 1024.0),
            timings.pcm_write_ms,
            timings.pcm_bytes as f64 / (1024.0 * 1024.0),
            timings.bsf_pass_ms,
            timings.ffmpeg_mux_ms,
            timings.probe_ms,
            timings.total_ms,
            timings.effective_fps,
            snap.fps,
        ),
    );

    // Post-save sanity check: probe the muxed output and compare against
    // `effective_fps` (the rate we asked ffmpeg to mux at), NOT the
    // configured `snap.fps`. With the framerate-stretch fix, video is muxed
    // at the actual encoder rate so audio/video align — the configured fps
    // is the worker's target, which the encoder may not have hit. A real
    // mismatch (bsf gate misfire on a new AMD-like vendor, or ffmpeg
    // ignoring our -framerate for some reason) still surfaces because the
    // probe sees a rate fundamentally different from what we requested.
    if let Some((probed_fps, probed_dur)) = timings.probed {
        let fps_delta = (probed_fps - timings.effective_fps).abs();
        if fps_delta > 0.5 {
            crate::diag(
                app,
                format!(
                    "[replay] save · ⚠ fps mismatch — muxed at {:.2}fps, probed {:.2}fps (Δ={:.2}) · dur={:.2}s · cfg={}fps · encoder=\"{}\" · please report",
                    timings.effective_fps, probed_fps, fps_delta, probed_dur,
                    snap.fps, snap.encoder_name,
                ),
            );
        } else {
            crate::diag(
                app,
                format!(
                    "[replay] save · verified {:.2}fps · dur={:.2}s",
                    probed_fps, probed_dur,
                ),
            );
        }
    } else {
        crate::diag(app, "[replay] save · probe skipped or failed (clip should still be playable)");
    }

    let out_path = out.to_string_lossy().into_owned();
    let size_mb = std::fs::metadata(&out)
        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0);
    // `total` + `dropped` were computed before the mux above. tracks_saved is
    // tracks with non-empty packet sets that actually landed in the MP4.
    let tracks_saved = (total - dropped.len()) as u32;
    let _ = app.emit("replay://saved", &SavedPayload {
        id: save_id,
        path: out_path.clone(),
        size_mb,
        window_title: snap.window_title.clone(),
        tracks_saved,
        tracks_total: total as u32,
    });
    crate::diag(
        app,
        format!(
            "[save id={save_id}] END · OK · {:.1}MB · {tracks_saved}/{total} tracks · {}",
            size_mb,
            out_path
        ),
    );
    Ok(ReplaySaveResult {
        path: out_path,
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
pub(super) fn save_dir_pref_file(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
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
