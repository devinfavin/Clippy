//! Multi-window replay coordinator.
//!
//! Owns the focus monitor and a worker per HWND that has ever been focused
//! since `start()`. On every foreground change the previous worker is paused
//! (keeps its packet buffer) and the new window's worker is started or
//! resumed. Save snapshots the worker for the currently-focused HWND.

use std::collections::HashMap;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

use super::buffer::VideoPacket;
use super::focus::{FocusEvent, FocusMonitor};
use super::games::{self, GameAllowlist};
use super::worker::{AudioTrackSnapshot, CaptureTarget, WorkerHandle, WorkerPerf};
use super::{ReplaySettings, ReplayStatus};
use tauri::Manager;

/// Read the verbose-diag toggle. Coordinator + worker call this before
/// embedding a non-game window title in a diag entry so a copy-pasted
/// support log never carries sensitive browser tabs / document names.
fn diag_verbose(app: &tauri::AppHandle) -> bool {
    app.try_state::<crate::DiagVerbose>()
        .map(|s| s.enabled())
        .unwrap_or(false)
}

/// Render a focus-event window title for the diag log. Game titles are
/// always shown (the user explicitly added the exe to the allowlist).
/// Non-game titles are redacted unless verbose-diag mode is on.
fn diag_focus_title(app: &tauri::AppHandle, is_game: bool, title: &str) -> String {
    if is_game || diag_verbose(app) {
        format!("\"{title}\"")
    } else {
        "<non-game window>".to_string()
    }
}

/// Top-level capture strategy. Per-window uses the focus monitor + allowlist;
/// monitor mode bypasses both and captures one display continuously.
#[derive(Clone, Copy)]
pub enum CaptureMode {
    PerWindow,
    Monitor(isize), // HMONITOR
}

pub struct SaveSnapshot {
    pub packets: Vec<VideoPacket>,
    pub audio_tracks: Vec<AudioTrackSnapshot>,
    pub fps: u32,
    pub window_title: String,
}

/// Per-worker perf snapshot exported via `Coordinator::perf_snapshot`.
/// Captured for `get_diagnostics`'s Performance section.
#[derive(Debug, Clone)]
pub struct WorkerPerfRow {
    pub label: String,
    pub encoder_name: String,
    pub enc_width: u32,
    pub enc_height: u32,
    pub fps: u32,
    pub perf: WorkerPerf,
}

enum CoordEvent {
    Focus(FocusEvent),
    Snapshot(SyncSender<Result<SaveSnapshot, String>>),
    PerfSnapshot(SyncSender<Vec<WorkerPerfRow>>),
    Stop,
}

pub struct Coordinator {
    cmd_tx: SyncSender<CoordEvent>,
    join_handle: Option<thread::JoinHandle<()>>,
    status: Arc<Mutex<ReplayStatus>>,
}

impl Coordinator {
    pub fn start(
        settings: ReplaySettings,
        mode: CaptureMode,
        allowlist: Arc<Mutex<GameAllowlist>>,
        app: tauri::AppHandle,
    ) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<CoordEvent>(64);
        let status = Arc::new(Mutex::new(ReplayStatus::Idle));
        let status_thread = Arc::clone(&status);

        // Extra clones for the panic guards so a coordinator-thread panic
        // still produces a diag entry and resets exported status.
        let status_panic = Arc::clone(&status);
        let app_panic = app.clone();

