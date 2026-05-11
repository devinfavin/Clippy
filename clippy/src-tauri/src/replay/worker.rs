//! Replay buffer worker thread.
//!
//! Owns the D3D11 device, WGC session, video processor, and async H.264
//! encoder for one capture target. Runs on its own OS thread because COM
//! interface pointers (IMFTransform, ID3D11*, etc.) are not Send-safe to move
//! across async runtime threads. Communicates with the rest of the app via
//! a channel; a snapshot of the encoded packet buffer is requested for save.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::audio::{AudioFormat, AudioPacket};
use super::buffer::VideoPacket;
use super::{ReplaySettings, ReplayStatus};

/// What `Snapshot` returns: the video buffer plus zero or more audio tracks.
pub struct WorkerSnapshot {
    pub video: Vec<VideoPacket>,
    /// One entry per audio device captured by this worker. Each carries its
    /// own raw PCM packets and the format needed to interpret the bytes.
    pub audio_tracks: Vec<AudioTrackSnapshot>,
}

pub struct AudioTrackSnapshot {
    pub format: AudioFormat,
    pub packets: Vec<AudioPacket>,
    /// Friendly name for the saved MP4's track title metadata. Empty string
    /// means "no custom name; the muxer can fall back to a default".
    pub name: String,
}

/// What this worker is capturing. Same encode pipeline downstream regardless.
#[derive(Clone, Copy, Debug)]
pub enum CaptureTarget {
    /// Per-window mode — `isize` is the HWND.
    Window(isize),
    /// Full-screen mode — `isize` is the HMONITOR.
    Monitor(isize),
}

pub enum WorkerCmd {
    /// Capture a snapshot of the current packet buffers (video + audio).
    Snapshot(mpsc::SyncSender<Result<WorkerSnapshot, String>>),
    /// Exit the worker loop.
    Stop,
}

/// Snapshot of the most-recent ~30s rollup published by a worker. Used by
/// the "Performance" section of `get_diagnostics` so the user copying the log
/// after a problem sees what the worker was actually doing.
#[derive(Debug, Clone, Default)]
pub struct WorkerPerf {
    /// Wall-clock seconds the most recent window covered (≈ 30s, smaller
    /// for the first window after init).
    pub window_secs: f32,
    /// Frames the WGC frame pool yielded during the window.
    pub captured_frames: u32,
    /// Frames actually submitted to the encoder (≤ captured + held-from-prev
    /// when WGC was idle and we resubmitted the previous NV12).
    pub submitted_frames: u32,
    /// Times we held the previous NV12 (no fresh capture this pacing tick).
    pub duplicated_frames: u32,
    /// Encoder output packets and total bytes.
    pub encoded_packets: u32,
    pub encoded_bytes: u64,
    /// Wall-clock UTC epoch (seconds) when this rollup was published.
    pub published_epoch: u64,
}

pub struct WorkerHandle {
    cmd_tx: mpsc::SyncSender<WorkerCmd>,
    join_handle: Option<thread::JoinHandle<()>>,
    status: Arc<Mutex<ReplayStatus>>,
    /// Latest 30s rollup. Updated by the worker thread; read by the
    /// coordinator (for `get_diagnostics`) and never mutated externally.
    perf: Arc<Mutex<WorkerPerf>>,
    /// Width/height the worker is encoding at (16-aligned). For UI / save mux.
    pub enc_width: u32,
    pub enc_height: u32,
    pub fps: u32,
    /// Friendly name of the activated encoder MFT (e.g. "NVIDIA H.264
    /// Encoder MFT"). Empty when MF didn't expose one.
    pub encoder_name: String,
}

impl WorkerHandle {
    /// Spawn a worker on the given capture target (window or monitor).
    /// Blocks briefly while the worker initializes its capture+encode pipeline;
    /// returns once it's running or has errored out.
    pub fn start(
        target: CaptureTarget,
        settings: ReplaySettings,
        app: tauri::AppHandle,
    ) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<WorkerCmd>(8);
        let (init_tx, init_rx) =
            mpsc::sync_channel::<Result<(u32, u32, u32, String), String>>(1);
        let status = Arc::new(Mutex::new(ReplayStatus::Idle));
        let status_thread = Arc::clone(&status);
        let perf = Arc::new(Mutex::new(WorkerPerf::default()));
        let perf_thread = Arc::clone(&perf);
        // Extra clones held for the panic handler so we can still publish
        // diagnostics + reset status if `run_worker` unwinds.
        let status_panic = Arc::clone(&status);
        let app_panic = app.clone();

