use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

use crate::diag::diag;
use crate::encoder_cascade::{
    encoder_args_high_quality, encoder_args_sized, encoder_chain, target_video_bitrate_bps,
    SIZED_AUDIO_BPS, WORKING_ENCODER,
};
use crate::helpers::{basename, escape_concat_path, trunc};

/// Source-pixel crop rectangle. Frontend supplies these in source coordinates;
/// backend just feeds them to the ffmpeg `crop` filter.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Crop {
    fn to_filter(self) -> String {
        format!("crop={}:{}:{}:{}", self.w, self.h, self.x, self.y)
    }
}

/// Per-region export descriptor for the concat commands. Each region carries
/// its own optional crop + speed + audio mix, applied in stage 1 (cut).
/// Normalize is a per-export setting handled at the concat level.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RegionExport {
    in_secs: f64,
    out_secs: f64,
    crop: Option<Crop>,
    speed: Option<f64>,
    /// Per-region audio mix override (track gains). When None, the function-
    /// level track_mix is used as the fallback for this region.
    #[serde(default)]
    mix: Option<Vec<TrackGain>>,
}

impl RegionExport {
    /// Effective duration after speed change. Used for progress + size math.
    fn effective_duration(&self) -> f64 {
        let raw = (self.out_secs - self.in_secs).max(0.0);
        let s = self.speed.unwrap_or(1.0);
        if s <= 0.0 {
            raw
        } else {
            raw / s
        }
    }
}

/// `atempo` only supports 0.5..2.0 per filter instance. Chain multiple to
/// reach the requested speed (0.25 → 0.5,0.5; 4.0 → 2.0,2.0).
fn atempo_chain(speed: f64) -> String {
    if (speed - 1.0).abs() < 1e-6 {
        return "atempo=1.0".to_string();
    }
    let mut s = speed.max(0.0001);
    let mut parts: Vec<String> = Vec::new();
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s *= 2.0;
    }
    while s > 2.0 {
        parts.push("atempo=2.0".to_string());
        s /= 2.0;
    }
    parts.push(format!("atempo={:.4}", s));
    parts.join(",")
}

/// loudnorm target chosen to match conversational/streaming loudness (≈ -16
/// LUFS, -1.5 dBTP). Single-pass; not as precise as 2-pass but plenty for
/// "make it actually audible on Discord".
const LOUDNORM_FILTER: &str = "loudnorm=I=-16:TP=-1.5:LRA=11";

/// Per-track gain in the user's audio mix. `volume` is a linear multiplier:
/// 0.0 = muted, 1.0 = source level, 2.0 = +6 dB. Exports drop tracks whose
/// effective volume rounds to zero, so muting a track is free.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct TrackGain {
    index: u32,
    volume: f64,
}

// (build_track_mix_filter removed — its logic now lives inline inside
// build_audio_filter_complex below for cleaner composition with the
// post-mix audio filter chain.)

/// Build the `-vf` chain (crop + speed). Returns "" when there's nothing to do.
fn build_video_filter(crop: Option<Crop>, speed: Option<f64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = crop {
        parts.push(c.to_filter());
    }
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 && s > 0.0 {
            parts.push(format!("setpts={:.6}*PTS", 1.0 / s));
        }
    }
    parts.join(",")
}

/// Build the audio post-mix filter chain (speed atempo + normalize loudnorm).
/// Applied AFTER track mixing, so the user's volume sliders drive the input
/// to loudnorm rather than fighting it.
fn build_audio_post_mix_filters(speed: Option<f64>, normalize: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 && s > 0.0 {
            parts.push(atempo_chain(s));
        }
    }
    if normalize {
        parts.push(LOUDNORM_FILTER.to_string());
    }
    parts.join(",")
}

/// Compose the audio half of `-filter_complex` from a track mix + post-mix
/// filters. Returns `(Some(graph), "[aout]")` when audio needs processing or
/// `(None, "0:a:0?")` when caller can use the source's first audio track
/// straight through (the existing fast-path).
///
/// `total_tracks` validates indices coming from the frontend.
fn build_audio_filter_complex(
    track_mix: &[TrackGain],
    total_tracks: usize,
    post_mix_filters: &str,
) -> (Option<String>, String) {
    let is_default_mix = track_mix.is_empty()
        || (track_mix.len() == total_tracks
            && track_mix.iter().all(|t| (t.volume - 1.0).abs() < 1e-6));

    // Fast path only for single-stream sources with nothing to process.
    // Multi-track sources must still amix all streams even at default gain —
    // returning "0:a:0?" would silently drop every track past the first.
    if is_default_mix && post_mix_filters.is_empty() && total_tracks <= 1 {
        return (None, "0:a:0?".to_string());
    }

    // Build the active track list. For a default mix with multiple source
    // tracks, all are active at unity; the explicit mix controls the rest.
    let active: Vec<(usize, f64)> = if is_default_mix {
        (0..total_tracks).map(|i| (i, 1.0)).collect()
    } else {
        track_mix
            .iter()
            .filter(|t| t.volume > 0.001 && (t.index as usize) < total_tracks)
            .map(|t| (t.index as usize, t.volume))
            .collect()
    };

    let mix_output = if post_mix_filters.is_empty() {
        "[aout]"
    } else {
        "[m]"
    };
    let mut parts: Vec<String> = Vec::new();

    let mix_tag: String = if active.is_empty() {
        // All tracks muted — synthesize silence so the muxer still has audio.
        parts.push(format!(
            "anullsrc=channel_layout=stereo:sample_rate=48000{}",
            mix_output
        ));
        mix_output.to_string()
    } else if active.len() == 1 {
        let (idx, vol) = active[0];
        if (vol - 1.0).abs() < 1e-6 {
            // Unity gain, single active stream — route directly into post-mix.
            // When there's ALSO no post-mix work to do, short-circuit to a
            // direct stream map: no filter graph at all. Previously we
            // returned `Some("")` + `"[aout]"` here, which ffmpeg correctly
            // rejected with "No filters specified in the graph description"
            // (the [aout] label was never defined). Reproduced when a user
            // muted all but one track or had a single source stream + non-
            // default mix metadata.
            if post_mix_filters.is_empty() {
                return (None, format!("0:a:{}?", idx));
            }
            format!("[0:a:{}]", idx)
        } else {
            parts.push(format!("[0:a:{}]volume={:.4}{}", idx, vol, mix_output));
            mix_output.to_string()
        }
    } else {
        let mut tags = Vec::new();
        for (idx, vol) in &active {
            let tag = format!("a{}", idx);
            parts.push(format!("[0:a:{}]volume={:.4}[{}]", idx, vol, tag));
            tags.push(format!("[{}]", tag));
        }
        parts.push(format!(
            "{}amix=inputs={}:duration=longest:dropout_transition=0:normalize=0{}",
            tags.join(""),
            active.len(),
            mix_output
        ));
        mix_output.to_string()
    };

    if !post_mix_filters.is_empty() {
        parts.push(format!("{}{}[aout]", mix_tag, post_mix_filters));
    }

    (Some(parts.join(";")), "[aout]".to_string())
}

