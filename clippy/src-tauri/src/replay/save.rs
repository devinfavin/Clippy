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

/// Resolve a sidecar binary (ffmpeg / ffprobe / etc.) across the layouts we
/// ship from:
///   - **Production (NSIS install)**: Tauri's bundler strips the target-triple
///     suffix on install, so the file sits next to the exe as plain
///     `<name>.exe`. This is the path used by friend installs from a release.
///   - **Production (raw `cargo tauri build` output)**: same dir as the exe,
///     also as `<name>.exe`.
///   - **`tauri dev`**: exe lives in `target/debug/`, sidecars stay in
///     `src-tauri/binaries/` with the full target-triple suffix preserved.
///
/// Resolution order: prod plain → prod suffixed (rare) → dev suffixed.
/// First miss falls through; only the final option is returned without an
/// `exists()` check so the spawn error surfaces with the path it tried.
fn sidecar_path(name: &str) -> Result<PathBuf, String> {
    let exe_dir = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("no exe parent dir")?
        .to_path_buf();
    // Installed layout (NSIS strips the target-triple suffix).
    let prod_plain = exe_dir.join(format!("{name}.exe"));
    if prod_plain.exists() {
        return Ok(prod_plain);
    }
    // Defensive: some bundlers leave the suffix intact next to the exe.
    let prod_suffixed = exe_dir.join(format!("{name}-x86_64-pc-windows-msvc.exe"));
    if prod_suffixed.exists() {
        return Ok(prod_suffixed);
    }
    // `tauri dev` — exe is at target/debug/, sidecar at src-tauri/binaries/.
    let dev = exe_dir
        .join("..")
        .join("..")
        .join("binaries")
        .join(format!("{name}-x86_64-pc-windows-msvc.exe"));
    Ok(dev)
}

pub fn ffmpeg_path() -> Result<PathBuf, String> {
    sidecar_path("ffmpeg")
}

