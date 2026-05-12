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

/// Write the snapshot to disk and mux into a single MP4 with video + N audio
/// tracks. Video is stream-copied; audio is encoded to AAC at 192kbps each.
pub async fn write_and_mux(
    packets: &[VideoPacket],
    audio_tracks: &[AudioTrackSnapshot],
    fps: u32,
    out_mp4: &Path,
) -> Result<(), String> {
    if packets.is_empty() {
        return Err("buffer is empty".into());
    }

    // Stage video.
    let h264_path = out_mp4.with_extension("h264");
    write_h264_raw(packets, &h264_path)
        .await
        .map_err(|e| format!("write h264: {e}"))?;

    // Stage each audio track as a separate raw PCM file.
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
        audio_paths.push((path, track));
    }

    // Build FFmpeg command:
    //   ffmpeg -y
    //          -framerate FPS -f h264 -i video.h264
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
    args.push(h264_path.to_string_lossy().into_owned());

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
    // Force the output stream's frame rate explicitly. The input `-framerate
    // FPS` flag above tells the raw-H.264 demuxer the source rate, but with
    // `-c:v copy` the output container's reported frame rate ends up derived
    // from the encoded stream's SPS VUI timing — and the AMD encoder MFT
    // writes that as half-rate (60fps source → 30fps file), doubling the
    // apparent clip duration when the player divides frames-by-rate.
    // Forcing `-r FPS` on the output side makes the muxer's tbr deterministic
    // regardless of what the SPS contains.
    args.push("-r".into());
    args.push(fps.to_string());
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

    let ffmpeg = ffmpeg_path()?;
    let mut cmd = tokio::process::Command::new(&ffmpeg);
    cmd.args(&args);
    // Suppress the flash-of-console-window when ffmpeg launches mid-game.
    // CREATE_NO_WINDOW (0x08000000) keeps the child process attached to no
    // console, which is what we want for a background mux during gameplay.
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let result = cmd.output().await;

    // Best-effort cleanup regardless of mux outcome.
    let _ = tokio::fs::remove_file(&h264_path).await;
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
    Ok(())
}