/// Returns true if any filter in the export forces a video re-encode. Crop
/// and speed change pixels/timestamps; normalize alone only touches audio.
fn forces_video_reencode(crop: Option<Crop>, speed: Option<f64>) -> bool {
    if crop.is_some() {
        return true;
    }
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 {
            return true;
        }
    }
    false
}

#[derive(Serialize, Clone, Debug)]
struct ExportProgress {
    progress: f64,
    elapsed_secs: f64,
}

/// Thin wrapper over [`crate::ffmpeg::run_ffmpeg`] that emits `ExportProgress`
/// on `event_name`. Kept so the many export call sites don't each repeat the
/// emit closure; the spawn/progress/stderr loop (and the full-stderr failure
/// log, finding F2) live in the shared runner.
async fn run_ffmpeg_with_progress(
    app: &AppHandle,
    args: Vec<String>,
    duration_secs: f64,
    event_name: &str,
) -> Result<(), String> {
    let app_emit = app.clone();
    let event_name = event_name.to_string();
    crate::ffmpeg::run_ffmpeg(
        app,
        "export",
        args,
        duration_secs,
        move |progress, elapsed| {
            let _ = app_emit.emit(
                &event_name,
                ExportProgress {
                    progress,
                    elapsed_secs: elapsed,
                },
            );
        },
    )
    .await
}

