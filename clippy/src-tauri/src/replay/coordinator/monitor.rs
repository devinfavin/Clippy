use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::replay::capture;
use crate::replay::worker::{CaptureTarget, WorkerHandle};
use crate::replay::{ReplaySettings, ReplayStatus};

use super::{CoordEvent, SaveSnapshot, WorkerPerfRow};

/// Full-screen mode: one worker on the given HMONITOR, no focus tracking,
/// no allowlist. Captures continuously until Stop.
pub(super) fn run_monitor(
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
            use tauri::Emitter;
            let _ = app.emit(
                "replay://spawn-failed",
                crate::replay::SpawnFailedPayload {
                    id: crate::replay::unix_nanos(),
                    window_title: String::new(),
                    kind: "encoder_init".into(),
                    msg: e.clone(),
                },
            );
            if let Ok(mut s) = status.lock() {
                *s = ReplayStatus::Idle;
            }
            return;
        }
    };

    // Friendly label for the snapshot result. Looks up the monitor in the
    // current display list; falls back to a generic label if not found.
    let label = capture::list_monitors()
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
                    encoder_name: worker.encoder_name.clone(),
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