        let join_handle = thread::Builder::new()
            .name("clippy-replay-worker".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_worker(target, settings, cmd_rx, status_thread, perf_thread, init_tx, app);
                }));
                if let Err(payload) = result {
                    let msg = panic_payload_to_string(&payload);
                    crate::diag(
                        &app_panic,
                        format!("[replay] worker PANIC for {target:?}: {msg}"),
                    );
                    if let Ok(mut s) = status_panic.lock() {
                        *s = ReplayStatus::Idle;
                    }
                }
            })
            .map_err(|e| format!("spawn worker: {e}"))?;

        // Wait for the worker to report its init outcome.
        let (enc_width, enc_height, fps, encoder_name) = init_rx
            .recv()
            .map_err(|_| "worker died during init".to_string())?
            .map_err(|e| format!("worker init: {e}"))?;

        Ok(WorkerHandle {
            cmd_tx,
            join_handle: Some(join_handle),
            status,
            perf,
            enc_width,
            enc_height,
            fps,
            encoder_name,
        })
    }

    /// Snapshot of the most-recent perf rollup. Cheap clone; safe to call
    /// from any thread.
    pub fn perf(&self) -> WorkerPerf {
        match self.perf.lock() {
            Ok(g) => g.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// Synchronously request a snapshot of the current packet buffers
    /// (video + every audio track this worker is capturing).
    pub fn snapshot(&self) -> Result<WorkerSnapshot, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.cmd_tx
            .send(WorkerCmd::Snapshot(reply_tx))
            .map_err(|e| format!("worker channel closed: {e}"))?;
        reply_rx
            .recv()
            .map_err(|e| format!("snapshot reply: {e}"))?
    }

    pub fn status(&self) -> ReplayStatus {
        match self.status.lock() {
            Ok(g) => g.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// True while the worker thread is still running. Goes false once the
    /// thread exits — clean stop, init failure, or panic. The coordinator
    /// uses this to evict zombie entries from its workers map.
    pub fn is_alive(&self) -> bool {
        self.join_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Consume the handle and stop the worker cleanly.
    pub fn stop(mut self) -> Result<(), String> {
        let _ = self.cmd_tx.send(WorkerCmd::Stop);
        if let Some(h) = self.join_handle.take() {
            h.join().map_err(|_| "worker thread panicked".to_string())?;
        }
        Ok(())
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(WorkerCmd::Stop);
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }
}

/// Best-effort string extraction from a panic payload (works for both
/// `panic!("foo")` and `panic!(some_string)` forms).
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Drop oldest packets to keep the buffer at approximately `duration_pts` of
/// footage, preserving decode validity (the buffer always starts on a keyframe).
fn trim_buffer(buffer: &mut VecDeque<VideoPacket>, duration_pts: i64) {
    if buffer.is_empty() {
        return;
    }
    let latest = match buffer.back() {
        Some(p) => p.pts,
        None => return,
    };
    let cutoff = latest - duration_pts;
    // Find the first keyframe whose pts >= cutoff. Drop everything before it.
    let mut drop_before = 0usize;
    for (i, p) in buffer.iter().enumerate() {
        if p.is_keyframe && p.pts >= cutoff {
            drop_before = i;
            break;
        }
    }
    if drop_before > 0 {
        buffer.drain(0..drop_before);
    }
}

#[cfg(windows)]
fn run_worker(
    target: CaptureTarget,
    settings: ReplaySettings,
    cmd_rx: mpsc::Receiver<WorkerCmd>,
    status: Arc<Mutex<ReplayStatus>>,
    perf: Arc<Mutex<WorkerPerf>>,
    init_tx: mpsc::SyncSender<Result<(u32, u32, u32, String), String>>,
    app: tauri::AppHandle,
) {
    use super::capture::windows_impl::*;
    use super::encoder::windows_impl::*;
    use super::vproc::windows_impl::VideoProcessor;
    use windows::core::Interface;
    use windows::Foundation::TypedEventHandler;
    use windows::Graphics::Capture::GraphicsCaptureItem;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::HMONITOR;
    use windows::Win32::Media::MediaFoundation::{
        IMFMediaEventGenerator, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, METransformHaveOutput,
        METransformNeedInput, MF_EVENT_FLAG_NO_WAIT, MF_E_NO_EVENTS_AVAILABLE,
        MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    };

    let fps: u32 = settings.fps.clamp(15, 240);
    let frame_duration_pts: i64 = 10_000_000 / fps as i64;
    let duration_pts: i64 = settings.duration_secs as i64 * 10_000_000;

    // Initialize the full GPU pipeline. If any step fails, send the error
    // back through init_tx and exit.
    let init = (|| -> Result<_, String> {
        let bundle = create_d3d11_device().map_err(|e| format!("D3D11: {e}"))?;
        let item = match target {
            CaptureTarget::Window(h) => {
                capture_item_for_hwnd(HWND(h as *mut _)).map_err(|e| format!("WGC item (window): {e}"))?
            }
            CaptureTarget::Monitor(h) => capture_item_for_monitor(HMONITOR(h as *mut _))
                .map_err(|e| format!("WGC item (monitor): {e}"))?,
        };
        let size = item.Size().map_err(|e| format!("item size: {e}"))?;
        let (src_w, src_h) = (size.Width as u32, size.Height as u32);

        // Resolve the encoder output dimensions from the user's resolution
        // setting. All paths end up 16-aligned for H.264 macroblock layout.
        let (target_w, target_h) = match settings.resolution_mode {
            super::ResolutionMode::Source => (src_w, src_h),
            super::ResolutionMode::Half => ((src_w / 2).max(16), (src_h / 2).max(16)),
            super::ResolutionMode::Custom { width, height } => {
                (width.max(16), height.max(16))
            }
        };
        let enc_w = (target_w + 15) & !15u32;
        let enc_h = (target_h + 15) & !15u32;

        let session = open_capture_session_for(&item, &bundle.device)
            .map_err(|e| format!("WGC session: {e}"))?;
        let vp = VideoProcessor::new(&bundle.device, &bundle.context, src_w, src_h, enc_w, enc_h, fps)
            .map_err(|e| format!("video processor: {e}"))?;

        mf_startup().map_err(|e| format!("MFStartup: {e}"))?;
        let (encoder, device_manager, encoder_name) = create_h264_encoder_hw_async(
            &bundle.device,
            enc_w,
            enc_h,
            settings.video_bitrate_kbps,
            fps,
            1,
            settings.encoder_preference,
            settings.keyframe_interval_secs,
        )
        .map_err(|e| format!("HW encoder: {e}"))?;
        let event_gen: IMFMediaEventGenerator = encoder
            .cast()
            .map_err(|e| format!("event generator cast: {e}"))?;

        Ok((bundle, session, vp, encoder, device_manager, event_gen, enc_w, enc_h, item, encoder_name))
    })();

    let (bundle, session, vp, encoder, _device_manager, event_gen, enc_w, enc_h, item, encoder_name) =
        match init {
            Ok(t) => t,
            Err(e) => {
                crate::diag(
                    &app,
                    format!("[replay] worker init FAILED for {target:?}: {e}"),
                );
                let _ = init_tx.send(Err(e));
                return;
            }
        };

    let title = item.DisplayName().ok().map(|s| s.to_string()).unwrap_or_default();
    let encoder_label = if encoder_name.is_empty() {
        "<unnamed encoder>".to_string()
    } else {
        encoder_name.clone()
    };
    crate::diag(
        &app,
        format!(
            "[replay] worker started — {target:?} \"{title}\" enc={enc_w}x{enc_h}@{fps} br={}kbps gop={:?}s pref={:?} encoder=\"{encoder_label}\"",
            settings.video_bitrate_kbps,
            settings.keyframe_interval_secs,
            settings.encoder_preference,
        ),
    );
    let _ = init_tx.send(Ok((enc_w, enc_h, fps, encoder_name.clone())));

    // Subscribe to GraphicsCaptureItem.Closed so we exit promptly when the
    // captured window/monitor goes away (game closed, monitor disconnected).
    // Without this the worker would keep ticking against a dead surface
    // forever and the coordinator would never know to evict it.
    let item_closed = Arc::new(AtomicBool::new(false));
    let _closed_token = {
        let flag = Arc::clone(&item_closed);
        let app_for_handler = app.clone();
        let target_for_handler = target;
        let handler = TypedEventHandler::<GraphicsCaptureItem, windows::core::IInspectable>::new(
            move |_sender, _args| {
                flag.store(true, Ordering::SeqCst);
                crate::diag(
                    &app_for_handler,
                    format!("[replay] capture item closed for {target_for_handler:?} — worker exiting"),
                );
                Ok(())
            },
        );
        item.Closed(&handler).ok()
    };

    // Shared epoch for video and audio PTS — both threads timestamp packets
    // relative to this so saved files have aligned A/V tracks.
    let epoch = Instant::now();

    // Build the audio capture set:
    //   1. (Phase 3.3) When in PerWindow mode and process loopback is enabled,
    //      try to capture only the game's process audio. On Win10 / activation
    //      failure this just yields no handle and we fall back to system audio.
    //   2. (Phase 3.2) Add one capture handle per user-selected output device.
    //   3. If we ended up with zero handles, fall back to the default render
    //      endpoint so the save still has audio.
    // (handle, friendly track name) — used for the saved MP4's title metadata.
    let mut audio_handles: Vec<(super::audio::AudioCaptureHandle, String)> = Vec::new();

    if settings.use_process_loopback {
        if let CaptureTarget::Window(hwnd_val) = target {
            let pid = unsafe {
                use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
                let mut p: u32 = 0;
                GetWindowThreadProcessId(HWND(hwnd_val as *mut _), Some(&mut p));
                p
            };
            if pid != 0 {
                if let Ok(h) = super::audio::AudioCaptureHandle::start_for_process(
                    pid,
                    epoch,
                    settings.duration_secs,
                ) {
                    let label = if title.trim().is_empty() {
                        "Game audio".to_string()
                    } else {
                        format!("{title} (game audio)")
                    };
                    audio_handles.push((h, label));
                }
            }
        }
    }

    for (i, device_id) in settings.audio_device_ids.iter().enumerate() {
        if let Ok(h) = super::audio::AudioCaptureHandle::start(
            Some(device_id.clone()),
            epoch,
            settings.duration_secs,
        ) {
            // User-provided friendly name (empty string means "default").
            let name = settings
                .audio_device_names
                .get(i)
                .cloned()
                .unwrap_or_default();
            audio_handles.push((h, name));
        }
    }

    if audio_handles.is_empty() {
        if let Ok(h) =
            super::audio::AudioCaptureHandle::start(None, epoch, settings.duration_secs)
        {
            audio_handles.push((h, "Default output".to_string()));
        }
    }

    crate::diag(
        &app,
        format!(
            "[replay] audio: {} track(s) initialised (process_loopback_requested={})",
            audio_handles.len(),
            settings.use_process_loopback
        ),
    );

    // Wrap in Arc so snapshot threads can read the handles while the worker
    // continues encoding. AudioCaptureHandle::snapshot takes &self so shared
    // access is safe; the audio threads stay alive as long as any clone is.
    let audio_handles = Arc::new(audio_handles);

    // Buffer state.
    let mut buffer: VecDeque<VideoPacket> = VecDeque::with_capacity(4096);
    let mut next_pts: i64 = 0;
    let mut have_pending_input = false;

    // Pacing state. Wall-clock interval between submissions; PTS advances by
    // one fixed-rate step per submission. This decouples saved-video timing
    // from WGC's content-driven capture rate so:
    //  - 30fps games don't play back at 2x speed
    //  - Static periods (loading screens, paused menus) don't compress
    //  - Unlocked-fps games don't oversaturate the encoder
    let frame_interval = Duration::from_micros(1_000_000 / fps as u64);
    let mut last_submit_time = Instant::now();
    let mut nv12_ready = false;

    // Encoder fires NeedInput before any input has been submitted. Pump events
    // once up front so the first real frame can be submitted immediately.
    let nowait = MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(MF_EVENT_FLAG_NO_WAIT.0 as u32);
    let block = MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0);

    // Rolling perf counters. Reset every ~30s; published to `perf` mutex on
    // each rollup so `get_diagnostics` can read the latest snapshot without
    // touching the worker thread.
    const ROLLUP_INTERVAL: Duration = Duration::from_secs(30);
    let mut roll_start = Instant::now();
    let mut c_captured: u32 = 0;
    let mut c_submitted: u32 = 0;
    let mut c_duplicated: u32 = 0;
    let mut c_packets: u32 = 0;
    let mut c_bytes: u64 = 0;

    'main: loop {
        // 0. If the captured item went away (window closed, monitor unplugged),
        // exit the main loop. Coordinator's per-tick liveness sweep will then
        // remove this worker from its workers map.
        if item_closed.load(Ordering::Relaxed) {
            break 'main;
        }

        // Periodic perf rollup. Publishes a snapshot for `get_diagnostics`
        // and emits a one-line diag entry so the user can spot frame drops
        // / encoder starvation in the live log.
        let now_inst = Instant::now();
        if now_inst.duration_since(roll_start) >= ROLLUP_INTERVAL {
            let elapsed = now_inst.duration_since(roll_start).as_secs_f32().max(0.001);
            let bytes_per_sec = (c_bytes as f32 / elapsed) as u64;
            let cap_fps = c_captured as f32 / elapsed;
            let sub_fps = c_submitted as f32 / elapsed;
            let snap = WorkerPerf {
                window_secs: elapsed,
                captured_frames: c_captured,
                submitted_frames: c_submitted,
                duplicated_frames: c_duplicated,
                encoded_packets: c_packets,
                encoded_bytes: c_bytes,
                published_epoch: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };
            if let Ok(mut p) = perf.lock() {
                *p = snap;
            }
            crate::diag(
                &app,
                format!(
                    "[replay] perf {target:?} {elapsed:.1}s — cap={c_captured} ({cap_fps:.1}fps) sub={c_submitted} ({sub_fps:.1}fps) dup={c_duplicated} pkts={c_packets} {}KB/s",
                    bytes_per_sec / 1024
                ),
            );
            roll_start = now_inst;
            c_captured = 0;
            c_submitted = 0;
            c_duplicated = 0;
            c_packets = 0;
            c_bytes = 0;
        }

        // 1. Drain control commands first so Stop/Save/Pause/Resume are responsive.
        loop {
            match cmd_rx.try_recv() {
                Ok(WorkerCmd::Stop) => break 'main,
                Ok(WorkerCmd::Snapshot(reply)) => {
                    // Clone the video buffer on this thread (cheap — VideoPacket
                    // wraps Arc<[u8]>, so .cloned() is just refcount bumps), then
                    // hand off to a short-lived thread to do the slower audio
                    // round-trips (each handle.snapshot() blocks on its audio
                    // thread). Worker keeps pumping encoder events meanwhile so
                    // CFR pacing isn't disturbed.
                    let video: Vec<VideoPacket> = buffer.iter().cloned().collect();
                    let handles_for_thread = Arc::clone(&audio_handles);
                    let _ = thread::Builder::new()
                        .name("clippy-replay-snapshot".into())
                        .spawn(move || {
                            let mut audio_tracks: Vec<AudioTrackSnapshot> = Vec::new();
                            for (handle, name) in handles_for_thread.iter() {
                                if let Ok(packets) = handle.snapshot() {
                                    audio_tracks.push(AudioTrackSnapshot {
                                        format: handle.format().clone(),
                                        packets,
                                        name: name.clone(),
                                    });
                                }
                            }
                            let _ = reply.send(Ok(WorkerSnapshot {
                                video,
                                audio_tracks,
                            }));
                        });
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'main,
            }
        }

        // 2. Pump MFT events non-blocking until queue is empty.
        loop {
            let event = match unsafe { event_gen.GetEvent(nowait) } {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                Err(_) => break, // unexpected, fall through and retry next tick
            };
            let etype = unsafe { event.GetType() }.unwrap_or(0);
            if etype == METransformNeedInput.0 as u32 {
                have_pending_input = true;
            } else if etype == METransformHaveOutput.0 as u32 {
                match process_output_async(&encoder) {
                    Ok(Some((data, pts, is_keyframe))) => {
                        c_packets = c_packets.saturating_add(1);
                        c_bytes = c_bytes.saturating_add(data.len() as u64);
                        buffer.push_back(VideoPacket {
                            data: data.into(),
                            pts,
                            is_keyframe,
                        });
                        trim_buffer(&mut buffer, duration_pts);
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
            }
        }

        // 3. Pacing-driven submission.
        //
        // We feed the encoder one frame per `frame_interval` of wall-clock
        // time. If WGC has a fresh BGRA frame, we convert it (overwriting
        // the video processor's NV12 texture). If not — static window,
        // capped-fps game, etc. — we just resubmit the previous NV12, which
        // keeps PTS contiguous and saved-video playback smooth at FPS.
        //
        // The worker captures continuously; focus changes don't pause it
        // (the saved video reflects real-time game state including AFK
        // moments). Coordinator just decides which buffer the user's save
        // hotkey targets.
        if have_pending_input {
            let now = Instant::now();
            if now.duration_since(last_submit_time) >= frame_interval {
                let mut got_fresh_this_tick = false;
                if let Ok(frame) = session.frame_pool.TryGetNextFrame() {
                    c_captured = c_captured.saturating_add(1);
                    if let Ok(bgra) = extract_texture_from_frame(&frame) {
                        if vp.convert(&bgra).is_ok() {
                            nv12_ready = true;
                            got_fresh_this_tick = true;
                        }
                    }
                }

                if nv12_ready {
                    // Wall-clock PTS aligned to the shared epoch — keeps
                    // audio and video in sync regardless of pacing drift.
                    // Quantize down to a frame_duration_pts boundary and
                    // ensure strict monotonicity vs the previous frame.
                    let elapsed_pts = (now - epoch).as_nanos() as i64 / 100;
                    let mut pts =
                        (elapsed_pts / frame_duration_pts) * frame_duration_pts;
                    if pts <= next_pts && next_pts > 0 {
                        pts = next_pts + frame_duration_pts;
                    }
                    if submit_nv12_texture(
                        &encoder,
                        vp.nv12_texture(),
                        pts,
                        frame_duration_pts,
                    )
                    .is_ok()
                    {
                        c_submitted = c_submitted.saturating_add(1);
                        if !got_fresh_this_tick {
                            c_duplicated = c_duplicated.saturating_add(1);
                        }
                        next_pts = pts;
                        have_pending_input = false;
                        last_submit_time = now;
                    }
                }
            }
        }

        // 4. Publish status (cheap, every iteration).
        {
            let buffered_pts = match (buffer.front(), buffer.back()) {
                (Some(f), Some(b)) => (b.pts - f.pts).max(0),
                _ => 0,
            };
            let buffered_secs = (buffered_pts / 10_000_000) as u32;
            if let Ok(mut s) = status.lock() {
                *s = ReplayStatus::Active {
                    window_title: title.clone(),
                    buffered_secs,
                    vram_mb: 0, // tracking lands in a later phase
                };
            }
        }

        // 5. Brief sleep to keep this loop from pegging a core. If no input
        // is pending and no frame arrived, we're effectively idle here.
        thread::sleep(Duration::from_millis(2));
    }

    // Shutdown — drain whatever the encoder still has buffered so a save
    // immediately after stop sees a complete tail.
    unsafe {
        let _ = encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        let _ = encoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
    }
    for _ in 0..512 {
        let event = match unsafe { event_gen.GetEvent(block) } {
            Ok(e) => e,
            Err(_) => break,
        };
        let etype = unsafe { event.GetType() }.unwrap_or(0);
        if etype == METransformHaveOutput.0 as u32 {
            if let Ok(Some((data, pts, is_keyframe))) = process_output_async(&encoder) {
                buffer.push_back(VideoPacket {
                    data: data.into(),
                    pts,
                    is_keyframe,
                });
            }
        } else {
            // Drain-complete or something else — stop draining.
            break;
        }
    }

    session.session.Close().ok();
    mf_shutdown();

    if let Ok(mut s) = status.lock() {
        *s = ReplayStatus::Idle;
    }

    // Keep bundle / vp / encoder / device_manager bound until here so their
    // Drop order is determined explicitly: capture → vproc → encoder → device.
    drop(vp);
    drop(encoder);
    drop(bundle);
}

#[cfg(not(windows))]
fn run_worker(
    _target: CaptureTarget,
    _settings: ReplaySettings,
    _cmd_rx: mpsc::Receiver<WorkerCmd>,
    _status: Arc<Mutex<ReplayStatus>>,
    _perf: Arc<Mutex<WorkerPerf>>,
    init_tx: mpsc::SyncSender<Result<(u32, u32, u32, String), String>>,
    _app: tauri::AppHandle,
) {
    let _ = init_tx.send(Err("replay buffer is Windows-only".into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(pts: i64, is_keyframe: bool) -> VideoPacket {
        VideoPacket {
            data: Arc::from(Vec::<u8>::new().into_boxed_slice()),
            pts,
            is_keyframe,
        }
    }

    fn buf<I: IntoIterator<Item = (i64, bool)>>(it: I) -> VecDeque<VideoPacket> {
        it.into_iter().map(|(p, k)| vp(p, k)).collect()
    }

    #[test]
    fn trim_buffer_noop_on_empty() {
        let mut b = VecDeque::<VideoPacket>::new();
        trim_buffer(&mut b, 30);
        assert!(b.is_empty());
    }

    #[test]
    fn trim_buffer_drops_to_first_keyframe_inside_window() {
        // PTS step 10, keyframes every 30. Latest=100, duration_pts=30, cutoff=70.
        // First keyframe with pts >= 70 is at pts=90 (index 9). Drop indices 0..9.
        let mut b = buf([
            (10, true),
            (20, false),
            (30, false),
            (40, true),
            (50, false),
            (60, false),
            (70, true),
            (80, false),
            (90, true),
            (100, false),
        ]);
        trim_buffer(&mut b, 30);
        let kept: Vec<(i64, bool)> = b.iter().map(|p| (p.pts, p.is_keyframe)).collect();
        // Trim anchors to the first keyframe >= cutoff (pts=70).
        assert_eq!(kept, vec![(70, true), (80, false), (90, true), (100, false)]);
        // Invariant: first kept packet is a keyframe so the buffer is decodable.
        assert!(kept[0].1);
    }

    #[test]
    fn trim_buffer_keeps_decodable_invariant_after_trim() {
        // Same shape, smaller window — first keyframe >= cutoff is at pts=90.
        let mut b = buf([
            (10, true),
            (40, true),
            (70, true),
            (90, true),
            (100, false),
        ]);
        trim_buffer(&mut b, 15); // cutoff = 100 - 15 = 85
        assert!(b.front().unwrap().is_keyframe);
        assert!(b.front().unwrap().pts >= 85);
    }

    #[test]
    fn trim_buffer_does_not_drop_when_no_keyframe_in_window() {
        // Only keyframe is older than cutoff; current code preserves buffer
        // rather than orphaning a non-keyframe head. Lock the behavior in.
        let mut b = buf([(10, true), (20, false), (30, false), (100, false)]);
        let before: Vec<i64> = b.iter().map(|p| p.pts).collect();
        trim_buffer(&mut b, 30); // cutoff = 70, no keyframe >= 70
        let after: Vec<i64> = b.iter().map(|p| p.pts).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn trim_buffer_noop_when_first_packet_already_satisfies() {
        // Front is the keyframe >= cutoff → drop_before stays 0 → no drain.
        let mut b = buf([(70, true), (80, false), (90, true), (100, false)]);
        let before: Vec<i64> = b.iter().map(|p| p.pts).collect();
        trim_buffer(&mut b, 30); // cutoff = 70
        let after: Vec<i64> = b.iter().map(|p| p.pts).collect();
        assert_eq!(before, after);
    }
}