/// Re-encode a single region from src_path to fit within target_size_mb.
/// Cascades through the available hardware encoders and falls back to libx264.
#[tauri::command]
pub async fn export_clip_sized(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    target_size_mb: f64,
    crop: Option<Crop>,
    speed: Option<f64>,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip_sized invoked · src={} in={:.3} out={:.3} dur={:.3} \
             target_mb={} crop={:?} speed={:?} normalize={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            target_size_mb,
            crop.is_some(),
            speed,
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if duration < 0.05 {
        diag(
            &app,
            "[export] export_clip_sized REJECTED · selection too short",
        );
        return Err("selection too short".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    // Effective output duration drives the bitrate calc — a 4× speed clip is a
    // quarter as long, so the same byte budget gives 4× the per-second bitrate.
    let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
    let video_bps = target_video_bitrate_bps(target_size_mb, effective_dur);
    let video_kbps = video_bps / 1000;
    let vf = build_video_filter(crop, speed);
    let post_filters = build_audio_post_mix_filters(speed, normalize);
    let (audio_fc, audio_map) = build_audio_filter_complex(&mix, total_tracks, &post_filters);

    let chain = encoder_chain(&app).await;
    let mut last_err = String::from("no encoders available");
    for enc in chain.iter() {
        let mut args: Vec<String> = vec![
            "-y".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-progress".into(),
            "pipe:1".into(),
            "-nostats".into(),
            "-ss".into(),
            format!("{:.6}", in_secs),
            "-to".into(),
            format!("{:.6}", out_secs),
            "-i".into(),
            src_path.clone(),
            "-map".into(),
            "0:v:0?".into(),
            "-map".into(),
            audio_map.clone(),
        ];
        if !vf.is_empty() {
            args.push("-vf".into());
            args.push(vf.clone());
        }
        if let Some(fc) = &audio_fc {
            args.push("-filter_complex".into());
            args.push(fc.clone());
        }
        args.extend(encoder_args_sized(enc, video_kbps));
        args.extend([
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            format!("{}k", SIZED_AUDIO_BPS / 1000),
            "-movflags".into(),
            "+faststart".into(),
            "-map_chapters".into(),
            "-1".into(),
            output_path.clone(),
        ]);
        match run_ffmpeg_with_progress(&app, args, effective_dur, "export:progress").await {
            Ok(()) => {
                *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                return Ok(());
            }
            Err(e) => {
                eprintln!("[clippy] sized export {} failed: {}", enc, e);
                let _ = std::fs::remove_file(&output_path);
                last_err = e;
            }
        }
    }
    Err(last_err)
}

/// Re-encode a stitched concat of N regions from src_path to fit within
/// target_size_mb. Same encoder cascade as export_clip_sized.
#[tauri::command]
pub async fn export_concat_sized(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    target_size_mb: f64,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    diag(
        &app,
        format!(
            "[export] export_concat_sized invoked · src={} regions={} target_mb={} \
             normalize={:?} tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            target_size_mb,
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat_sized REJECTED · no regions");
        return Err("no regions to concat".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    // Sized export is single-pass concat-demuxer + encoder, so per-region
    // filters aren't supported. Frontend should gate this, but verify.
    let first = regions[0].clone();
    for (i, r) in regions.iter().enumerate().skip(1) {
        if r.crop != first.crop || r.speed != first.speed || r.mix != first.mix {
            diag(
                &app,
                format!(
                    "[export] export_concat_sized REJECTED · region {} differs in crop/speed/mix from region 1",
                    i + 1
                ),
            );
            return Err(format!(
                "size-targeted stitched export needs uniform crop, speed, and audio mix across regions (region {} differs)",
                i + 1
            ));
        }
    }
    let total_duration: f64 = regions.iter().map(|r| r.effective_duration()).sum();
    if total_duration < 0.05 {
        return Err("total duration too short".into());
    }
    let video_bps = target_video_bitrate_bps(target_size_mb, total_duration);
    let video_kbps = video_bps / 1000;
    let vf = build_video_filter(first.crop, first.speed);
    let post_filters = build_audio_post_mix_filters(first.speed, normalize);
    // Per-region mix wins; falls back to function-level mix.
    let active_mix: &[TrackGain] = first.mix.as_deref().unwrap_or(&mix);
    let (audio_fc, audio_map) = build_audio_filter_complex(active_mix, total_tracks, &post_filters);

    // Write a concat list (forward slashes + escaped quotes for ffmpeg).
    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let list_file = temp_dir.join(format!("concat-sized-{}.txt", stamp));
    let escaped = escape_concat_path(&src_path)?;
    let mut content = String::new();
    for r in &regions {
        content.push_str(&format!("file '{}'\n", escaped));
        content.push_str(&format!("inpoint {:.6}\n", r.in_secs));
        content.push_str(&format!("outpoint {:.6}\n", r.out_secs));
    }
    std::fs::write(&list_file, &content).map_err(|e| e.to_string())?;
    let list_str = list_file.to_string_lossy().to_string();

    let chain = encoder_chain(&app).await;
    let mut last_err = String::from("no encoders available");
    for enc in chain.iter() {
        let mut args: Vec<String> = vec![
            "-y".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-progress".into(),
            "pipe:1".into(),
            "-nostats".into(),
            "-f".into(),
            "concat".into(),
            "-safe".into(),
            "0".into(),
            "-i".into(),
            list_str.clone(),
            "-map".into(),
            "0:v:0?".into(),
            "-map".into(),
            audio_map.clone(),
            "-fflags".into(),
            "+genpts".into(),
        ];
        if !vf.is_empty() {
            args.push("-vf".into());
            args.push(vf.clone());
        }
        if let Some(fc) = &audio_fc {
            args.push("-filter_complex".into());
            args.push(fc.clone());
        }
        args.extend(encoder_args_sized(enc, video_kbps));
        args.extend([
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            format!("{}k", SIZED_AUDIO_BPS / 1000),
            "-movflags".into(),
            "+faststart".into(),
            "-map_chapters".into(),
            "-1".into(),
            output_path.clone(),
        ]);
        match run_ffmpeg_with_progress(&app, args, total_duration, "export:progress").await {
            Ok(()) => {
                *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                let _ = std::fs::remove_file(&list_file);
                return Ok(());
            }
            Err(e) => {
                eprintln!("[clippy] sized concat export {} failed: {}", enc, e);
                let _ = std::fs::remove_file(&output_path);
                last_err = e;
            }
        }
    }
    let _ = std::fs::remove_file(&list_file);
    Err(last_err)
}

/// Cut a single region (with its own crop + speed + the export-wide track mix)
/// into `out_path`. Three paths from fastest to slowest:
///   * pure stream-copy (no filters at all)
///   * audio-only re-encode (track mix only, video stream-copies)
///   * full re-encode (crop/speed + maybe audio mix)
///
/// Audio normalize is per-export and applied at the concat stage, never here.
async fn cut_segment(
    app: &AppHandle,
    src_path: &str,
    region: RegionExport,
    out_path: &str,
    track_mix: &[TrackGain],
    total_audio_tracks: usize,
) -> Result<(), String> {
    let post_filters = build_audio_post_mix_filters(region.speed, false);
    // Per-region mix override wins; fall back to the export-wide mix.
    let active_mix: &[TrackGain] = region.mix.as_deref().unwrap_or(track_mix);
    let (audio_fc, audio_map) =
        build_audio_filter_complex(active_mix, total_audio_tracks, &post_filters);
    let needs_video_reencode = forces_video_reencode(region.crop, region.speed);
    let needs_audio_reencode = audio_fc.is_some();

    if needs_video_reencode {
        let vf = build_video_filter(region.crop, region.speed);
        if let Some(ref fc) = audio_fc {
            diag(
                app,
                format!(
                    "export: full re-encode — vf=[{}] fc=[{}]",
                    trunc(&vf, 80),
                    trunc(fc, 120)
                ),
            );
        } else {
            diag(
                app,
                format!("export: full re-encode — vf=[{}]", trunc(&vf, 80)),
            );
        }
        let chain = encoder_chain(app).await;
        let mut last_err = String::from("no encoders available");
        for enc in chain.iter() {
            let mut args: Vec<String> = vec![
                "-y".into(),
                "-hide_banner".into(),
                "-loglevel".into(),
                "error".into(),
                "-ss".into(),
                format!("{:.6}", region.in_secs),
                "-to".into(),
                format!("{:.6}", region.out_secs),
                "-i".into(),
                src_path.into(),
                "-map".into(),
                "0:v:0?".into(),
                "-map".into(),
                audio_map.clone(),
            ];
            if !vf.is_empty() {
                args.push("-vf".into());
                args.push(vf.clone());
            }
            if let Some(fc) = &audio_fc {
                args.push("-filter_complex".into());
                args.push(fc.clone());
            }
            args.extend(encoder_args_high_quality(enc).into_iter().map(String::from));
            args.extend([
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                "160k".into(),
                "-map_chapters".into(),
                "-1".into(),
                out_path.into(),
            ]);
            let t0 = std::time::Instant::now();
            match crate::ffmpeg::run_ffmpeg(app, "cut", args, 0.0, |_, _| {}).await {
                Ok(()) => {
                    diag(
                        app,
                        format!(
                            "export: cut OK via {} in {:.1}s",
                            enc,
                            t0.elapsed().as_secs_f64()
                        ),
                    );
                    *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("[clippy] crop/speed cut {} failed: {}", enc, e);
                    let _ = std::fs::remove_file(out_path);
                    last_err = e;
                }
            }
        }
        return Err(last_err);
    }

    if needs_audio_reencode {
        // Track mix changed but no crop/speed — video stream-copies, audio
        // gets the filter_complex treatment. Same speed as a normalize-only export.
        if let Some(ref fc) = audio_fc {
            diag(
                app,
                format!("export: audio re-encode — fc=[{}]", trunc(fc, 160)),
            );
        }
        let mut args: Vec<String> = vec![
            "-y".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-ss".into(),
            format!("{:.6}", region.in_secs),
            "-to".into(),
            format!("{:.6}", region.out_secs),
            "-i".into(),
            src_path.into(),
            "-map".into(),
            "0:v:0?".into(),
            "-map".into(),
            audio_map.clone(),
            "-c:v".into(),
            "copy".into(),
        ];
        if let Some(fc) = &audio_fc {
            args.push("-filter_complex".into());
            args.push(fc.clone());
        }
        args.extend([
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "160k".into(),
            "-avoid_negative_ts".into(),
            "make_zero".into(),
            "-map_chapters".into(),
            "-1".into(),
            out_path.into(),
        ]);
        let t0 = std::time::Instant::now();
        crate::ffmpeg::run_ffmpeg(app, "cut", args, 0.0, |_, _| {}).await?;
        diag(
            app,
            format!(
                "export: cut audio re-encode OK in {:.1}s",
                t0.elapsed().as_secs_f64()
            ),
        );
        return Ok(());
    }

    diag(app, "export: stream-copy (no crop/speed/mix change)");

    let args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{:.6}", region.in_secs),
        "-to".into(),
        format!("{:.6}", region.out_secs),
        "-i".into(),
        src_path.into(),
        "-map".into(),
        "0:v:0?".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c".into(),
        "copy".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-map_chapters".into(),
        "-1".into(),
        out_path.into(),
    ];
    crate::ffmpeg::run_ffmpeg(app, "cut", args, 0.0, |_, _| {}).await
}

/// Concatenate N regions from the same source into a single output file.
///
/// Two-stage stream-copy: cut each region into a clean intermediate MP4 first
/// (keyframe-aligned input seek, self-contained file), then concat the
/// intermediates with `-c copy`. Earlier versions fed `inpoint`/`outpoint` of
/// the same source straight into the concat demuxer, which left mid-GOP frags
/// at boundaries and caused frozen video while audio continued. Boundaries
/// still snap to source keyframes (~1 s with OBS keyframe=1). No re-encode.
#[tauri::command]
pub async fn export_concat(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    diag(
        &app,
        format!(
            "[export] export_concat invoked · src={} regions={} normalize={:?} \
             tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat REJECTED · no regions");
        return Err("no regions to concat".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let mix_active = !(mix.is_empty()
        || mix.len() == total_tracks && mix.iter().all(|t| (t.volume - 1.0).abs() < 1e-6));
    // Sum of post-speed durations — what the final output will actually be.
    let total_duration: f64 = regions.iter().map(|r| r.effective_duration()).sum();
    if total_duration < 0.05 {
        diag(
            &app,
            "[export] export_concat REJECTED · total duration too short",
        );
        return Err("total duration is too short to export".into());
    }
    // Stage 1 dominates wall-clock when any region needs re-encode (crop/speed).
    let any_reencode = regions
        .iter()
        .any(|r| forces_video_reencode(r.crop, r.speed));

    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let cleanup = |segs: &[PathBuf], list: Option<&PathBuf>| {
        for s in segs {
            let _ = std::fs::remove_file(s);
        }
        if let Some(l) = list {
            let _ = std::fs::remove_file(l);
        }
    };

    // Stage 1: cut each region into its own clean MP4 intermediate. Per-region
    // crop + speed are applied here; segments may have different dimensions if
    // crops differ — the frontend gates that case for stitched mode.
    let start = std::time::Instant::now();
    let mut temp_segments: Vec<PathBuf> = Vec::with_capacity(regions.len());
    let mut produced_secs: f64 = 0.0;
    for (idx, region) in regions.iter().enumerate() {
        let seg_path = temp_dir.join(format!("seg-{}-{}.mp4", stamp, idx));
        let seg_str = seg_path.to_string_lossy().to_string();
        if let Err(e) = cut_segment(
            &app,
            &src_path,
            region.clone(),
            &seg_str,
            &mix,
            total_tracks,
        )
        .await
        {
            cleanup(&temp_segments, None);
            return Err(format!("region {} cut failed: {}", idx + 1, e));
        }
        temp_segments.push(seg_path);
        produced_secs += region.effective_duration();
        // With re-encodes (video filter or audio mix), stage 1 dominates.
        let stage1_share = if any_reencode || mix_active {
            0.85
        } else {
            0.4
        };
        let progress = (produced_secs / total_duration * stage1_share).clamp(0.0, stage1_share);
        let _ = app.emit(
            "export:progress",
            ExportProgress {
                progress,
                elapsed_secs: start.elapsed().as_secs_f64(),
            },
        );
    }

    // Stage 2: concat the intermediates with stream copy. They're all from the
    // same source, so codec/timescale match — the demuxer just glues them.
    let list_file = temp_dir.join(format!("concat-{}.txt", stamp));
    let mut content = String::new();
    for seg in &temp_segments {
        let escaped = escape_concat_path(&seg.to_string_lossy())?;
        content.push_str(&format!("file '{}'\n", escaped));
    }
    if let Err(e) = std::fs::write(&list_file, &content) {
        cleanup(&temp_segments, None);
        return Err(e.to_string());
    }
    let list_str = list_file.to_string_lossy().to_string();

    // Normalize forces an audio re-encode at concat time (filter incompatible
    // with -c copy on the audio stream). Video can still stream-copy.
    let mut concat_args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_str.clone(),
        "-map".into(),
        "0:v:0?".into(),
        "-map".into(),
        "0:a:0?".into(),
    ];
    if normalize {
        concat_args.extend([
            "-c:v".into(),
            "copy".into(),
            "-af".into(),
            LOUDNORM_FILTER.into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "160k".into(),
        ]);
    } else {
        concat_args.extend(["-c".into(), "copy".into()]);
    }
    concat_args.extend([
        "-movflags".into(),
        "+faststart".into(),
        "-map_chapters".into(),
        "-1".into(),
        output_path.clone(),
    ]);
    let stage1_share = if any_reencode || mix_active {
        0.85
    } else {
        0.4
    };
    let app_emit = app.clone();
    let res = crate::ffmpeg::run_ffmpeg(
        &app,
        "concat",
        concat_args,
        total_duration,
        move |frac, elapsed| {
            let progress = (stage1_share + frac * (1.0 - stage1_share)).min(1.0);
            let _ = app_emit.emit(
                "export:progress",
                ExportProgress {
                    progress,
                    elapsed_secs: elapsed,
                },
            );
        },
    )
    .await;
    cleanup(&temp_segments, Some(&list_file));
    res?;
    diag(
        &app,
        format!(
            "export_concat: done in {:.1}s",
            start.elapsed().as_secs_f64()
        ),
    );
    Ok(())
}

#[tauri::command]
pub async fn export_clip(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    crop: Option<Crop>,
    speed: Option<f64>,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip invoked · src={} in={:.3} out={:.3} dur={:.3} \
             crop={:?} speed={:?} normalize={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            crop.is_some(),
            speed,
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if duration < 0.05 {
        diag(&app, "[export] export_clip REJECTED · selection too short");
        return Err("selection too short".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let post_filters = build_audio_post_mix_filters(speed, normalize);
    let (audio_fc, audio_map) = build_audio_filter_complex(&mix, total_tracks, &post_filters);
    let needs_audio_reencode = audio_fc.is_some();

    // Four paths, fastest to slowest:
    //   * pure stream-copy (no mix, no normalize, no crop/speed)
    //   * audio re-encode only (track mix or normalize but no video filter)
    //   * full re-encode (crop/speed + maybe audio mix/normalize)
    if forces_video_reencode(crop, speed) {
        let vf = build_video_filter(crop, speed);
        let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
        let chain = encoder_chain(&app).await;
        let mut last_err = String::from("no encoders available");
        for enc in chain.iter() {
            let mut args: Vec<String> = vec![
                "-y".into(),
                "-hide_banner".into(),
                "-loglevel".into(),
                "error".into(),
                "-progress".into(),
                "pipe:1".into(),
                "-nostats".into(),
                "-ss".into(),
                format!("{:.6}", in_secs),
                "-to".into(),
                format!("{:.6}", out_secs),
                "-i".into(),
                src_path.clone(),
                "-map".into(),
                "0:v:0?".into(),
                "-map".into(),
                audio_map.clone(),
            ];
            if !vf.is_empty() {
                args.push("-vf".into());
                args.push(vf.clone());
            }
            if let Some(fc) = &audio_fc {
                args.push("-filter_complex".into());
                args.push(fc.clone());
            }
            args.extend(encoder_args_high_quality(enc).into_iter().map(String::from));
            args.extend([
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                "160k".into(),
                "-movflags".into(),
                "+faststart".into(),
                "-map_chapters".into(),
                "-1".into(),
                output_path.clone(),
            ]);
            match run_ffmpeg_with_progress(&app, args, effective_dur, "export:progress").await {
                Ok(()) => {
                    *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("[clippy] crop/speed export {} failed: {}", enc, e);
                    let _ = std::fs::remove_file(&output_path);
                    last_err = e;
                }
            }
        }
        return Err(last_err);
    }

    if needs_audio_reencode {
        // Video stream-copy, audio runs through filter_complex (track mix +
        // optional normalize). Cheap — only the audio track is touched.
        let mut args: Vec<String> = vec![
            "-y".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-progress".into(),
            "pipe:1".into(),
            "-nostats".into(),
            "-ss".into(),
            format!("{:.6}", in_secs),
            "-to".into(),
            format!("{:.6}", out_secs),
            "-i".into(),
            src_path.clone(),
            "-map".into(),
            "0:v:0?".into(),
            "-map".into(),
            audio_map.clone(),
            "-c:v".into(),
            "copy".into(),
        ];
        if let Some(fc) = &audio_fc {
            args.push("-filter_complex".into());
            args.push(fc.clone());
        }
        args.extend([
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "160k".into(),
            "-avoid_negative_ts".into(),
            "make_zero".into(),
            "-movflags".into(),
            "+faststart".into(),
            "-map_chapters".into(),
            "-1".into(),
            output_path.clone(),
        ]);
        return run_ffmpeg_with_progress(&app, args, duration, "export:progress").await;
    }

    let args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-ss".into(),
        format!("{:.6}", in_secs),
        "-to".into(),
        format!("{:.6}", out_secs),
        "-i".into(),
        src_path,
        "-c".into(),
        "copy".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-map".into(),
        "0".into(),
        "-map_chapters".into(),
        "-1".into(),
        output_path,
    ];
    run_ffmpeg_with_progress(&app, args, duration, "export:progress").await
}

/// MP3 audio bitrate used for all audio-only exports. 192k is the conventional
/// "small but sounds fine for music + voice" sweet spot.
const MP3_BITRATE: &str = "192k";

/// Export the audio of a single [in,out] slice as MP3. Always re-encodes via
/// libmp3lame (source audio is almost always AAC, not MP3). Crops are ignored
/// — there's no video stream in the output. Speed + normalize are honored
/// because both are audio-only filters (atempo + loudnorm).
#[tauri::command]
pub async fn export_clip_audio(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    speed: Option<f64>,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip_audio invoked · src={} in={:.3} out={:.3} dur={:.3} \
             speed={:?} normalize={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            speed,
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if duration < 0.05 {
        diag(
            &app,
            "[export] export_clip_audio REJECTED · selection too short",
        );
        return Err("selection too short".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
    let post_filters = build_audio_post_mix_filters(speed, normalize);
    let (audio_fc, audio_map) = build_audio_filter_complex(&mix, total_tracks, &post_filters);

    let mut args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-ss".into(),
        format!("{:.6}", in_secs),
        "-to".into(),
        format!("{:.6}", out_secs),
        "-i".into(),
        src_path,
        "-vn".into(),
        "-map".into(),
        audio_map,
    ];
    if let Some(fc) = audio_fc {
        args.push("-filter_complex".into());
        args.push(fc);
    }
    args.extend([
        "-c:a".into(),
        "libmp3lame".into(),
        "-b:a".into(),
        MP3_BITRATE.into(),
        "-map_chapters".into(),
        "-1".into(),
        output_path,
    ]);
    run_ffmpeg_with_progress(&app, args, effective_dur, "export:progress").await
}

/// Export N regions concatenated as a single MP3. Single-pass: ffmpeg's concat
/// demuxer feeds the regions straight into libmp3lame. Speed/normalize apply
/// uniformly across all regions (single-pass means we can't do per-region
/// filters). Frontend should gate mixed-speed cases.
#[tauri::command]
pub async fn export_concat_audio(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    normalize: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    diag(
        &app,
        format!(
            "[export] export_concat_audio invoked · src={} regions={} normalize={:?} \
             tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            normalize,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat_audio REJECTED · no regions");
        return Err("no regions to concat".into());
    }
    let normalize = normalize.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let first = regions[0].clone();
    for (i, r) in regions.iter().enumerate().skip(1) {
        if r.speed != first.speed || r.mix != first.mix {
            diag(
                &app,
                format!(
                    "[export] export_concat_audio REJECTED · region {} differs in speed/mix from region 1",
                    i + 1
                ),
            );
            return Err(format!(
                "stitched MP3 export needs uniform speed and audio mix across regions (region {} differs)",
                i + 1
            ));
        }
    }
    let total_duration: f64 = regions.iter().map(|r| r.effective_duration()).sum();
    if total_duration < 0.05 {
        diag(
            &app,
            "[export] export_concat_audio REJECTED · total duration too short",
        );
        return Err("total duration too short".into());
    }
    let post_filters = build_audio_post_mix_filters(first.speed, normalize);
    let active_mix: &[TrackGain] = first.mix.as_deref().unwrap_or(&mix);
    let (audio_fc, audio_map) = build_audio_filter_complex(active_mix, total_tracks, &post_filters);

    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let list_file = temp_dir.join(format!("concat-mp3-{}.txt", stamp));
    let escaped = escape_concat_path(&src_path)?;
    let mut content = String::new();
    for r in &regions {
        content.push_str(&format!("file '{}'\n", escaped));
        content.push_str(&format!("inpoint {:.6}\n", r.in_secs));
        content.push_str(&format!("outpoint {:.6}\n", r.out_secs));
    }
    std::fs::write(&list_file, &content).map_err(|e| e.to_string())?;
    let list_str = list_file.to_string_lossy().to_string();

    let mut args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_str,
        "-vn".into(),
        "-map".into(),
        audio_map,
    ];
    if let Some(fc) = audio_fc {
        args.push("-filter_complex".into());
        args.push(fc);
    }
    args.extend([
        "-c:a".into(),
        "libmp3lame".into(),
        "-b:a".into(),
        MP3_BITRATE.into(),
        "-map_chapters".into(),
        "-1".into(),
        output_path,
    ]);
    let result = run_ffmpeg_with_progress(&app, args, total_duration, "export:progress").await;
    let _ = std::fs::remove_file(&list_file);
    result
}

// ---- GIF + PNG export ----

/// GIF target framerate. 15 fps is the conventional reaction-clip cadence —
/// smooth enough to look intentional, small enough to keep file sizes sane.
const GIF_FPS: u32 = 15;
/// Default GIF width if the caller doesn't specify (≈ 720p widescreen height).
const GIF_DEFAULT_WIDTH: u32 = 1280;

/// Build the `-vf` filter for a GIF export. Combines optional crop + speed +
/// the standard fps/scale/palettegen pipeline. The scale filter caps to
/// `min(target, iw)` so requesting Source / over-resolution never upscales.
fn gif_filter_chain(crop: Option<Crop>, speed: Option<f64>, target_width: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = crop {
        parts.push(c.to_filter());
    }
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 && s > 0.0 {
            parts.push(format!("setpts={:.6}*PTS", 1.0 / s));
        }
    }
    parts.push(format!("fps={}", GIF_FPS));
    parts.push(format!("scale='min({},iw)':-2:flags=lanczos", target_width));
    let pre = parts.join(",");
    format!("{},split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5", pre)
}

/// Single-region GIF export. Crop + speed honored; no audio. `target_width`
/// caps the long edge — `min(target_width, iw)` so over-spec never upscales.
#[tauri::command]
pub async fn export_clip_gif(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    crop: Option<Crop>,
    speed: Option<f64>,
    target_width: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    if duration < 0.05 {
        return Err("selection too short".into());
    }
    let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
    let filter = gif_filter_chain(crop, speed, target_width.unwrap_or(GIF_DEFAULT_WIDTH));
    let args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-ss".into(),
        format!("{:.6}", in_secs),
        "-to".into(),
        format!("{:.6}", out_secs),
        "-i".into(),
        src_path,
        "-an".into(),
        "-filter_complex".into(),
        filter,
        "-loop".into(),
        "0".into(),
        output_path,
    ];
    run_ffmpeg_with_progress(&app, args, effective_dur, "export:progress").await
}

/// Stitched GIF: concat regions then apply the GIF pipeline. Two-stage like
/// the video concat — stage 1 cuts each region (with per-region crop+speed
/// pre-applied), stage 2 concatenates and runs through palettegen.
#[tauri::command]
pub async fn export_concat_gif(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    target_width: Option<u32>,
) -> Result<(), String> {
    if regions.is_empty() {
        return Err("no regions to concat".into());
    }
    let total_duration: f64 = regions.iter().map(|r| r.effective_duration()).sum();
    if total_duration < 0.05 {
        return Err("total duration too short".into());
    }

    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    // Stage 1: cut each region (per-region filters baked in).
    let start = std::time::Instant::now();
    let mut temp_segments: Vec<PathBuf> = Vec::with_capacity(regions.len());
    let mut produced_secs: f64 = 0.0;
    for (idx, region) in regions.iter().enumerate() {
        let seg_path = temp_dir.join(format!("seg-gif-{}-{}.mp4", stamp, idx));
        let seg_str = seg_path.to_string_lossy().to_string();
        // GIF drops audio in stage 2, so the mix is irrelevant here.
        if let Err(e) = cut_segment(&app, &src_path, region.clone(), &seg_str, &[], 0).await {
            for s in &temp_segments {
                let _ = std::fs::remove_file(s);
            }
            return Err(format!("region {} cut failed: {}", idx + 1, e));
        }
        temp_segments.push(seg_path);
        produced_secs += region.effective_duration();
        let progress = (produced_secs / total_duration * 0.7).clamp(0.0, 0.7);
        let _ = app.emit(
            "export:progress",
            ExportProgress {
                progress,
                elapsed_secs: start.elapsed().as_secs_f64(),
            },
        );
    }

    // Stage 2: build a concat list, run through palette pipeline.
    let list_file = temp_dir.join(format!("concat-gif-{}.txt", stamp));
    let mut content = String::new();
    for seg in &temp_segments {
        let escaped = escape_concat_path(&seg.to_string_lossy())?;
        content.push_str(&format!("file '{}'\n", escaped));
    }
    if let Err(e) = std::fs::write(&list_file, &content) {
        for s in &temp_segments {
            let _ = std::fs::remove_file(s);
        }
        return Err(e.to_string());
    }
    let list_str = list_file.to_string_lossy().to_string();
    // Stage 2 filter: only fps + scale + palette (per-region crop/speed already
    // applied in stage 1).
    let target = target_width.unwrap_or(GIF_DEFAULT_WIDTH);
    let filter = format!(
        "fps={},scale='min({},iw)':-2:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5",
        GIF_FPS, target
    );
    let args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_str,
        "-an".into(),
        "-filter_complex".into(),
        filter,
        "-loop".into(),
        "0".into(),
        output_path,
    ];
    let result = run_ffmpeg_with_progress(&app, args, total_duration, "export:progress").await;
    let _ = std::fs::remove_file(&list_file);
    for s in &temp_segments {
        let _ = std::fs::remove_file(s);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- build_audio_filter_complex coverage ----------
    //
    // Lock in every branch — the bug fixed 2026-05-11 was a missing
    // short-circuit for "single active track at unity gain with no
    // post-mix filters" that produced `Some("")` + `[aout]` and ffmpeg
    // rejected the empty graph with code -22. These tests pin the
    // contract per-branch so a future refactor can't silently regress.

    fn gain(index: u32, volume: f64) -> TrackGain {
        TrackGain { index, volume }
    }

    #[test]
    fn fc_single_source_default_mix_takes_fast_path() {
        // Default mix, single source, no post-filters → direct map, no graph.
        let (fc, map) = build_audio_filter_complex(&[], 1, "");
        assert!(
            fc.is_none(),
            "single-track default mix should not build a graph"
        );
        assert_eq!(map, "0:a:0?");
    }

    #[test]
    fn fc_multi_source_default_mix_builds_amix() {
        // 3 tracks, no explicit mix entries → still amix all of them at unity.
        let (fc, map) = build_audio_filter_complex(&[], 3, "");
        let fc = fc.expect("multi-track default mix needs an amix graph");
        assert!(!fc.is_empty(), "graph must not be empty");
        assert!(fc.contains("amix=inputs=3"), "graph: {fc}");
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_single_active_unity_no_post_filters_short_circuits() {
        // The regression: user has 1 track at unity + 0 post filters → must
        // return None + direct stream map. Previously returned Some("") +
        // "[aout]" which crashed ffmpeg with code -22.
        let mix = vec![gain(0, 1.0)];
        let (fc, map) = build_audio_filter_complex(&mix, 1, "");
        assert!(
            fc.is_none(),
            "single unity-gain active track + no post-filters must NOT build a graph (got {fc:?})"
        );
        assert_eq!(map, "0:a:0?");
    }

    #[test]
    fn fc_single_active_unity_with_post_filters_builds_graph() {
        // Same active set but WITH post-mix filters (e.g. normalize) → we
        // need a graph that routes the lone stream through them.
        let mix = vec![gain(0, 1.0)];
        let (fc, map) = build_audio_filter_complex(&mix, 1, ",loudnorm");
        let fc = fc.expect("post-filters force a graph");
        assert!(
            fc.contains("[0:a:0]"),
            "graph must reference the active stream: {fc}"
        );
        assert!(
            fc.contains("loudnorm"),
            "graph must include the post-filter: {fc}"
        );
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_all_muted_synthesizes_silence() {
        // Volumes all 0 → no active tracks → anullsrc fallback so the muxer
        // still has an audio stream to attach.
        let mix = vec![gain(0, 0.0), gain(1, 0.0)];
        let (fc, map) = build_audio_filter_complex(&mix, 2, "");
        let fc = fc.expect("muted-all path still needs a graph (anullsrc)");
        assert!(fc.contains("anullsrc"), "graph: {fc}");
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_single_track_non_unity_emits_volume_filter() {
        let mix = vec![gain(0, 0.5)];
        let (fc, map) = build_audio_filter_complex(&mix, 1, "");
        let fc = fc.expect("non-unity volume requires a filter");
        assert!(fc.contains("volume=0.5000"), "graph: {fc}");
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_multi_track_explicit_mix_amixes_active() {
        let mix = vec![gain(0, 1.0), gain(1, 0.5), gain(2, 0.0)];
        let (fc, map) = build_audio_filter_complex(&mix, 3, "");
        let fc = fc.expect("multi-track mix requires a graph");
        // track 2 was muted → only tracks 0 + 1 should be in the amix.
        assert!(
            fc.contains("amix=inputs=2"),
            "expected 2-input amix, graph: {fc}"
        );
        assert_eq!(map, "[aout]");
    }

    #[test]
    fn fc_returned_graph_is_never_empty_when_some() {
        // Property: if the function returns Some(graph), the graph string
        // must be non-empty. (The bug was Some("") sneaking through.)
        let cases: Vec<(Vec<TrackGain>, usize, &str)> = vec![
            (vec![], 1, ""),
            (vec![], 3, ""),
            (vec![gain(0, 1.0)], 1, ""),
            (vec![gain(0, 1.0)], 1, ",loudnorm"),
            (vec![gain(0, 0.5)], 1, ""),
            (vec![gain(0, 0.0)], 1, ""),
            (vec![gain(0, 1.0), gain(1, 1.0)], 2, ""),
            (
                vec![gain(0, 1.0), gain(1, 0.5), gain(2, 0.0)],
                3,
                ",atempo=2.0",
            ),
        ];
        for (mix, total, post) in cases {
            let (fc, _map) = build_audio_filter_complex(&mix, total, post);
            if let Some(g) = fc {
                assert!(
                    !g.is_empty(),
                    "build_audio_filter_complex returned Some(\"\") for mix={mix:?} total={total} post={post:?}"
                );
            }
        }
    }
}