        let join_handle = match mode {
            CaptureMode::PerWindow => {
                // Focus monitor + per-game workers + allowlist filtering.
                let (focus_monitor, focus_rx) = FocusMonitor::start()?;
                {
                    let cmd_tx_focus = cmd_tx.clone();
                    thread::Builder::new()
                        .name("clippy-focus-relay".into())
                        .spawn(move || {
                            while let Ok(ev) = focus_rx.recv() {
                                if cmd_tx_focus.send(CoordEvent::Focus(ev)).is_err() {
                                    break;
                                }
                            }
                        })
                        .map_err(|e| format!("spawn focus relay: {e}"))?;
                }
                let app_thread = app.clone();
                thread::Builder::new()
                    .name("clippy-coordinator-perwindow".into())
                    .spawn(move || {
                        let result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                run_per_window(
                                    settings,
                                    cmd_rx,
                                    status_thread,
                                    allowlist,
                                    focus_monitor,
                                    app_thread,
                                );
                            }));
                        if let Err(payload) = result {
                            let msg = panic_payload_to_string(&payload);
                            crate::diag(
                                &app_panic,
                                format!("[replay] coordinator (per-window) PANIC: {msg}"),
                            );
                            if let Ok(mut s) = status_panic.lock() {
                                *s = ReplayStatus::Idle;
                            }
                        }
                    })
                    .map_err(|e| format!("spawn coordinator: {e}"))?
            }
            CaptureMode::Monitor(hmon) => {
                // Single worker on the chosen monitor. No focus monitor,
                // no allowlist — deliberate "record everything on this
                // display" choice.
                let app_thread = app.clone();
                thread::Builder::new()
                    .name("clippy-coordinator-monitor".into())
                    .spawn(move || {
                        let result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                run_monitor(settings, hmon, cmd_rx, status_thread, app_thread);
                            }));
                        if let Err(payload) = result {
                            let msg = panic_payload_to_string(&payload);
                            crate::diag(
                                &app_panic,
                                format!("[replay] coordinator (monitor) PANIC: {msg}"),
                            );
                            if let Ok(mut s) = status_panic.lock() {
                                *s = ReplayStatus::Idle;
                            }
                        }
                    })
                    .map_err(|e| format!("spawn coordinator: {e}"))?
            }
        };

        Ok(Coordinator {
            cmd_tx,
            join_handle: Some(join_handle),
            status,
        })
    }

    pub fn snapshot(&self) -> Result<SaveSnapshot, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.cmd_tx
            .send(CoordEvent::Snapshot(tx))
            .map_err(|_| "coordinator stopped".to_string())?;
        rx.recv().map_err(|e| format!("snapshot reply: {e}"))?
    }

    /// Synchronously collect perf rows for every running worker. Empty Vec
    /// when nothing is being captured. Times out after a brief wait so a
    /// stalled coordinator doesn't hang `get_diagnostics`.
    pub fn perf_snapshot(&self) -> Vec<WorkerPerfRow> {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.cmd_tx.send(CoordEvent::PerfSnapshot(tx)).is_err() {
            return Vec::new();
        }
        rx.recv_timeout(std::time::Duration::from_millis(250))
            .unwrap_or_default()
    }

    pub fn status(&self) -> ReplayStatus {
        match self.status.lock() {
            Ok(g) => g.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// True while the coordinator thread is still running. Goes false on
    /// clean stop or panic. `replay_start` checks this before refusing with
    /// "already running" so a dead-but-not-cleared slot can recover.
    pub fn is_alive(&self) -> bool {
        self.join_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    pub fn stop(mut self) -> Result<(), String> {
        let _ = self.cmd_tx.send(CoordEvent::Stop);
        if let Some(h) = self.join_handle.take() {
            h.join().map_err(|_| "coordinator panicked".to_string())?;
        }
        Ok(())
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(CoordEvent::Stop);
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }
}

fn run_per_window(
    settings: ReplaySettings,
    cmd_rx: mpsc::Receiver<CoordEvent>,
    status: Arc<Mutex<ReplayStatus>>,
    allowlist: Arc<Mutex<GameAllowlist>>,
    focus_monitor: FocusMonitor,
    app: tauri::AppHandle,
) {
    crate::diag(
        &app,
        format!(
            "[replay] coordinator (per-window) started · duration={}s bitrate={}kbps audio_devices={} use_process_loopback={}",
            settings.duration_secs,
            settings.video_bitrate_kbps,
            settings.audio_device_ids.len(),
            settings.use_process_loopback
        ),
    );
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    let mut workers: HashMap<isize, WorkerEntry> = HashMap::new();
    // LRU queue of HWNDs — front is most-recently-focused, back is the
    // eviction candidate when we hit `max_concurrent_workers`. Mirrors the
    // `workers` map's keyset.
    let mut lru: Vec<isize> = Vec::new();
    let cap = settings.max_concurrent_workers.max(1) as usize;
    // The HWND of the currently-or-most-recently captured game. Save targets
    // this. Stays set when focus drifts to a non-game (Discord, browser, etc.)
    // so the save hotkey still fires against the last game played.
    let mut last_game_hwnd: Option<isize> = None;

    loop {
        let event_result = cmd_rx.recv_timeout(Duration::from_millis(250));

        match event_result {
            Ok(CoordEvent::Stop) => break,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                // Periodic tick — fall through to status refresh below.
            }

            Ok(CoordEvent::Focus(FocusEvent { hwnd, title })) => {
                // Determine whether the focused window is an allowlisted game.
                let is_game = match games::resolve_window_exe(hwnd) {
                    Some(exe) => match allowlist.lock() {
                        Ok(g) => g.contains(&exe),
                        Err(_) => false,
                    },
                    None => false,
                };

                // Continuous-capture model: never pause existing workers on
                // focus change. They keep recording in the background so the
                // saved buffer reflects real-time game state (including AFK
                // periods while the user was tabbed away).
                if !is_game {
                    crate::diag(
                        &app,
                        format!(
                            "[replay] focus → non-game {} (hwnd {hwnd:#x}) — buffer continues for previous game",
                            diag_focus_title(&app, false, &title)
                        ),
                    );
                    continue;
                }

                // First time seeing this game window — spawn a worker for it.
                // Existing workers for other games keep running.
                if !workers.contains_key(&hwnd) {
                    // LRU eviction: if at the user's cap, retire the least
                    // recently focused worker before spawning the new one.
                    // This bounds VRAM use and (more importantly) keeps us
                    // under the consumer NVENC ~3-session limit.
                    while workers.len() >= cap {
                        let Some(victim) = lru.pop() else { break };
                        if let Some(entry) = workers.remove(&victim) {
                            crate::diag(
                                &app,
                                format!(
                                    "[replay] LRU cap reached ({cap}) — evicting \"{}\" (hwnd {victim:#x}) for new game \"{title}\"",
                                    entry.title
                                ),
                            );
                            let evicted_title = entry.title.clone();
                            let _ = entry.handle.stop();
                            // Clear save target if it pointed at the evicted worker.
                            if last_game_hwnd == Some(victim) {
                                last_game_hwnd = None;
                            }
                            // Best-effort UI notification — surface the eviction
                            // so the user understands why an older game stopped
                            // capturing.
                            use tauri::Emitter;
                            let _ = app.emit(
                                "replay://worker-evicted",
                                serde_json::json!({
                                    "hwnd": format!("{victim:#x}"),
                                    "title": evicted_title,
                                    "cap": cap,
                                }),
                            );
                        }
                    }

                    crate::diag(
                        &app,
                        format!("[replay] focus → game \"{title}\" (hwnd {hwnd:#x}) — spawning worker"),
                    );
                    match WorkerHandle::start(
                        CaptureTarget::Window(hwnd),
                        settings.clone(),
                        app.clone(),
                    ) {
                        Ok(handle) => {
                            workers.insert(
                                hwnd,
                                WorkerEntry {
                                    handle,
                                    title: title.clone(),
                                },
                            );
                            lru.insert(0, hwnd);
                        }
                        Err(e) => {
                            let hint = encoder_failure_hint(&e, workers.len());
                            crate::diag(
                                &app,
                                format!("[replay] worker spawn FAILED for \"{title}\": {e}{hint}"),
                            );
                            continue;
                        }
                    }
                } else {
                    crate::diag(
                        &app,
                        format!("[replay] focus → game \"{title}\" (hwnd {hwnd:#x}) — already capturing"),
                    );
                    // Re-focusing an existing game pushes it to the front of
                    // the LRU so older games stay eviction candidates.
                    lru.retain(|h| *h != hwnd);
                    lru.insert(0, hwnd);
                }

                last_game_hwnd = Some(hwnd);
            }

            Ok(CoordEvent::Snapshot(reply)) => {
                let result = match last_game_hwnd.and_then(|h| workers.get(&h)) {
                    Some(entry) => entry.handle.snapshot().map(|s| SaveSnapshot {
                        packets: s.video,
                        audio_tracks: s.audio_tracks,
                        fps: entry.handle.fps,
                        window_title: entry.title.clone(),
                    }),
                    None => Err("no game has been captured this session — focus a game in the allowlist first".into()),
                };
                let _ = reply.send(result);
            }

            Ok(CoordEvent::PerfSnapshot(reply)) => {
                let rows: Vec<WorkerPerfRow> = workers
                    .iter()
                    .map(|(_, e)| WorkerPerfRow {
                        label: e.title.clone(),
                        encoder_name: e.handle.encoder_name.clone(),
                        enc_width: e.handle.enc_width,
                        enc_height: e.handle.enc_height,
                        fps: e.handle.fps,
                        perf: e.handle.perf(),
                    })
                    .collect();
                let _ = reply.send(rows);
            }
        }

        // Sweep dead workers (window closed, encoder thread panicked, etc.).
        // Without this they pile up in the map and the save target can be a
        // zombie entry with no thread behind it.
        let dead: Vec<isize> = workers
            .iter()
            .filter(|(_, e)| !e.handle.is_alive())
            .map(|(h, _)| *h)
            .collect();
        for h in dead {
            if let Some(entry) = workers.remove(&h) {
                crate::diag(
                    &app,
                    format!(
                        "[replay] worker for \"{}\" (hwnd {h:#x}) is dead — evicting",
                        entry.title
                    ),
                );
                lru.retain(|x| *x != h);
                if last_game_hwnd == Some(h) {
                    last_game_hwnd = None;
                }
            }
        }

        // Refresh exported status from whichever worker is currently the save
        // target. Worker reports its own live buffered_secs; we just forward.
        let live = last_game_hwnd
            .and_then(|h| workers.get(&h))
            .map(|e| e.handle.status())
            .unwrap_or(ReplayStatus::Idle);
        if let Ok(mut s) = status.lock() {
            *s = live;
        }
    }

    for (_, entry) in workers.drain() {
        let _ = entry.handle.stop();
    }
    drop(focus_monitor);

    if let Ok(mut s) = status.lock() {
        *s = ReplayStatus::Idle;
    }
    crate::diag(&app, "[replay] coordinator stopped");
}

struct WorkerEntry {
    handle: WorkerHandle,
    title: String,
}

/// Generate an actionable hint string when worker init fails at the encoder
/// stage. NVENC consumer cards cap concurrent encode sessions (3 on most
/// pre-2023 GeForce drivers); when that limit is hit the MFT activation
/// fails generically and the user has no idea why their Nth game refused
/// to capture. We surface the hint when:
///   - the failure mentions the encoder stage AND
///   - we already have at least 2 workers running (so the cap is plausibly
///     in play; for 0/1 prior workers a different cause is more likely).
fn encoder_failure_hint(err: &str, prior_worker_count: usize) -> String {
    let lower = err.to_lowercase();
    let encoder_related = lower.contains("hw encoder")
        || lower.contains("nvenc")
        || lower.contains("mft")
        || lower.contains("0x80004003")
        || lower.contains("0x80070008");
    if encoder_related && prior_worker_count >= 2 {
        format!(
            " (hint: {prior_worker_count} game(s) already capturing — NVENC consumer cards cap at ~3 concurrent encoder sessions; close another captured game or restart the buffer)"
        )
    } else {
        String::new()
    }
}

/// Best-effort string extraction from a panic payload.
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Full-screen mode: one worker on the given HMONITOR, no focus tracking,
/// no allowlist. Captures continuously until Stop.
fn run_monitor(
    settings: ReplaySettings,
    hmon: isize,
    cmd_rx: mpsc::Receiver<CoordEvent>,
    status: Arc<Mutex<ReplayStatus>>,
    app: tauri::AppHandle,
) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    crate::diag(
        &app,
        format!(
            "[replay] coordinator (monitor) started · hmon={hmon:#x} duration={}s bitrate={}kbps",
            settings.duration_secs, settings.video_bitrate_kbps
        ),
    );

    let worker = match WorkerHandle::start(
        CaptureTarget::Monitor(hmon),
        settings,
        app.clone(),
    ) {
        Ok(w) => w,
        Err(e) => {
            crate::diag(&app, format!("[replay] monitor worker spawn FAILED: {e}"));
            if let Ok(mut s) = status.lock() {
                *s = ReplayStatus::Idle;
            }
            return;
        }
    };

    // Friendly label for the snapshot result. Looks up the monitor in the
    // current display list; falls back to a generic label if not found.
    let label = super::capture::list_monitors()
        .into_iter()
        .find(|m| m.hmonitor == format!("{}", hmon))
        .map(|m| m.label)
        .unwrap_or_else(|| "Display".to_string());

    loop {
        let event = cmd_rx.recv_timeout(Duration::from_millis(250));
        match event {
            Ok(CoordEvent::Stop) => break,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
            Ok(CoordEvent::Focus(_)) => {} // ignored in monitor mode
            Ok(CoordEvent::Snapshot(reply)) => {
                let result = worker.snapshot().map(|s| SaveSnapshot {
                    packets: s.video,
                    audio_tracks: s.audio_tracks,
                    fps: worker.fps,
                    window_title: label.clone(),
                });
                let _ = reply.send(result);
            }
            Ok(CoordEvent::PerfSnapshot(reply)) => {
                let row = WorkerPerfRow {
                    label: label.clone(),
                    encoder_name: worker.encoder_name.clone(),
                    enc_width: worker.enc_width,
                    enc_height: worker.enc_height,
                    fps: worker.fps,
                    perf: worker.perf(),
                };
                let _ = reply.send(vec![row]);
            }
        }

        // If the lone monitor worker died (capture item closed, encoder
        // panic), exit the coordinator. `replay_start` will then detect the
        // dead slot via `is_alive()` and let the user start fresh.
        if !worker.is_alive() {
            crate::diag(&app, "[replay] monitor worker died — coordinator exiting");
            break;
        }

        if let Ok(mut s) = status.lock() {
            *s = worker.status();
        }
    }

    let _ = worker.stop();
    if let Ok(mut s) = status.lock() {
        *s = ReplayStatus::Idle;
    }
}
