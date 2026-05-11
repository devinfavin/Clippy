//! Save pipeline: snapshot encoded packets → raw .h264 + raw PCM tracks →
//! FFmpeg mux to MP4 with one audio stream per captured device.

use std::path::{Path, PathBuf};

use super::buffer::VideoPacket;
use super::worker::AudioTrackSnapshot;

/// Concatenate the H.264 packets in PTS order and write to disk.
pub async fn write_h264_raw(packets: &[VideoPacket], path: &Path) -> std::io::Result<()> {
    let mut bytes: Vec<u8> = Vec::with_capacity(packets.iter().map(|p| p.data.len()).sum());
    for p in packets {
        bytes.extend_from_slice(&p.data);
    }
    tokio::fs::write(path, bytes).await
}

/// Write the raw PCM bytes of an audio track in PTS order.
async fn write_pcm_raw(track: &AudioTrackSnapshot, path: &Path) -> std::io::Result<()> {
    let mut bytes: Vec<u8> =
        Vec::with_capacity(track.packets.iter().map(|p| p.data.len()).sum());
    for p in &track.packets {
        bytes.extend_from_slice(&p.data);
    }
    tokio::fs::write(path, bytes).await
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
    let result = tokio::process::Command::new(&ffmpeg)
        .args(&args)
        .output()
        .await;

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
