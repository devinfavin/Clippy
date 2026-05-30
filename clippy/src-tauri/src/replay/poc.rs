//! Phase 1 / Stage A / Stage B PoC pipeline-validation commands.
//!
//! Feature-gated behind `--features poc`; not compiled into default release
//! builds. Three Tauri commands drive incrementally-more-GPU pipelines so a
//! developer can confirm each stage end-to-end:
//!
//! - `replay_poc_test`        — software encode, CPU readback
//! - `replay_poc_gpu_convert` — GPU color convert, CPU readback for encode
//! - `replay_poc_gpu_full`    — full GPU + async hardware encoder
//!
//! Output MP4s land in `%TEMP%` and can be opened directly from devtools.

#![cfg(feature = "poc")]

/// Phase 1 end-to-end pipeline test.
///
/// Captures ~90 frames from the current foreground window, converts BGRA→NV12
/// in software, encodes with Media Foundation H.264, muxes with FFmpeg, and
/// returns the path to the output MP4. Invoke from Tauri devtools:
///
///   await window.__TAURI__.core.invoke('replay_poc_test')
///
/// Open the returned path in Clippy to verify the video plays.
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

    let ffmpeg = super::save::ffmpeg_path()?;

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
#[cfg(windows)]
fn poc_capture_and_encode() -> Result<(Vec<u8>, usize, u32, u32), String> {
    use super::capture::windows_impl::*;
    use super::convert::windows_impl::*;
    use super::encoder::windows_impl::*;
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

    let session =
        open_capture_session(hwnd, &bundle.device).map_err(|e| format!("WGC session: {e}"))?;

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

            let bgra =
                readback_bgra_texture(&bundle.device, &bundle.context, &texture, width, height)
                    .map_err(|e| format!("readback f{frame_count}: {e}"))?;

            let nv12 = bgra_to_nv12(
                &bgra,
                width as usize,
                height as usize,
                enc_width as usize,
                enc_height as usize,
            );
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

#[cfg(not(windows))]
fn poc_capture_and_encode() -> Result<(Vec<u8>, usize, u32, u32), String> {
    Err("replay buffer is Windows-only".into())
}

// ---------- Stage A: GPU color conversion validation ----------

/// Stage A PoC: validates the D3D11 video processor (BGRA→NV12 on GPU)
/// while still using the software encoder for the encode step. NV12 is read
/// back from GPU just for the encoder hand-off — once Stage B lands, the
/// readback goes away entirely. Output should look identical to the CPU PoC.
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

    let ffmpeg = super::save::ffmpeg_path()?;

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

#[cfg(windows)]
fn poc_gpu_convert_and_encode() -> Result<(Vec<u8>, usize, u32, u32), String> {
    use super::capture::windows_impl::*;
    use super::encoder::windows_impl::*;
    use super::vproc::windows_impl::VideoProcessor;
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

    let session =
        open_capture_session(hwnd, &bundle.device).map_err(|e| format!("WGC session: {e}"))?;

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
            let bgra_tex = super::capture::windows_impl::extract_texture_from_frame(&frame)
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
        return Err(format!("no encoded output after {frame_count} frames"));
    }

    Ok((all_h264, frame_count, enc_width, enc_height))
}

#[cfg(not(windows))]
fn poc_gpu_convert_and_encode() -> Result<(Vec<u8>, usize, u32, u32), String> {
    Err("Windows only".into())
}

// ---------- Stage B: full GPU + async hardware encoder ----------

/// Stage B PoC: production path. WGC → D3D11 BGRA → video processor NV12 →
/// async hardware H.264 encoder via DXGI surface buffer. Zero CPU readback.
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

    let ffmpeg = super::save::ffmpeg_path()?;

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

#[cfg(windows)]
fn poc_gpu_full() -> Result<(Vec<u8>, usize, u32, u32, String), String> {
    use super::capture::windows_impl::*;
    use super::encoder::windows_impl::*;
    use super::vproc::windows_impl::VideoProcessor;
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

    let session =
        open_capture_session(hwnd, &bundle.device).map_err(|e| format!("WGC session: {e}"))?;

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
    let (encoder, _device_manager, _encoder_name, _rate_report) = create_h264_encoder_hw_async(
        &bundle.device,
        enc_width,
        enc_height,
        50_000,
        60,
        1,
        super::EncoderPreference::Auto,
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
            let event = unsafe {
                event_gen.GetEvent(
                    windows::Win32::Media::MediaFoundation::MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(
                        0,
                    ),
                )
            }
            .map_err(|e| format!("GetEvent: {e}"))?;
            let etype = unsafe { event.GetType() }.map_err(|e| format!("event type: {e}"))?;

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
        let event = match unsafe {
            event_gen.GetEvent(
                windows::Win32::Media::MediaFoundation::MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0),
            )
        } {
            Ok(e) => e,
            Err(_) => break,
        };
        let etype = unsafe { event.GetType() }.unwrap_or(0);
        if etype == METransformHaveOutput.0 as u32 {
            if let Some((data, _, _)) =
                process_output_async(&encoder).map_err(|e| format!("final drain: {e}"))?
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

#[cfg(not(windows))]
fn poc_gpu_full() -> Result<(Vec<u8>, usize, u32, u32, String), String> {
    Err("Windows only".into())
}