fn ffprobe_path() -> Result<PathBuf, String> {
    sidecar_path("ffprobe")
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

/// Defense-in-depth sanitization for user-supplied strings that flow into
/// either ffmpeg metadata args or diag log entries. Strips all control
/// characters (newlines, NUL, ESC, DEL, C1 controls) and caps the result at
/// `max` Unicode scalar values. Truncation is on char boundary (never
/// splits a multi-byte codepoint).
///
/// Why this exists: `track.name` originates from the frontend's audio
/// device-rename input, the WGC `DisplayName` of a game window, or our own
/// `"Game audio"` / `"Default output"` fallbacks. Any of those can in
/// principle contain control characters or be arbitrarily long. Without
/// truncation a 5000-char game window title would land verbatim in the
/// MP4 metadata title field, and a `\n`-bearing rename would leak a
/// newline into the diag log (corrupting the line-per-entry contract).
pub(super) fn truncate_for_metadata(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut count = 0usize;
    for c in s.chars() {
        if count >= max {
            break;
        }
        if c.is_control() {
            continue;
        }
        out.push(c);
        count += 1;
    }
    out
}

/// Whether the SPS-rewrite pre-pass is needed for this encoder.
///
/// AMD's H.264 MFT writes a SPS VUI timing block that ffmpeg's H.264 parser
/// reads as half-rate (e.g. 30fps for a 60fps capture), which would double
/// the saved-clip duration. NVENC, QSV, and the software encoder write
/// correct SPS and don't need the rewrite — running it on them is ~7-10s of
/// wasted ffmpeg time per minute of clip (CBS has to parse every NAL header
/// to find the SPS).
///
/// Match is case-insensitive substring on common identifiers Microsoft +
/// vendors use in `MFT_FRIENDLY_NAME`. Empty string (encoder MFT didn't
/// expose a name) falls through to `true` so we don't ship a broken clip
/// when we can't tell — the cost is observable in the save log and the
/// post-save ffprobe (task 3.2) will warn-log if our guess was wrong.
pub(crate) fn needs_bsf_pass(encoder_name: &str) -> bool {
    if encoder_name.is_empty() {
        return true;
    }
    let lower = encoder_name.to_lowercase();
    lower.contains("amd") || lower.contains("amf")
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
    pub probe_ms: u64,
    pub total_ms: u64,
    pub h264_bytes: u64,
    pub pcm_bytes: u64,
    /// Post-save ffprobe verification. `Some((probed_fps, duration_secs))`
    /// when the probe succeeded; `None` when ffprobe failed or didn't parse
    /// (probe is observability-only and never fails the save). Caller
    /// compares probed_fps against `effective_fps` (NOT the configured `fps`)
    /// and warn-logs on a >0.5fps mismatch.
    pub probed: Option<(f64, f64)>,
    /// Framerate the video was muxed at — computed from the actual encoded
    /// PTS span, not the worker's configured `fps`. When the encoder
    /// back-pressures below the configured rate (AMD's H.264 MFT under
    /// concurrent load is the observed case), the buffer ends up with N
    /// packets over a wall-clock span > N/fps, so muxing at the configured
    /// rate produces a video shorter than the wall-clock window it covers.
    /// Audio (captured at native sample rate) ends up longer than the
    /// video — visible to the user as waveform/video drift across the clip.
    /// Muxing at `effective_fps` instead keeps video duration == audio
    /// duration == wall-clock duration.
    pub effective_fps: f64,
}

/// Probe the muxed output and parse its first video stream's frame rate +
/// duration. Returns `None` on any failure (ffprobe missing, non-zero exit,
/// unparseable output). Quick — usually <100ms.
async fn probe_saved_clip(mp4: &Path) -> Option<(f64, f64)> {
    let probe = ffprobe_path().ok()?;
    let result = {
        let mut cmd = tokio::process::Command::new(&probe);
        cmd.args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=r_frame_rate,duration",
            "-of",
            "csv=p=0",
            mp4.to_string_lossy().as_ref(),
        ]);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        cmd.output().await.ok()?
    };
    if !result.status.success() {
        return None;
    }
    // Expected output format: "60/1,60.000000\n"
    let s = String::from_utf8_lossy(&result.stdout);
    let line = s.lines().next()?.trim();
    let mut parts = line.split(',');
    let rate = parts.next()?;
    let duration: f64 = parts.next()?.parse().ok()?;
    let mut rate_parts = rate.split('/');
    let num: f64 = rate_parts.next()?.parse().ok()?;
    let den: f64 = rate_parts.next()?.parse().ok()?;
    if den == 0.0 {
        return None;
    }
    Some((num / den, duration))
}

