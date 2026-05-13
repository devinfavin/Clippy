use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use crate::diag::diag;
use crate::helpers::basename;
use crate::proxy::{proxy_cache_key, proxy_dir};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AudioTrack {
    /// Stream index *within the audio streams only* (0 = first audio track).
    /// This is what ffmpeg's `0:a:N` selector wants, NOT the absolute stream
    /// index, which differs across containers.
    index: usize,
    codec: String,
    channels: u32,
    /// Channel layout string from ffprobe (e.g. "stereo", "5.1"). Optional —
    /// some containers don't report it.
    layout: Option<String>,
    /// Title from stream metadata. SteelSeries Sonar / OBS often set this to
    /// "Game" / "Mic" / "Discord" etc; we surface it verbatim. None → fall
    /// back to "Track N+1" in the UI.
    title: Option<String>,
    language: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VideoInfo {
    pub duration_secs: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    /// First audio codec (kept for back-compat). Use `audio_tracks` for the
    /// full list when handling multi-track sources.
    pub audio_codec: Option<String>,
    pub audio_tracks: Vec<AudioTrack>,
    pub container: String,
    pub bit_rate_bps: Option<u64>,
}

fn parse_rate(s: &str) -> f64 {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() == 2 {
        let num: f64 = parts[0].parse().unwrap_or(0.0);
        let den: f64 = parts[1].parse().unwrap_or(1.0);
        if den == 0.0 { 0.0 } else { num / den }
    } else {
        s.parse().unwrap_or(0.0)
    }
}

async fn probe_video_inner(app: &AppHandle, path: &str) -> Result<VideoInfo, String> {
    let output = app
        .shell()
        .sidecar("ffprobe")
        .map_err(|e| e.to_string())?
        .args([
            "-v", "error",
            "-print_format", "json",
            "-show_format",
            "-show_streams",
            path,
        ])
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;

    let format = json.get("format").ok_or("no format section")?;
    let duration_secs: f64 = format
        .get("duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let container = format
        .get("format_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let bit_rate_bps = format
        .get("bit_rate")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok());

    let streams = json
        .get("streams")
        .and_then(|v| v.as_array())
        .ok_or("no streams")?;

    let video_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"))
        .ok_or("no video stream")?;

    // Walk every audio stream and build the per-track list. Index here is
    // a-stream-relative (0..N), matching ffmpeg's `0:a:N` selector.
    let mut audio_tracks: Vec<AudioTrack> = Vec::new();
    for s in streams.iter() {
        if s.get("codec_type").and_then(|v| v.as_str()) != Some("audio") {
            continue;
        }
        let idx = audio_tracks.len();
        let codec = s.get("codec_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let channels = s.get("channels").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let layout = s.get("channel_layout").and_then(|v| v.as_str()).map(String::from);
        let tags = s.get("tags");
        // Read `title` first (MKV, NUT, other containers — standard key),
        // fall back to `name` (ffmpeg's MP4 mov muxer routes per-stream
        // -metadata title=X into the track's `udta` atom under the key
        // `name`, which is what ffprobe surfaces for MP4 saves coming
        // out of Clippy's replay pipeline).
        let title = tags
            .and_then(|t| t.get("title").or_else(|| t.get("name")))
            .and_then(|v| v.as_str())
            .map(String::from);
        let language = tags
            .and_then(|t| t.get("language"))
            .and_then(|v| v.as_str())
            .map(String::from);
        audio_tracks.push(AudioTrack {
            index: idx,
            codec,
            channels,
            layout,
            title,
            language,
        });
    }

    let width = video_stream
        .get("width")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let height = video_stream
        .get("height")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let video_codec = video_stream
        .get("codec_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let r_frame_rate = video_stream
        .get("r_frame_rate")
        .and_then(|v| v.as_str())
        .unwrap_or("0/1");
    let fps = parse_rate(r_frame_rate);
    let audio_codec = audio_tracks.first().map(|t| t.codec.clone());
    let info = VideoInfo {
        duration_secs,
        width,
        height,
        fps,
        video_codec: video_codec.clone(),
        audio_codec: audio_codec.clone(),
        audio_tracks: audio_tracks.clone(),
        container: container.clone(),
        bit_rate_bps,
    };
    diag(app, format!(
        "probe: {} → {}/{}, {}×{} @ {:.2}fps, {} audio track(s), {:.1}s",
        basename(path),
        video_codec,
        audio_codec.as_deref().unwrap_or("none"),
        width, height, fps,
        audio_tracks.len(),
        duration_secs,
    ));
    Ok(info)
}

#[tauri::command]
pub async fn probe_video(app: AppHandle, path: String) -> Result<VideoInfo, String> {
    probe_video_inner(&app, &path).await
}

const WAVEFORM_BINS: usize = 4000;

/// List the timestamps (in seconds) of every video keyframe in the source.
/// Cached per source-fingerprint as a binary f32 blob; second-open is free.
/// Frontend uses these to draw faint tick marks on the timeline so the user
/// can see where stream-copy cuts will actually snap.
#[tauri::command]
pub async fn probe_keyframes(app: AppHandle, path: String) -> Result<Vec<f32>, String> {
    let t0 = std::time::Instant::now();
    let key = proxy_cache_key(&path)?;
    let cache_path = proxy_dir(&app)?.join(format!("{}.kf.f32", &key[..32]));
    if cache_path.exists() {
        if let Ok(bytes) = std::fs::read(&cache_path) {
            if bytes.len() % 4 == 0 {
                let mut out = Vec::with_capacity(bytes.len() / 4);
                for i in (0..bytes.len()).step_by(4) {
                    let arr: [u8; 4] = bytes[i..i + 4]
                        .try_into()
                        .map_err(|_| "bad cache slice".to_string())?;
                    out.push(f32::from_le_bytes(arr));
                }
                diag(
                    &app,
                    format!(
                        "[keyframes] HIT · {} ({} kf, {}ms)",
                        basename(&path),
                        out.len(),
                        t0.elapsed().as_millis()
                    ),
                );
                return Ok(out);
            }
        }
    }

    // ffprobe: walk video packets, keep only those with the keyframe flag
    // (`pict_type=I`). Stream as CSV for compactness.
    let output = app
        .shell()
        .sidecar("ffprobe")
        .map_err(|e| e.to_string())?
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-skip_frame", "nokey",
            "-show_entries", "frame=pts_time",
            "-of", "csv=print_section=0",
            &path,
        ])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe (keyframes) failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut keyframes: Vec<f32> = Vec::new();
    for line in stdout.lines() {
        let s = line.trim();
        if s.is_empty() || s == "N/A" { continue; }
        if let Ok(v) = s.parse::<f32>() {
            keyframes.push(v);
        }
    }

    // Cache as raw little-endian f32s.
    let mut buf = Vec::with_capacity(keyframes.len() * 4);
    for &v in &keyframes {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let _ = std::fs::write(&cache_path, &buf);

    diag(
        &app,
        format!(
            "[keyframes] MISS · {} ({} kf, {}ms)",
            basename(&path),
            keyframes.len(),
            t0.elapsed().as_millis()
        ),
    );
    Ok(keyframes)
}

/// Extract a peak-amplitude waveform from one audio track. Returns a vector
/// of WAVEFORM_BINS f32 values in [0, 1] where each bin is the max sample
/// magnitude over its slice of the timeline. Cached per (source, track) on
/// disk so reopening the file is instant.
#[tauri::command]
pub async fn extract_waveform(
    app: AppHandle,
    path: String,
    info: VideoInfo,
    track_index: Option<u32>,
) -> Result<Vec<f32>, String> {
    let t0 = std::time::Instant::now();
    let track_idx = track_index.unwrap_or(0);
    let key = proxy_cache_key(&path)?;
    // Track-indexed cache name. Single-track sources end up with .wave-0.f32
    // (was .wave.f32 in v1; old caches will simply re-extract once).
    let cache_path = proxy_dir(&app)?.join(format!("{}.wave-{}.f32", &key[..32], track_idx));
    if cache_path.exists() {
        if let Ok(bytes) = std::fs::read(&cache_path) {
            if bytes.len() == WAVEFORM_BINS * 4 {
                let mut bins = Vec::with_capacity(WAVEFORM_BINS);
                for i in 0..WAVEFORM_BINS {
                    let arr: [u8; 4] = bytes[i * 4..i * 4 + 4]
                        .try_into()
                        .map_err(|_| "bad cache slice".to_string())?;
                    bins.push(f32::from_le_bytes(arr));
                }
                diag(
                    &app,
                    format!(
                        "[waveform] HIT · {} track={} ({}ms)",
                        basename(&path),
                        track_idx,
                        t0.elapsed().as_millis()
                    ),
                );
                return Ok(bins);
            }
        }
    }

    if info.audio_tracks.is_empty()
        || (track_idx as usize) >= info.audio_tracks.len()
        || info.duration_secs <= 0.0
    {
        return Ok(vec![0.0; WAVEFORM_BINS]);
    }

    // Stream raw mono 8kHz s16le PCM from ffmpeg's stdout for the target track.
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-i", &path,
            "-map", &format!("0:a:{}?", track_idx),
            "-vn",
            "-ac", "1",
            "-ar", "8000",
            "-f", "s16le",
            "-",
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    // Stream-compute peaks per bin without buffering all PCM. A 60-min source
    // at 8 kHz mono s16 would otherwise hold ~57 MB in RAM.
    let total_expected_samples = (info.duration_secs * 8000.0).max(1.0);
    let mut bins = vec![0.0f32; WAVEFORM_BINS];
    let mut leftover: Option<u8> = None;
    let mut samples_seen: u64 = 0;
    let mut current_bin: usize = 0;
    let mut current_max: f32 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(bytes) => {
                let len = bytes.len();
                let mut idx = 0;
                while idx < len {
                    let (lo, hi) = if let Some(prev) = leftover.take() {
                        let h = bytes[idx];
                        idx += 1;
                        (prev, h)
                    } else if idx + 1 >= len {
                        leftover = Some(bytes[idx]);
                        break;
                    } else {
                        let l = bytes[idx];
                        let h = bytes[idx + 1];
                        idx += 2;
                        (l, h)
                    };
                    let sample = i16::from_le_bytes([lo, hi]);
                    let amp = (sample.unsigned_abs() as f32) / 32768.0;
                    let bin_idx = (((samples_seen as f64) * (WAVEFORM_BINS as f64))
                        / total_expected_samples)
                        .floor() as usize;
                    let bin_idx = bin_idx.min(WAVEFORM_BINS - 1);
                    if bin_idx != current_bin {
                        if current_max > bins[current_bin] {
                            bins[current_bin] = current_max;
                        }
                        current_bin = bin_idx;
                        current_max = 0.0;
                    }
                    if amp > current_max {
                        current_max = amp;
                    }
                    samples_seen += 1;
                }
            }
            CommandEvent::Stderr(bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&bytes));
            }
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "waveform extract failed (code {:?}): {}",
                        payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    // Flush the final in-progress bin.
    if current_max > bins[current_bin] {
        bins[current_bin] = current_max;
    }

    // Cache
    let mut buf = Vec::with_capacity(WAVEFORM_BINS * 4);
    for &v in &bins {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let _ = std::fs::write(&cache_path, &buf);

    Ok(bins)
}
