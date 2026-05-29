//! Multi-window replay coordinator.
//!
//! Owns the focus monitor and a worker per HWND that has ever been focused
//! since `start()`. On every foreground change the previous worker is paused
//! (keeps its packet buffer) and the new window's worker is started or
//! resumed. Save snapshots the worker for the currently-focused HWND.

mod monitor;
mod per_window;

use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

use super::buffer::VideoPacket;
use super::focus::{FocusEvent, FocusMonitor};
use super::games::GameAllowlist;
use super::worker::{AudioTrackSnapshot, WorkerPerf};
use super::{ReplaySettings, ReplayStatus};
use tauri::Manager;

use self::monitor::run_monitor;
use self::per_window::run_per_window;

/// Read the verbose-diag toggle. Coordinator + worker call this before
/// embedding a non-game window title in a diag entry so a copy-pasted
/// support log never carries sensitive browser tabs / document names.
pub(super) fn diag_verbose(app: &tauri::AppHandle) -> bool {
    app.try_state::<crate::DiagVerbose>()
        .map(|s| s.enabled())
        .unwrap_or(false)
}

/// Render a focus-event window title for the diag log. Game titles are
/// always shown (the user explicitly added the exe to the allowlist).
/// Non-game titles are redacted unless verbose-diag mode is on.
pub(super) fn diag_focus_title(app: &tauri::AppHandle, is_game: bool, title: &str) -> String {
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
    /// Friendly name of the encoder MFT the worker is using (e.g.
    /// "NVIDIA H.264 Encoder MFT", "AMDh264Encoder"). Consumed by the save
    /// pipeline to decide whether the AMD-specific SPS-rewrite pre-pass is
    /// needed. Empty when MF didn't expose a friendly name — the save then
    /// runs the pre-pass defensively.
    pub encoder_name: String,
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

pub(super) enum CoordEvent {
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
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
