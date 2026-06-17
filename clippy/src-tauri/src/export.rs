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

/// Per-track gain in the user's audio mix. `volume` is a linear multiplier:
/// 0.0 = muted, 1.0 = source level, 2.0 = +6 dB. Exports drop tracks whose
/// effective volume rounds to zero, so muting a track is free.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct TrackGain {
    index: u32,
    volume: f64,
}

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

/// Build the audio post-mix filter chain (currently only speed atempo).
/// Applied AFTER track mixing, so the user's volume sliders drive the input.
fn build_audio_post_mix_filters(speed: Option<f64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = speed {
        if (s - 1.0).abs() > 1e-6 && s > 0.0 {
            parts.push(atempo_chain(s));
        }
    }
    parts.join(",")
}

/// Result of planning the audio half of an ffmpeg invocation. Carries the
/// filter graph (if any), the `-map` arguments to emit (one per output audio
/// stream), and the codec-selection hints the caller needs to finish the
/// command line.
#[derive(Debug, Clone, PartialEq)]
struct AudioMux {
    /// Filter graph for `-filter_complex`, or `None` for direct stream mapping.
    filter_complex: Option<String>,
    /// `-map` arguments. One per output audio stream. `[aN]` labels reference
    /// `filter_complex` outputs; `0:a:N?` / `0:a?` reference source streams.
    maps: Vec<String>,
    /// `true` → caller emits `-c:a aac -b:a {bitrate}` (and a per-channel-pair
    /// bitrate scale if it wants). `false` → caller emits `-c:a copy`.
    needs_encode: bool,
    /// `true` → caller appends `-ac 2 -ar 48000` to force stereo 48 kHz on the
    /// output. Only set when folding the source down to a single stream;
    /// preserved-multi-track exports leave source layout/rate intact so
    /// downstream NLEs see the original channel mapping.
    downmix_to_stereo: bool,
}

