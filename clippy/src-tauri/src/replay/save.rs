//! Save pipeline: snapshot encoded packets → raw .h264 + raw PCM tracks →
//! FFmpeg mux to MP4 with one audio stream per captured device.

use std::path::{Path, PathBuf};

use super::buffer::VideoPacket;
use super::worker::AudioTrackSnapshot;

/// Stream H.264 packets to disk in PTS order. Uses a buffered writer rather
/// than concatenating into one giant `Vec<u8>` first — for a 5-min @100 Mbps
/// buffer that single allocation alone was ~3.75 GB held alongside the
/// already-resident Arc-shared packet buffers.
pub async fn write_h264_raw(packets: &[VideoPacket], path: &Path) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let file = tokio::fs::File::create(path).await?;
    // 256 KB buffer — large enough to amortize syscall cost across many
    // small NALU packets, small enough to stay out of the way of other RAM.
    let mut writer = tokio::io::BufWriter::with_capacity(256 * 1024, file);
    for p in packets {
        writer.write_all(&p.data).await?;
    }
    writer.flush().await
}

/// Stream PCM bytes for one audio track to disk. Same rationale as
/// `write_h264_raw` — avoid the whole-track Vec allocation.
async fn write_pcm_raw(track: &AudioTrackSnapshot, path: &Path) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let file = tokio::fs::File::create(path).await?;
    let mut writer = tokio::io::BufWriter::with_capacity(256 * 1024, file);
    for p in &track.packets {
        writer.write_all(&p.data).await?;
    }
    writer.flush().await
}

/// Resolve the FFmpeg sidecar binary across the layouts we ship from:
///   - **Production (NSIS install)**: Tauri's bundler strips the target-triple
///     suffix on install, so the file sits next to the exe as plain
///     `ffmpeg.exe`. This is the path used by friend installs from a release.
///   - **Production (raw `cargo tauri build` output)**: same dir as the exe,
///     also as `ffmpeg.exe`.
///   - **`tauri dev`**: exe lives in `target/debug/`, sidecars stay in
///     `src-tauri/binaries/` with the full target-triple suffix preserved.
///
/// Resolution order: prod plain → prod suffixed (rare) → dev suffixed.
/// First miss falls through; only the final option is returned without an
/// `exists()` check so the spawn error surfaces with the path it tried.
pub fn ffmpeg_path() -> Result<PathBuf, String> {
    let exe_dir = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("no exe parent dir")?
        .to_path_buf();
    // Installed layout (NSIS strips the target-triple suffix).
    let prod_plain = exe_dir.join("ffmpeg.exe");
    if prod_plain.exists() {
        return Ok(prod_plain);
    }
    // Defensive: some bundlers leave the suffix intact next to the exe.
    let prod_suffixed = exe_dir.join("ffmpeg-x86_64-pc-windows-msvc.exe");
    if prod_suffixed.exists() {
        return Ok(prod_suffixed);
    }
    // `tauri dev` — exe is at target/debug/, sidecar at src-tauri/binaries/.
    let dev = exe_dir
        .join("..")
        .join("..")
        .join("binaries")
        .join("ffmpeg-x86_64-pc-windows-msvc.exe");
    Ok(dev)
}

/// FFmpeg `-f` value for raw PCM in the given format.
fn pcm_demuxer_flag(format: &super::audio::AudioFormat) -> &'static str {
    if format.is_float {
        // Most WASAPI mix formats are 32-bit IEEE float.
        "f32le"
    } else {
        match format.bits_per_sample {
            16 => "s16le",
            24 => "s24le",
            32 => "s32le",
            _ => "s16le",
        }
    }
}

/// Per-step timings for one `write_and_mux` invocation. Returned to the
/// caller so it can emit a diag breakdown like
/// `save · h264 130ms · pcm 22ms · bsf 800ms · ffmpeg 410ms · total 1342ms` —
/// pinpointing where a slow save spent its time.
pub struct SaveTimings {
    pub h264_write_ms: u64,
    pub pcm_write_ms: u64,
    pub bsf_pass_ms: u64,
    pub ffmpeg_mux_ms: u64,
    pub total_ms: u64,
    pub h264_bytes: u64,
    pub pcm_bytes: u64,
}

