use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::replay::focus::{FocusEvent, FocusMonitor};
use crate::replay::games::{self, GameAllowlist};
use crate::replay::worker::{CaptureTarget, WorkerHandle};
use crate::replay::{ReplaySettings, ReplayStatus};

use super::{diag_focus_title, CoordEvent, SaveSnapshot, WorkerPerfRow};

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

pub(super) fn run_per_window(
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
    // Dedup state for non-game focus diag entries. Without this the log fills
    // with `focus → non-game <hwnd X>` lines as the user alt-tabs through
    // Discord/Steam/browser overlays — observed ~80% of all entries in a
    // typical session were this. We suppress logging when the same hwnd
    // re-fires within a short window.
    let mut last_nongame_logged_hwnd: Option<isize> = None;
    let mut last_nongame_logged_at = std::time::Instant::now();
    const NONGAME_DEDUP_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

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
                    // Dedup: suppress if same hwnd as last logged within the
                    // window. Most rapid alt-tabs flutter through 2-4
                    // overlay windows in <500ms; logging each adds noise
                    // without changing what the user actually did.
                    let now = std::time::Instant::now();
                    let suppress = last_nongame_logged_hwnd == Some(hwnd)
                        && now.duration_since(last_nongame_logged_at) < NONGAME_DEDUP_WINDOW;
                    if !suppress {
                        crate::diag(
                            &app,
                            format!(
                                "[replay] focus → non-game {} (hwnd {hwnd:#x}) — buffer continues for previous game",
                                diag_focus_title(&app, false, &title)
                            ),
                        );
                        last_nongame_logged_hwnd = Some(hwnd);
                        last_nongame_logged_at = now;
                    }
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
                            // Surface to the user via the in-game overlay so
                            // they actually see that the buffer didn't start.
                            // Without this, the user fires the save hotkey
                            // later, nothing happens, no clue why.
                            use tauri::Emitter;
                            let kind = if !hint.is_empty() {
                                "nvenc_ceiling"
                            } else {
                                "encoder_init"
                            };
                            let combined_msg = if hint.is_empty() {
                                e.clone()
                            } else {
                                format!("{e}{hint}")
                            };
                            let _ = app.emit(
                                "replay://spawn-failed",
                                crate::replay::SpawnFailedPayload {
                                    id: crate::replay::unix_nanos(),
                                    window_title: title.clone(),
                                    kind: kind.into(),
                                    msg: combined_msg,
                                },
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
                        encoder_name: entry.handle.encoder_name.clone(),
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