/// Write the snapshot to disk and mux into a single MP4 with video + N audio
/// tracks. Video is stream-copied; audio is encoded to AAC at 192kbps each.
pub async fn write_and_mux(
    packets: &[VideoPacket],
    audio_tracks: &[AudioTrackSnapshot],
    fps: u32,
    encoder_name: &str,
    out_mp4: &Path,
) -> Result<SaveTimings, String> {
    use std::time::Instant;
    let total_start = Instant::now();
    if packets.is_empty() {
        return Err("buffer is empty".into());
    }

    // Staging directory for `.h264` / `.fixed.h264` / `.aN.pcm` intermediates.
    // Kept under the OS temp dir (rather than next to the output MP4) so a
    // save to a slow/network/external drive doesn't pay double the IO — only
    // the final mux output lands at the user's save location. Per-save
    // subdir suffixed with wall-clock nanos so two saves in the same second
    // (e.g. hotkey mash) don't share temp files and clobber each other on
    // cleanup.
    let stem = out_mp4
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("clippy-save");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let save_dir = std::env::temp_dir()
        .join("clippy-save")
        .join(format!("{stem}-{nanos}"));
    std::fs::create_dir_all(&save_dir)
        .map_err(|e| format!("create save temp dir {}: {e}", save_dir.display()))?;

    // Stage video.
    let h264_path = save_dir.join("raw.h264");
    let h264_bytes: u64 = packets.iter().map(|p| p.data.len() as u64).sum();
    let h264_start = Instant::now();
    if let Err(e) = write_h264_raw(packets, &h264_path).await {
        let _ = tokio::fs::remove_dir_all(&save_dir).await;
        return Err(format!("write h264: {e}"));
    }
    let h264_write_ms = h264_start.elapsed().as_millis() as u64;

    // Stage each audio track as a separate raw PCM file.
    let pcm_start = Instant::now();
    let mut pcm_bytes: u64 = 0;
    let mut audio_paths: Vec<(PathBuf, &AudioTrackSnapshot)> = Vec::new();
    for (i, track) in audio_tracks.iter().enumerate() {
        if track.packets.is_empty() {
            continue;
        }
        let path = save_dir.join(format!("a{i}.pcm"));
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

    let ffmpeg = ffmpeg_path()?;

    // Pass 1 (AMD only): rewrite the H.264 SPS VUI timing in the raw stream
    // BEFORE the main mux reads it. The AMD encoder MFT writes SPS that
    // ffmpeg's H.264 parser interprets as half-rate (e.g. 30fps for a 60fps
    // capture); that locks the parser's packet PTS spacing to 1/30s during
    // demux, doubling the saved-clip duration. An output bsf can't fix this
    // — output bsfs run AFTER the muxer has already received packets with
    // their (wrong) PTS. So we do a tiny pre-pass that rewrites the SPS
    // in-place; the main mux below then sees a stream the parser interprets
    // at full rate.
    //
    // NVENC, QSV, and the software encoder all write correct SPS and don't
    // need this — skipping the pass saves ~7-10s per minute of clip there
    // because CBS has to parse every NAL header to find the SPS. The
    // post-save ffprobe verification (task 3.2) catches the case where a
    // new encoder vendor also has the AMD bug and we missed it in the gate.
    let needs_bsf = needs_bsf_pass(encoder_name);
    let bsf_start = Instant::now();
    let mux_input_path: PathBuf = if needs_bsf {
        let h264_fixed_path = save_dir.join("fixed.h264");
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
        match &bsf_result {
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&save_dir).await;
                return Err(format!("ffmpeg sps-rewrite spawn ({ffmpeg:?}): {e}"));
            }
            Ok(out) if !out.status.success() => {
                let _ = tokio::fs::remove_dir_all(&save_dir).await;
                return Err(format!(
                    "ffmpeg sps-rewrite failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            _ => {}
        }
        h264_fixed_path
    } else {
        h264_path.clone()
    };
    let bsf_pass_ms = bsf_start.elapsed().as_millis() as u64;

    // Compute the actual framerate of the captured bitstream. The worker's
    // pacing targets `fps` (e.g. 60), but the encoder can back-pressure below
    // that — observed on AMD's H.264 MFT producing ~56.5 fps under 2K capture
    // load even when WGC was supplying fresh frames. The buffer then holds
    // (say) 3376 packets spanning 60 s of wall-clock, but muxing at the
    // configured 60 fps stretches that into a 56.27 s video alongside the
    // 60 s audio — visible to the user as waveform/visual drift accumulating
    // from 0 s at the clip start to ~3.7 s at the end.
    //
    // Fix: derive framerate from packet count / encoded PTS span and pass
    // that to ffmpeg's H.264 raw demuxer. Video duration then matches audio
    // duration matches wall-clock duration.
    //
    // PTS span is in 100-ns units (Media Foundation's REFERENCE_TIME). For a
    // partially-filled buffer (worker just started, <0.5 s of capture) we
    // fall back to the configured fps since the span is too short to
    // estimate reliably.
    let effective_fps: f64 = if packets.len() >= 2 {
        let first_pts = packets.first().map(|p| p.pts).unwrap_or(0);
        let last_pts = packets.last().map(|p| p.pts).unwrap_or(0);
        let span_secs = ((last_pts - first_pts) as f64 / 10_000_000.0).max(0.0);
        if span_secs >= 0.5 {
            (packets.len() as f64 / span_secs).max(1.0)
        } else {
            fps as f64
        }
    } else {
        fps as f64
    };

    // Build FFmpeg command:
    //   ffmpeg -y
    //          -r EFFECTIVE_FPS -framerate EFFECTIVE_FPS -f h264 -i video.fixed.h264
    //          [-f f32le -ar SR -ac CH -i a0.pcm  ...for each track]
    //          -c:v copy -c:a aac -b:a 192k
    //          -map 0:v [-map 1:a -map 2:a ...]
    //          out.mp4
    //
    // Both `-r` and `-framerate` here are INPUT options (placed before -i)
    // for the H.264 raw demuxer. Belt and suspenders:
    //   - `-framerate` is the H.264 raw demuxer's preferred rate setting,
    //     used when the bitstream has no SPS-VUI timing info.
    //   - `-r` as an input option additionally instructs ffmpeg to IGNORE
    //     timestamps stored in the file and generate at constant rate.
    //     The bsf pass for AMD rewrites SPS to declare tick_rate=fps*2,
    //     which the raw demuxer would otherwise treat as authoritative —
    //     winning over `-framerate` and pinning the output to the bsf-
    //     declared rate (observed: code requested 55.88fps but ffprobe
    //     reported 60.00fps, duration 55.82s instead of 60s). `-r` is
    //     documented to override that.
    let mut args: Vec<String> = Vec::new();
    args.push("-y".into());
    args.push("-r".into());
    args.push(format!("{:.4}", effective_fps));
    args.push("-framerate".into());
    args.push(format!("{:.4}", effective_fps));
    args.push("-f".into());
    args.push("h264".into());
    args.push("-i".into());
    args.push(mux_input_path.to_string_lossy().into_owned());

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
            // Sanitize defensively — name flows from the frontend rename UI
            // and from WGC DisplayName, neither of which we control.
            let safe_name = truncate_for_metadata(&track.name, 128);
            if !safe_name.is_empty() {
                args.push(format!("-metadata:s:a:{i}"));
                args.push(format!("title={safe_name}"));
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

    // Best-effort cleanup regardless of mux outcome — single recursive remove
    // because every intermediate (.h264, .fixed.h264, .aN.pcm) lives in the
    // per-save subdir we created up top.
    let _ = tokio::fs::remove_dir_all(&save_dir).await;

    let out = result.map_err(|e| format!("ffmpeg spawn ({ffmpeg:?}): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg mux failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    // Post-save ffprobe verification. Never blocks; returns None if probe
    // failed for any reason. Caller does the expected-vs-actual comparison.
    let probe_start = Instant::now();
    let probed = probe_saved_clip(out_mp4).await;
    let probe_ms = probe_start.elapsed().as_millis() as u64;

    Ok(SaveTimings {
        h264_write_ms,
        pcm_write_ms,
        bsf_pass_ms,
        ffmpeg_mux_ms,
        probe_ms,
        total_ms: total_start.elapsed().as_millis() as u64,
        h264_bytes,
        pcm_bytes,
        probed,
        effective_fps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- needs_bsf_pass coverage ----------
    //
    // The gate decides whether the save pipeline pays ~7-10s of ffmpeg
    // pre-pass per minute of clip. Wrong "false" → AMD-style clips ship
    // with broken duration. Wrong "true" → 40s of wasted work on every
    // save. Both regressions are invisible at the unit-test level without
    // these.

    #[test]
    fn bsf_required_for_amd_friendly_name() {
        // Observed encoder names from our dev machine; AMD MFT writes the
        // half-rate SPS that the pre-pass fixes.
        assert!(needs_bsf_pass("AMDh264Encoder"));
        assert!(needs_bsf_pass("AMD H.264 Encoder MFT"));
        assert!(needs_bsf_pass("AMF H.264 Encoder"));
        assert!(needs_bsf_pass("amd h264 hardware encoder"));
    }

    #[test]
    fn bsf_skipped_for_nvidia_intel_software() {
        // These vendors write spec-conformant SPS — skipping saves the
        // CBS parse pass.
        assert!(!needs_bsf_pass("NVIDIA H.264 Encoder MFT"));
        assert!(!needs_bsf_pass("Intel® Quick Sync Video H.264 Encoder MFT"));
        assert!(!needs_bsf_pass("Intel Hardware H.264 Encoder MFT"));
        assert!(!needs_bsf_pass("Microsoft H264 Video Encoder MFT"));
    }

    #[test]
    fn bsf_defaults_to_running_when_name_unknown() {
        // Empty string = MF didn't expose a friendly name. Run the pass
        // defensively so a clip from an unknown vendor isn't shipped with
        // broken timing.
        assert!(needs_bsf_pass(""));
    }

    #[test]
    fn bsf_match_is_case_insensitive() {
        assert!(needs_bsf_pass("amd"));
        assert!(needs_bsf_pass("AMD"));
        assert!(needs_bsf_pass("AmD"));
        assert!(needs_bsf_pass("amf"));
        assert!(needs_bsf_pass("AMF"));
    }

    // ---------- truncate_for_metadata coverage ----------

    #[test]
    fn truncate_caps_at_max_chars() {
        assert_eq!(truncate_for_metadata("abcdefghij", 5), "abcde");
        assert_eq!(truncate_for_metadata("abcdefghij", 10), "abcdefghij");
        assert_eq!(truncate_for_metadata("abcdefghij", 100), "abcdefghij");
    }

    #[test]
    fn truncate_strips_control_chars() {
        // Newlines, CR, NUL, tabs, ESC — all dropped without occupying a slot
        // in the char budget.
        let dirty = "good\nname\rwith\tcontrol\x00chars";
        assert_eq!(truncate_for_metadata(dirty, 128), "goodnamewithcontrolchars");
        // C1 controls (0x80-0x9F) also stripped.
        assert_eq!(truncate_for_metadata("hi\u{0085}there", 128), "hithere");
    }

    #[test]
    fn truncate_strips_dont_count_against_budget() {
        // If we have 10 letters interleaved with newlines, max=5 should
        // still return 5 letters (not stop early because of the strips).
        assert_eq!(truncate_for_metadata("a\nb\nc\nd\ne\nf\ng", 5), "abcde");
    }

    #[test]
    fn truncate_preserves_unicode_on_char_boundary() {
        // Multi-byte codepoints count as one char each. Cap at 3 chars on
        // a 9-byte 3-emoji string returns all three.
        let emojis = "🎮🎯🎲";
        assert_eq!(truncate_for_metadata(emojis, 3), "🎮🎯🎲");
        assert_eq!(truncate_for_metadata(emojis, 2), "🎮🎯");
        // Mixed: ASCII + multi-byte. Cap at 4 chars.
        assert_eq!(truncate_for_metadata("abc🎮def", 4), "abc🎮");
    }

    #[test]
    fn truncate_handles_empty_input() {
        assert_eq!(truncate_for_metadata("", 128), "");
    }

    #[test]
    fn truncate_handles_zero_max() {
        assert_eq!(truncate_for_metadata("anything", 0), "");
    }

    #[test]
    fn truncate_preserves_normal_punctuation_and_space() {
        // " ", "-", quotes, punctuation are all NOT control chars — kept.
        let s = "Stream A - \"Game Audio\" #1!";
        assert_eq!(truncate_for_metadata(s, 128), s);
    }
}