/// Write the snapshot to disk and mux into a single MP4 with video + N audio
/// tracks. Video is stream-copied; audio is encoded to AAC at 192kbps each.
pub async fn write_and_mux(
    packets: &[VideoPacket],
    audio_tracks: &[AudioTrackSnapshot],
    fps: u32,
    out_mp4: &Path,
) -> Result<SaveTimings, String> {
    use std::time::Instant;
    let total_start = Instant::now();
    if packets.is_empty() {
        return Err("buffer is empty".into());
    }

    // Stage video.
    let h264_path = out_mp4.with_extension("h264");
    let h264_bytes: u64 = packets.iter().map(|p| p.data.len() as u64).sum();
    let h264_start = Instant::now();
    write_h264_raw(packets, &h264_path)
        .await
        .map_err(|e| format!("write h264: {e}"))?;
    let h264_write_ms = h264_start.elapsed().as_millis() as u64;

    // Stage each audio track as a separate raw PCM file.
    let pcm_start = Instant::now();
    let mut pcm_bytes: u64 = 0;
    let mut audio_paths: Vec<(PathBuf, &AudioTrackSnapshot)> = Vec::new();
    for (i, track) in audio_tracks.iter().enumerate() {
        if track.packets.is_empty() {
            continue;
        }
        let path = out_mp4.with_extension(format!("a{i}.pcm"));
        if let Err(e) = write_pcm_raw(track, &path).await {
            // Skip this track but keep going — better to save without audio
            // than to fail the whole save.
            eprintln!("audio track {i} write failed: {e}");
            continue;
        }
        pcm_bytes += track.packets.iter().map(|p| p.data.len() as u64).sum::<u64>();
        audio_paths.push((path, track));
    }
    let pcm_write_ms = pcm_start.elapsed().as_millis() as u64;

    // Pass 1: rewrite the H.264 SPS VUI timing in the raw stream BEFORE the
    // main mux reads it. The AMD encoder MFT writes SPS that ffmpeg's H.264
    // parser interprets as half-rate (e.g. 30fps for a 60fps capture); that
    // locks the parser's packet PTS spacing to 1/30s during demux, doubling
    // the saved-clip duration. An output bsf can't fix this — output bsfs
    // run AFTER the muxer has already received packets with their (wrong)
    // PTS. So we do a tiny pre-pass that rewrites the SPS in-place; the
    // main mux below then sees a stream the parser interprets at full rate.
    let h264_fixed_path = out_mp4.with_extension("fixed.h264");
    let ffmpeg = ffmpeg_path()?;
    let bsf_start = Instant::now();
    let bsf_result = {
        let mut cmd = tokio::process::Command::new(&ffmpeg);
        cmd.args([
            "-y",
            "-i",
            h264_path.to_string_lossy().as_ref(),
            "-c",
            "copy",
            "-bsf:v",
            &format!(
                "h264_metadata=tick_rate={}:fixed_frame_rate_flag=1",
                fps * 2
            ),
            "-f",
            "h264",
            h264_fixed_path.to_string_lossy().as_ref(),
        ]);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        cmd.output().await
    };
    let bsf_pass_ms = bsf_start.elapsed().as_millis() as u64;
    match &bsf_result {
        Err(e) => {
            let _ = tokio::fs::remove_file(&h264_path).await;
            for (path, _) in &audio_paths {
                let _ = tokio::fs::remove_file(path).await;
            }
            return Err(format!("ffmpeg sps-rewrite spawn ({ffmpeg:?}): {e}"));
        }
        Ok(out) if !out.status.success() => {
            let _ = tokio::fs::remove_file(&h264_path).await;
            let _ = tokio::fs::remove_file(&h264_fixed_path).await;
            for (path, _) in &audio_paths {
                let _ = tokio::fs::remove_file(path).await;
            }
            return Err(format!(
                "ffmpeg sps-rewrite failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        _ => {}
    }

    // Build FFmpeg command:
    //   ffmpeg -y
    //          -framerate FPS -f h264 -i video.fixed.h264
    //          [-f f32le -ar SR -ac CH -i a0.pcm  ...for each track]
    //          -c:v copy -c:a aac -b:a 192k
    //          -map 0:v [-map 1:a -map 2:a ...]
    //          out.mp4
    let mut args: Vec<String> = Vec::new();
    args.push("-y".into());
    args.push("-framerate".into());
    args.push(fps.to_string());
    args.push("-f".into());
    args.push("h264".into());
    args.push("-i".into());
    args.push(h264_fixed_path.to_string_lossy().into_owned());

    for (path, track) in &audio_paths {
        args.push("-f".into());
        args.push(pcm_demuxer_flag(&track.format).into());
        args.push("-ar".into());
        args.push(track.format.sample_rate.to_string());
        args.push("-ac".into());
        args.push(track.format.channels.to_string());
        args.push("-i".into());
        args.push(path.to_string_lossy().into_owned());
    }

    args.push("-c:v".into());
    args.push("copy".into());
    args.push("-map".into());
    args.push("0:v".into());

    if !audio_paths.is_empty() {
        args.push("-c:a".into());
        args.push("aac".into());
        args.push("-b:a".into());
        args.push("192k".into());
        for (i, (_, track)) in audio_paths.iter().enumerate() {
            args.push("-map".into());
            args.push(format!("{}:a", i + 1));
            // Embed the track's friendly name as MP4 stream metadata so the
            // editor's mixer surfaces it instead of "Track 1 / Track 2".
            if !track.name.is_empty() {
                args.push(format!("-metadata:s:a:{i}"));
                args.push(format!("title={}", track.name));
            }
        }
    }

    args.push(out_mp4.to_string_lossy().into_owned());

    let mut cmd = tokio::process::Command::new(&ffmpeg);
    cmd.args(&args);
    // Suppress the flash-of-console-window when ffmpeg launches mid-game.
    // CREATE_NO_WINDOW (0x08000000) keeps the child process attached to no
    // console, which is what we want for a background mux during gameplay.
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let ffmpeg_start = Instant::now();
    let result = cmd.output().await;
    let ffmpeg_mux_ms = ffmpeg_start.elapsed().as_millis() as u64;

    // Best-effort cleanup regardless of mux outcome.
    let _ = tokio::fs::remove_file(&h264_path).await;
    let _ = tokio::fs::remove_file(&h264_fixed_path).await;
    for (path, _) in &audio_paths {
        let _ = tokio::fs::remove_file(path).await;
    }

    let out = result.map_err(|e| format!("ffmpeg spawn ({ffmpeg:?}): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg mux failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(SaveTimings {
        h264_write_ms,
        pcm_write_ms,
        bsf_pass_ms,
        ffmpeg_mux_ms,
        total_ms: total_start.elapsed().as_millis() as u64,
        h264_bytes,
        pcm_bytes,
    })
}