/// Plan the audio side of an export.
///
/// Two modes governed by `preserve_multi_track`:
///
/// - **Downmix (default).** All active source tracks are folded with `amix`
///   into a single AAC stream, then forced to stereo 48 kHz at mux. This is
///   what plays everywhere (Windows Photos, Discord upload, web embeds).
///   When the source already has just one track at unity, the fast path
///   returns a direct `0:a:0?` map with no graph.
///
/// - **Preserve.** Each surviving (non-muted) source track lands as its own
///   AAC output stream so downstream NLEs can re-mix. When the mix is
///   identity (no volume changes, no muted tracks, no post-mix filters), this
///   collapses to a pure `-map 0:a -c:a copy` stream-copy of every source
///   audio stream — zero re-encode, full fidelity, source channel layout
///   preserved. Any non-unity volume or speed change re-encodes per stream
///   with the per-track gain (and atempo) applied in the graph.
///
/// `post_mix_filters` is a comma-joined filter chain (e.g. `"atempo=2.0"`)
/// produced by [`build_audio_post_mix_filters`]; pass `""` when there's
/// nothing to apply after the mix stage.
///
/// `total_tracks` validates indices coming from the frontend.
fn build_audio_filter_complex(
    track_mix: &[TrackGain],
    total_tracks: usize,
    post_mix_filters: &str,
    preserve_multi_track: bool,
) -> AudioMux {
    let is_default_mix = track_mix.is_empty()
        || (track_mix.len() == total_tracks
            && track_mix.iter().all(|t| (t.volume - 1.0).abs() < 1e-6));

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

    if preserve_multi_track {
        // True passthrough: every source track stream-copies, no filter graph.
        // `0:a?` maps every audio stream from input 0 (the `?` makes it
        // tolerant of source files with no audio).
        if is_default_mix && post_mix_filters.is_empty() {
            return AudioMux {
                filter_complex: None,
                maps: vec!["0:a?".into()],
                needs_encode: false,
                downmix_to_stereo: false,
            };
        }
        // Everything muted in preserve mode: still need ONE stream so the
        // muxer is happy. Stereo silence is the smallest legal output.
        if active.is_empty() {
            let post_chain = if post_mix_filters.is_empty() {
                String::new()
            } else {
                format!(",{}", post_mix_filters)
            };
            let graph = format!(
                "anullsrc=channel_layout=stereo:sample_rate=48000{}[a0]",
                post_chain
            );
            return AudioMux {
                filter_complex: Some(graph),
                maps: vec!["[a0]".into()],
                needs_encode: true,
                downmix_to_stereo: false,
            };
        }
        // Per-track graph: each surviving track gets its own volume + post
        // filter chain and its own labeled output. No amix — tracks stay
        // separate streams.
        let mut parts: Vec<String> = Vec::with_capacity(active.len());
        let mut maps: Vec<String> = Vec::with_capacity(active.len());
        for (out_idx, (idx, vol)) in active.iter().enumerate() {
            let label = format!("a{}", out_idx);
            let mut chain = format!("volume={:.4}", vol);
            if !post_mix_filters.is_empty() {
                chain.push(',');
                chain.push_str(post_mix_filters);
            }
            parts.push(format!("[0:a:{}]{}[{}]", idx, chain, label));
            maps.push(format!("[{}]", label));
        }
        return AudioMux {
            filter_complex: Some(parts.join(";")),
            maps,
            needs_encode: true,
            downmix_to_stereo: false,
        };
    }

    // === Downmix path (default). Fold everything to one stereo AAC stream. ===

    // Single-stream source with nothing to process → stream-copy. We trust
    // the source format here; multi-track sources go through the amix branch
    // below where we DO force a downmix because 7.1 @ 96 kHz from virtual
    // mixers is the case Windows Photos rejects.
    if is_default_mix && post_mix_filters.is_empty() && total_tracks <= 1 {
        return AudioMux {
            filter_complex: None,
            maps: vec!["0:a:0?".into()],
            needs_encode: false,
            downmix_to_stereo: false,
        };
    }

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
            // Unity gain, single active stream. If there's no post-mix work,
            // short-circuit to a direct stream map (returning Some("") +
            // "[aout]" here used to crash ffmpeg with "No filters specified
            // in the graph description" — the [aout] label was never defined).
            if post_mix_filters.is_empty() {
                return AudioMux {
                    filter_complex: None,
                    maps: vec![format!("0:a:{}?", idx)],
                    needs_encode: false,
                    downmix_to_stereo: false,
                };
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

    AudioMux {
        filter_complex: Some(parts.join(";")),
        maps: vec!["[aout]".into()],
        needs_encode: true,
        downmix_to_stereo: true,
    }
}

/// Emit the trailing audio args (codec, bitrate, downmix) consistently for a
/// planned `AudioMux`. Bitrate is per stream when preserving multi-track.
fn push_audio_output_args(args: &mut Vec<String>, mux: &AudioMux, bitrate_kbps: u32) {
    if mux.needs_encode {
        args.push("-c:a".into());
        args.push("aac".into());
        args.push("-b:a".into());
        args.push(format!("{}k", bitrate_kbps));
        if mux.downmix_to_stereo {
            // Source clips can carry 7.1 @ 96 kHz tracks from virtual mixers
            // (Sonar, Voicemeeter); when amix folds those in, the AAC encoder
            // defaults to the union layout — 7.1 @ 96 kHz @ 160k AAC ≈
            // 20 kbps/channel, which is both objectively bad and gets
            // rejected by Windows Photos (its Media Foundation pipeline plays
            // AAC LC stereo / mono / 5.1 only). Downmix to plain stereo so
            // the export plays in Photos and the bitrate is actually spent
            // on the two channels that matter. ffmpeg inserts the standard
            // ITU downmix coefficients for the 7.1 → stereo conversion.
            args.push("-ac".into());
            args.push("2".into());
            args.push("-ar".into());
            args.push("48000".into());
        }
    } else {
        args.push("-c:a".into());
        args.push("copy".into());
    }
}

/// Push one `-map <m>` arg per audio output stream.
fn push_audio_maps(args: &mut Vec<String>, mux: &AudioMux) {
    for m in &mux.maps {
        args.push("-map".into());
        args.push(m.clone());
    }
}

/// Returns true if any filter in the export forces a video re-encode. Only
/// crop (geometry) and speed (timestamps) qualify; audio mix changes touch
/// only the audio stream.
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
    preserve_multi_track: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip_sized invoked · src={} in={:.3} out={:.3} dur={:.3} \
             target_mb={} crop={:?} speed={:?} preserve={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            target_size_mb,
            crop.is_some(),
            speed,
            preserve_multi_track,
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
    let preserve_multi_track = preserve_multi_track.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    // Effective output duration drives the bitrate calc — a 4× speed clip is a
    // quarter as long, so the same byte budget gives 4× the per-second bitrate.
    let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
    let video_bps = target_video_bitrate_bps(target_size_mb, effective_dur);
    let video_kbps = video_bps / 1000;
    let vf = build_video_filter(crop, speed);
    let post_filters = build_audio_post_mix_filters(speed);
    let mux = build_audio_filter_complex(&mix, total_tracks, &post_filters, preserve_multi_track);

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
        ];
        push_audio_maps(&mut args, &mux);
        if !vf.is_empty() {
            args.push("-vf".into());
            args.push(vf.clone());
        }
        if let Some(fc) = &mux.filter_complex {
            args.push("-filter_complex".into());
            args.push(fc.clone());
        }
        args.extend(encoder_args_sized(enc, video_kbps));
        push_audio_output_args(&mut args, &mux, (SIZED_AUDIO_BPS / 1000) as u32);
        args.extend([
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
    preserve_multi_track: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    diag(
        &app,
        format!(
            "[export] export_concat_sized invoked · src={} regions={} target_mb={} \
             preserve={:?} tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            target_size_mb,
            preserve_multi_track,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat_sized REJECTED · no regions");
        return Err("no regions to concat".into());
    }
    let preserve_multi_track = preserve_multi_track.unwrap_or(false);
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
    let post_filters = build_audio_post_mix_filters(first.speed);
    // Per-region mix wins; falls back to function-level mix.
    let active_mix: &[TrackGain] = first.mix.as_deref().unwrap_or(&mix);
    let mux = build_audio_filter_complex(
        active_mix,
        total_tracks,
        &post_filters,
        preserve_multi_track,
    );

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
        ];
        push_audio_maps(&mut args, &mux);
        args.extend(["-fflags".into(), "+genpts".into()]);
        if !vf.is_empty() {
            args.push("-vf".into());
            args.push(vf.clone());
        }
        if let Some(fc) = &mux.filter_complex {
            args.push("-filter_complex".into());
            args.push(fc.clone());
        }
        args.extend(encoder_args_sized(enc, video_kbps));
        push_audio_output_args(&mut args, &mux, (SIZED_AUDIO_BPS / 1000) as u32);
        args.extend([
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
///   * pure stream-copy (no filters at all; preserves whatever audio streams
///     the source had — fixed in this rewrite to use `-map` for every mux
///     output stream instead of the legacy hardcoded `0:a:0?` which silently
///     dropped tracks past the first)
///   * audio-only re-encode (track mix or downmix, video stream-copies)
///   * full re-encode (crop/speed + maybe audio mix)
async fn cut_segment(
    app: &AppHandle,
    src_path: &str,
    region: RegionExport,
    out_path: &str,
    track_mix: &[TrackGain],
    total_audio_tracks: usize,
    preserve_multi_track: bool,
) -> Result<(), String> {
    let post_filters = build_audio_post_mix_filters(region.speed);
    // Per-region mix override wins; fall back to the export-wide mix.
    let active_mix: &[TrackGain] = region.mix.as_deref().unwrap_or(track_mix);
    let mux = build_audio_filter_complex(
        active_mix,
        total_audio_tracks,
        &post_filters,
        preserve_multi_track,
    );
    let needs_video_reencode = forces_video_reencode(region.crop, region.speed);
    let needs_audio_reencode = mux.needs_encode;

    if needs_video_reencode {
        let vf = build_video_filter(region.crop, region.speed);
        if let Some(ref fc) = mux.filter_complex {
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
            ];
            push_audio_maps(&mut args, &mux);
            if !vf.is_empty() {
                args.push("-vf".into());
                args.push(vf.clone());
            }
            if let Some(fc) = &mux.filter_complex {
                args.push("-filter_complex".into());
                args.push(fc.clone());
            }
            args.extend(encoder_args_high_quality(enc).into_iter().map(String::from));
            push_audio_output_args(&mut args, &mux, 192);
            args.extend(["-map_chapters".into(), "-1".into(), out_path.into()]);
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
        // Track mix or downmix triggered an audio re-encode — video stream-
        // copies, audio gets the filter_complex treatment.
        if let Some(ref fc) = mux.filter_complex {
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
        ];
        push_audio_maps(&mut args, &mux);
        args.extend(["-c:v".into(), "copy".into()]);
        if let Some(fc) = &mux.filter_complex {
            args.push("-filter_complex".into());
            args.push(fc.clone());
        }
        push_audio_output_args(&mut args, &mux, 192);
        args.extend([
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
    ];
    push_audio_maps(&mut args, &mux);
    args.extend([
        "-c".into(),
        "copy".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-map_chapters".into(),
        "-1".into(),
        out_path.into(),
    ]);
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
    preserve_multi_track: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    diag(
        &app,
        format!(
            "[export] export_concat invoked · src={} regions={} preserve={:?} \
             tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            preserve_multi_track,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat REJECTED · no regions");
        return Err("no regions to concat".into());
    }
    let preserve_multi_track = preserve_multi_track.unwrap_or(false);
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
            preserve_multi_track,
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

    // Stage 2 always stream-copies. The intermediates produced by stage 1
    // already carry the right audio shape (single downmixed stream for the
    // default export, N preserved streams when `preserve_multi_track` is on),
    // so we just map everything through. Using `0:a?` (all audio streams,
    // optional) instead of `0:a:0?` is what makes preserve work end-to-end —
    // the old hardcoded first-stream map was the silent track-loss bug.
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
        "0:a?".into(),
        "-c".into(),
        "copy".into(),
    ];
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
    preserve_multi_track: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip invoked · src={} in={:.3} out={:.3} dur={:.3} \
             crop={:?} speed={:?} preserve={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            crop.is_some(),
            speed,
            preserve_multi_track,
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if duration < 0.05 {
        diag(&app, "[export] export_clip REJECTED · selection too short");
        return Err("selection too short".into());
    }
    let preserve_multi_track = preserve_multi_track.unwrap_or(false);
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let post_filters = build_audio_post_mix_filters(speed);
    let mux = build_audio_filter_complex(&mix, total_tracks, &post_filters, preserve_multi_track);
    let needs_audio_reencode = mux.needs_encode;

    // Three paths, fastest to slowest:
    //   * pure stream-copy (no mix, no crop/speed; or preserve + identity mix)
    //   * audio re-encode only (track mix or downmix but no video filter)
    //   * full re-encode (crop/speed + maybe audio mix)
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
            ];
            push_audio_maps(&mut args, &mux);
            if !vf.is_empty() {
                args.push("-vf".into());
                args.push(vf.clone());
            }
            if let Some(fc) = &mux.filter_complex {
                args.push("-filter_complex".into());
                args.push(fc.clone());
            }
            args.extend(encoder_args_high_quality(enc).into_iter().map(String::from));
            push_audio_output_args(&mut args, &mux, 192);
            args.extend([
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
        // Video stream-copy, audio runs through filter_complex (track mix or
        // downmix). Cheap — only the audio track is touched.
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
        ];
        push_audio_maps(&mut args, &mux);
        args.extend(["-c:v".into(), "copy".into()]);
        if let Some(fc) = &mux.filter_complex {
            args.push("-filter_complex".into());
            args.push(fc.clone());
        }
        push_audio_output_args(&mut args, &mux, 192);
        args.extend([
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

    // Pure stream-copy: video and audio both copy. `-map 0` carries every
    // source stream through (video + all audio tracks); preserves multi-track
    // structure at zero CPU cost.
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
/// — there's no video stream in the output. Speed is honored via atempo.
#[tauri::command]
pub async fn export_clip_audio(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    speed: Option<f64>,
    // MP3 is a single-stream format; the toggle is accepted for IPC uniformity
    // but ignored here — we always fold to one stream via the downmix path.
    preserve_multi_track: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let _ = preserve_multi_track;
    let duration = (out_secs - in_secs).max(0.0);
    diag(
        &app,
        format!(
            "[export] export_clip_audio invoked · src={} in={:.3} out={:.3} dur={:.3} \
             speed={:?} tracks_total={:?} mix_entries={}",
            basename(&src_path),
            in_secs,
            out_secs,
            duration,
            speed,
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
    let mix = track_mix.unwrap_or_default();
    let total_tracks = total_audio_tracks.unwrap_or(1) as usize;
    let effective_dur = duration / speed.unwrap_or(1.0).max(0.0001);
    let post_filters = build_audio_post_mix_filters(speed);
    let mux = build_audio_filter_complex(&mix, total_tracks, &post_filters, false);

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
    ];
    push_audio_maps(&mut args, &mux);
    if let Some(fc) = mux.filter_complex {
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
/// demuxer feeds the regions straight into libmp3lame. Speed applies uniformly
/// across all regions (single-pass means we can't do per-region filters).
/// Frontend should gate mixed-speed cases.
#[tauri::command]
pub async fn export_concat_audio(
    app: AppHandle,
    src_path: String,
    regions: Vec<RegionExport>,
    output_path: String,
    // MP3 is a single-stream format; the toggle is accepted for IPC uniformity
    // but ignored here — we always fold to one stream via the downmix path.
    preserve_multi_track: Option<bool>,
    track_mix: Option<Vec<TrackGain>>,
    total_audio_tracks: Option<u32>,
) -> Result<(), String> {
    let _ = preserve_multi_track;
    diag(
        &app,
        format!(
            "[export] export_concat_audio invoked · src={} regions={} \
             tracks_total={:?} top_level_mix_entries={}",
            basename(&src_path),
            regions.len(),
            total_audio_tracks,
            track_mix.as_ref().map(|v| v.len()).unwrap_or(0),
        ),
    );
    if regions.is_empty() {
        diag(&app, "[export] export_concat_audio REJECTED · no regions");
        return Err("no regions to concat".into());
    }
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
    let post_filters = build_audio_post_mix_filters(first.speed);
    let active_mix: &[TrackGain] = first.mix.as_deref().unwrap_or(&mix);
    let mux = build_audio_filter_complex(active_mix, total_tracks, &post_filters, false);

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
    ];
    push_audio_maps(&mut args, &mux);
    if let Some(fc) = mux.filter_complex {
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
        if let Err(e) = cut_segment(&app, &src_path, region.clone(), &seg_str, &[], 0, false).await
        {
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

    // ----- Downmix mode (default; preserve_multi_track = false) -----

    #[test]
    fn fc_single_source_default_mix_takes_fast_path() {
        // Default mix, single source, no post-filters → direct map, no graph,
        // stream-copy (no re-encode). Matches the historical fast path that
        // `export_clip`'s `-c copy` branch consumed.
        let mux = build_audio_filter_complex(&[], 1, "", false);
        assert!(
            mux.filter_complex.is_none(),
            "single-track default mix should not build a graph"
        );
        assert_eq!(mux.maps, vec!["0:a:0?"]);
        assert!(!mux.needs_encode, "single-track no-mix should stream-copy");
        assert!(!mux.downmix_to_stereo);
    }

    #[test]
    fn fc_multi_source_default_mix_builds_amix() {
        // 3 tracks, no explicit mix entries → amix all of them at unity AND
        // force the standard stereo / 48 kHz output that keeps Windows Photos
        // happy.
        let mux = build_audio_filter_complex(&[], 3, "", false);
        let fc = mux.filter_complex.expect("multi-track default needs amix");
        assert!(fc.contains("amix=inputs=3"), "graph: {fc}");
        assert_eq!(mux.maps, vec!["[aout]"]);
        assert!(mux.needs_encode);
        assert!(mux.downmix_to_stereo);
    }

    #[test]
    fn fc_single_active_unity_no_post_filters_short_circuits() {
        // The regression: user has 1 track at unity + 0 post filters → must
        // return None + direct stream map (Some("") + "[aout]" used to crash
        // ffmpeg with code -22).
        let mix = vec![gain(0, 1.0)];
        let mux = build_audio_filter_complex(&mix, 1, "", false);
        assert!(
            mux.filter_complex.is_none(),
            "single unity-gain active track + no post-filters must NOT build a graph",
        );
        assert_eq!(mux.maps, vec!["0:a:0?"]);
        assert!(!mux.needs_encode);
    }

    #[test]
    fn fc_single_active_unity_with_post_filters_builds_graph() {
        // Same active set but WITH post-mix filters (atempo) → graph routes
        // the lone stream through them.
        let mix = vec![gain(0, 1.0)];
        let mux = build_audio_filter_complex(&mix, 1, "atempo=2.0000", false);
        let fc = mux.filter_complex.expect("post-filters force a graph");
        assert!(fc.contains("[0:a:0]"), "graph: {fc}");
        assert!(fc.contains("atempo"), "graph: {fc}");
        assert_eq!(mux.maps, vec!["[aout]"]);
        assert!(mux.needs_encode);
    }

    #[test]
    fn fc_all_muted_synthesizes_silence() {
        // Volumes all 0 → no active tracks → anullsrc fallback so the muxer
        // still has an audio stream to attach.
        let mix = vec![gain(0, 0.0), gain(1, 0.0)];
        let mux = build_audio_filter_complex(&mix, 2, "", false);
        let fc = mux.filter_complex.expect("muted-all needs anullsrc");
        assert!(fc.contains("anullsrc"), "graph: {fc}");
        assert_eq!(mux.maps, vec!["[aout]"]);
    }

    #[test]
    fn fc_single_track_non_unity_emits_volume_filter() {
        let mix = vec![gain(0, 0.5)];
        let mux = build_audio_filter_complex(&mix, 1, "", false);
        let fc = mux
            .filter_complex
            .expect("non-unity volume requires a graph");
        assert!(fc.contains("volume=0.5000"), "graph: {fc}");
        assert_eq!(mux.maps, vec!["[aout]"]);
    }

    #[test]
    fn fc_multi_track_explicit_mix_amixes_active() {
        let mix = vec![gain(0, 1.0), gain(1, 0.5), gain(2, 0.0)];
        let mux = build_audio_filter_complex(&mix, 3, "", false);
        let fc = mux.filter_complex.expect("multi-track mix needs a graph");
        // track 2 was muted → only tracks 0 + 1 should be in the amix.
        assert!(fc.contains("amix=inputs=2"), "graph: {fc}");
        assert_eq!(mux.maps, vec!["[aout]"]);
        assert!(mux.downmix_to_stereo);
    }

    // ----- Preserve mode (preserve_multi_track = true) -----

    #[test]
    fn fc_preserve_default_mix_pure_streamcopy() {
        // The headline win: identity mix on a 3-track source under preserve
        // collapses to a single `-map 0:a?` and `-c:a copy`. No filter graph,
        // no re-encode, full fidelity, source channel layouts intact.
        let mux = build_audio_filter_complex(&[], 3, "", true);
        assert!(
            mux.filter_complex.is_none(),
            "preserve + identity mix must stream-copy"
        );
        assert_eq!(mux.maps, vec!["0:a?"]);
        assert!(!mux.needs_encode, "preserve identity must NOT re-encode");
        assert!(!mux.downmix_to_stereo);
    }

    #[test]
    fn fc_preserve_mix_changes_emit_per_track_streams() {
        // Non-identity mix: each surviving track emits its own labeled output.
        let mix = vec![gain(0, 1.0), gain(1, 0.5), gain(2, 0.0)];
        let mux = build_audio_filter_complex(&mix, 3, "", true);
        let fc = mux.filter_complex.expect("non-identity mix needs a graph");
        // Track 2 muted → 2 surviving streams; no amix (preserve never folds).
        assert!(
            !fc.contains("amix"),
            "preserve must NOT fold via amix: {fc}"
        );
        assert!(fc.contains("[0:a:0]volume=1.0000[a0]"), "graph: {fc}");
        assert!(fc.contains("[0:a:1]volume=0.5000[a1]"), "graph: {fc}");
        assert_eq!(mux.maps, vec!["[a0]", "[a1]"]);
        assert!(mux.needs_encode);
        assert!(!mux.downmix_to_stereo, "preserve must NOT force stereo");
    }

    #[test]
    fn fc_preserve_with_speed_applies_atempo_per_track() {
        // Post-mix filter (atempo for speed change) must apply to every kept
        // track, not just one fold-down stream.
        let mux = build_audio_filter_complex(&[], 2, "atempo=2.0000", true);
        let fc = mux.filter_complex.expect("post-filters force a graph");
        assert!(
            fc.contains("[0:a:0]volume=1.0000,atempo=2.0000[a0]"),
            "graph: {fc}"
        );
        assert!(
            fc.contains("[0:a:1]volume=1.0000,atempo=2.0000[a1]"),
            "graph: {fc}"
        );
        assert_eq!(mux.maps, vec!["[a0]", "[a1]"]);
        assert!(mux.needs_encode);
    }

    #[test]
    fn fc_preserve_all_muted_still_emits_one_stream() {
        // Edge case: user muted every track. We still need to give the muxer
        // ONE audio stream, so synthesize silence rather than producing a file
        // with no audio at all.
        let mix = vec![gain(0, 0.0), gain(1, 0.0)];
        let mux = build_audio_filter_complex(&mix, 2, "", true);
        let fc = mux
            .filter_complex
            .expect("preserve + all-muted needs anullsrc");
        assert!(fc.contains("anullsrc"), "graph: {fc}");
        assert_eq!(mux.maps, vec!["[a0]"]);
        assert!(mux.needs_encode);
    }

    // ----- Property tests -----

    #[test]
    fn fc_returned_graph_is_never_empty_when_some() {
        // Property: if the function returns Some(graph), the graph string
        // must be non-empty. (The bug was Some("") sneaking through.)
        let cases: Vec<(Vec<TrackGain>, usize, &str, bool)> = vec![
            (vec![], 1, "", false),
            (vec![], 3, "", false),
            (vec![gain(0, 1.0)], 1, "", false),
            (vec![gain(0, 1.0)], 1, "atempo=2.0000", false),
            (vec![gain(0, 0.5)], 1, "", false),
            (vec![gain(0, 0.0)], 1, "", false),
            (vec![gain(0, 1.0), gain(1, 1.0)], 2, "", false),
            (
                vec![gain(0, 1.0), gain(1, 0.5), gain(2, 0.0)],
                3,
                "atempo=2.0000",
                false,
            ),
            // Preserve mode coverage
            (vec![], 1, "", true),
            (vec![], 3, "", true),
            (vec![gain(0, 0.5), gain(1, 1.0)], 2, "", true),
            (vec![gain(0, 0.0), gain(1, 0.0)], 2, "", true),
            (vec![], 2, "atempo=2.0000", true),
        ];
        for (mix, total, post, preserve) in cases {
            let mux = build_audio_filter_complex(&mix, total, post, preserve);
            if let Some(g) = &mux.filter_complex {
                assert!(
                    !g.is_empty(),
                    "build_audio_filter_complex returned Some(\"\") for \
                     mix={mix:?} total={total} post={post:?} preserve={preserve}"
                );
            }
            assert!(
                !mux.maps.is_empty(),
                "AudioMux.maps must never be empty for \
                 mix={mix:?} total={total} post={post:?} preserve={preserve}"
            );
        }
    }

    // ----- push_audio_output_args coverage -----

    #[test]
    fn output_args_encode_with_downmix() {
        let mux = build_audio_filter_complex(&[], 3, "", false);
        let mut args: Vec<String> = Vec::new();
        push_audio_output_args(&mut args, &mux, 192);
        let joined = args.join(" ");
        assert!(joined.contains("-c:a aac"), "args: {joined}");
        assert!(joined.contains("-b:a 192k"), "args: {joined}");
        assert!(joined.contains("-ac 2"), "args: {joined}");
        assert!(joined.contains("-ar 48000"), "args: {joined}");
    }

    #[test]
    fn output_args_streamcopy_emits_only_copy() {
        let mux = build_audio_filter_complex(&[], 3, "", true);
        let mut args: Vec<String> = Vec::new();
        push_audio_output_args(&mut args, &mux, 192);
        let joined = args.join(" ");
        assert!(joined.contains("-c:a copy"), "args: {joined}");
        assert!(!joined.contains("aac"), "should not encode: {joined}");
        assert!(!joined.contains("-ac"), "should not downmix: {joined}");
        assert!(!joined.contains("-ar"), "should not resample: {joined}");
    }

    #[test]
    fn output_args_preserve_per_track_encode_no_downmix() {
        let mix = vec![gain(0, 0.5), gain(1, 1.0)];
        let mux = build_audio_filter_complex(&mix, 2, "", true);
        let mut args: Vec<String> = Vec::new();
        push_audio_output_args(&mut args, &mux, 192);
        let joined = args.join(" ");
        assert!(joined.contains("-c:a aac"), "args: {joined}");
        assert!(joined.contains("-b:a 192k"), "args: {joined}");
        assert!(
            !joined.contains("-ac 2"),
            "preserve must not force stereo: {joined}"
        );
        assert!(
            !joined.contains("-ar 48000"),
            "preserve must not resample: {joined}"
        );
    }

    #[test]
    fn push_maps_emits_one_arg_per_stream() {
        let mix = vec![gain(0, 0.5), gain(1, 0.7), gain(2, 1.0)];
        let mux = build_audio_filter_complex(&mix, 3, "", true);
        let mut args: Vec<String> = Vec::new();
        push_audio_maps(&mut args, &mux);
        // 3 surviving tracks → 3 (-map, label) pairs = 6 args
        assert_eq!(args.len(), 6);
        assert_eq!(args[0], "-map");
        assert_eq!(args[1], "[a0]");
        assert_eq!(args[2], "-map");
        assert_eq!(args[3], "[a1]");
        assert_eq!(args[4], "-map");
        assert_eq!(args[5], "[a2]");
    }
}
