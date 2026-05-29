use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use crate::diag::diag;
use crate::helpers::basename;
use crate::media_server::ServerInfo;
use crate::proxy::{proxy_cache_key, proxy_dir};

// ---- Per-track audio extraction (for WebAudio multi-track preview) ----
//
// SteelSeries Sonar / OBS produce MP4s with separate audio tracks for game,
// mic, Discord, etc. The HTML5 video element only plays one track at a time,
// so to give the user real per-track mute/volume sliders we extract each
// audio stream into its own playable file and feed them through WebAudio.
// The same fingerprint-keyed cache as proxies/waveforms.

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TrackExtractResult {
    pub track_index: u32,
    /// Playable media-server URL on success.
    pub url: Option<String>,
    /// ffmpeg stderr / IO error on failure.
    pub error: Option<String>,
}

fn cache_path_for(dir: &Path, key: &str, track_index: u32) -> PathBuf {
    dir.join(format!("{}.track-{}.m4a", &key[..32], track_index))
}

async fn register_url(state: &ServerInfo, cache_path: PathBuf) -> String {
    let path_str = cache_path.to_string_lossy().to_string();
    state.state.allowlist.lock().await.insert(cache_path);
    let encoded = urlencoding::encode(&path_str).into_owned();
    format!(
        "http://127.0.0.1:{}/vid?token={}&p={}",
        state.port, state.state.token, encoded
    )
}

/// Stream-copy a single track to `temp_path`. Returns Ok on success or
/// Err(stderr) on ffmpeg failure (which tells the caller to try re-encode).
async fn ffmpeg_copy_one(
    app: &AppHandle,
    src_path: &str,
    track_index: u32,
    temp_path: &str,
) -> Result<(), String> {
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            src_path,
            "-map",
            &format!("0:a:{}?", track_index),
            "-vn",
            "-c:a",
            "copy",
            "-bsf:a",
            "aac_adtstoasc",
            "-map_chapters",
            "-1",
            temp_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
            CommandEvent::Terminated(payload) => {
                if payload.code == Some(0) {
                    return Ok(());
                }
                return Err(stderr_buf);
            }
            _ => {}
        }
    }
    Err(stderr_buf)
}

async fn ffmpeg_reencode_one(
    app: &AppHandle,
    src_path: &str,
    track_index: u32,
    temp_path: &str,
) -> Result<(), String> {
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            src_path,
            "-map",
            &format!("0:a:{}?", track_index),
            "-vn",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-map_chapters",
            "-1",
            temp_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
            CommandEvent::Terminated(payload) => {
                if payload.code == Some(0) {
                    return Ok(());
                }
                return Err(stderr_buf);
            }
            _ => {}
        }
    }
    Err(stderr_buf)
}

/// Extract one track to `cache_path`. No-op if it already exists. Emits a diag
/// line tagged HIT / MISS so a slow open is traceable to the right track.
async fn extract_one_to_cache(
    app: &AppHandle,
    src_path: &str,
    track_index: u32,
    cache_path: &Path,
) -> Result<(), String> {
    let t0 = std::time::Instant::now();
    if cache_path.exists() {
        diag(
            app,
            format!(
                "[extract_track] HIT · {} track={} ({}ms)",
                basename(src_path),
                track_index,
                t0.elapsed().as_millis()
            ),
        );
        return Ok(());
    }
    let cache_str = cache_path.to_string_lossy().to_string();
    let temp_str = format!("{}.tmp.m4a", cache_str);

    let copy_err = ffmpeg_copy_one(app, src_path, track_index, &temp_str)
        .await
        .err();
    let copy_ms = t0.elapsed().as_millis();
    if let Some(_err) = &copy_err {
        // Fallback: re-encode. Source codec might not fit in M4A (e.g. opus).
        let _ = std::fs::remove_file(&temp_str);
        if let Err(e) = ffmpeg_reencode_one(app, src_path, track_index, &temp_str).await {
            let _ = std::fs::remove_file(&temp_str);
            diag(
                app,
                format!(
                    "[extract_track] FAILED · {} track={} ({}ms)",
                    basename(src_path),
                    track_index,
                    t0.elapsed().as_millis()
                ),
            );
            return Err(format!("track extract failed: {}", e));
        }
        diag(
            app,
            format!(
                "[extract_track] MISS · {} track={} re-encode ({}ms total, copy attempt {}ms)",
                basename(src_path),
                track_index,
                t0.elapsed().as_millis(),
                copy_ms
            ),
        );
    } else {
        diag(
            app,
            format!(
                "[extract_track] MISS · {} track={} stream-copy ({}ms)",
                basename(src_path),
                track_index,
                copy_ms
            ),
        );
    }
    std::fs::rename(&temp_str, cache_path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn extract_track(
    app: AppHandle,
    state: State<'_, ServerInfo>,
    src_path: String,
    track_index: u32,
) -> Result<String, String> {
    let key = proxy_cache_key(&src_path)?;
    let cache_path = cache_path_for(&proxy_dir(&app)?, &key, track_index);
    extract_one_to_cache(&app, &src_path, track_index, &cache_path).await?;
    Ok(register_url(&state, cache_path).await)
}

/// Extract every requested track from `src_path` in a single ffmpeg read pass
/// when possible. Falls back to per-track sequential extraction (still no
/// parallel disk thrash) if the batched stream-copy fails — typically because
/// one track's codec doesn't fit in M4A and needs a re-encode.
///
/// Cached tracks are skipped from the batch. The return vector preserves the
/// input order so callers can zip results back to their tracks; entries with
/// `url: None` indicate per-track failure with `error` populated.
#[tauri::command]
pub async fn extract_tracks_batch(
    app: AppHandle,
    state: State<'_, ServerInfo>,
    src_path: String,
    track_indices: Vec<u32>,
) -> Result<Vec<TrackExtractResult>, String> {
    if track_indices.is_empty() {
        return Ok(Vec::new());
    }
    let t0 = std::time::Instant::now();
    let key = proxy_cache_key(&src_path)?;
    let dir = proxy_dir(&app)?;

    // Resolve cache paths once. We need them both to skip HITs and to rename
    // tmp → final after the batched ffmpeg run.
    let cache_paths: Vec<PathBuf> = track_indices
        .iter()
        .map(|&idx| cache_path_for(&dir, &key, idx))
        .collect();

    // Miss list: tracks that don't have a cached extract yet. Skip duplicates
    // (frontend can ask for the same idx twice in pathological cases).
    let mut misses: Vec<(u32, PathBuf, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (&idx, cache_path) in track_indices.iter().zip(cache_paths.iter()) {
        if !seen.insert(idx) {
            continue;
        }
        if cache_path.exists() {
            continue;
        }
        let cache_str = cache_path.to_string_lossy().to_string();
        let temp_str = format!("{}.tmp.m4a", cache_str);
        misses.push((idx, cache_path.clone(), temp_str));
    }

    let hits = track_indices.len() - misses.len();
    let mut batch_failed = false;

    if !misses.is_empty() {
        // Build one ffmpeg command with one input and N outputs. ffmpeg reads
        // the source exactly once and demuxes packets to every matching
        // output — the whole point of this command vs. N parallel
        // extract_track calls thrashing the same disk.
        let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
        let mut args: Vec<String> = vec![
            "-y".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-i".into(),
            src_path.clone(),
        ];
        for (idx, _, temp) in &misses {
            args.extend_from_slice(&[
                "-map".into(),
                format!("0:a:{}?", idx),
                "-vn".into(),
                "-c:a".into(),
                "copy".into(),
                "-bsf:a".into(),
                "aac_adtstoasc".into(),
                "-map_chapters".into(),
                "-1".into(),
                temp.clone(),
            ]);
        }
        let (mut rx, _child) = sidecar.args(args).spawn().map_err(|e| e.to_string())?;
        let mut stderr_buf = String::new();
        let mut ok = false;
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
                CommandEvent::Terminated(payload) => {
                    if payload.code == Some(0) {
                        ok = true;
                    }
                    break;
                }
                _ => {}
            }
        }
        if ok {
            // Promote every temp file to its cache path. If any individual
            // rename fails we still count the rest as good — the failed one
            // will be retried on the next open.
            for (idx, cache_path, temp) in &misses {
                if let Err(e) = std::fs::rename(temp, cache_path) {
                    let _ = std::fs::remove_file(temp);
                    diag(
                        &app,
                        format!(
                            "[extract_tracks_batch] rename failed · {} track={}: {}",
                            basename(&src_path),
                            idx,
                            e
                        ),
                    );
                }
            }
            diag(
                &app,
                format!(
                    "[extract_tracks_batch] batch-copy · {} {} hit / {} extracted ({}ms)",
                    basename(&src_path),
                    hits,
                    misses.len(),
                    t0.elapsed().as_millis()
                ),
            );
        } else {
            batch_failed = true;
            // Wipe any partial temps before falling back so the fallback's
            // own rename doesn't trip over a stale file.
            for (_, _, temp) in &misses {
                let _ = std::fs::remove_file(temp);
            }
            diag(
                &app,
                format!(
                    "[extract_tracks_batch] batch-copy FAILED · {} ({}ms) — falling back per-track. stderr: {}",
                    basename(&src_path),
                    t0.elapsed().as_millis(),
                    stderr_buf.lines().last().unwrap_or("")
                ),
            );
        }
    }

    // Fallback path: any track that's still missing (either batch failed or
    // batch rename failed for that one) gets the per-track extract w/
    // re-encode fallback. Sequential to keep priority-2 (no parallel disk
    // thrash) intact.
    let mut failures: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    if batch_failed {
        for (idx, cache_path, _) in &misses {
            if cache_path.exists() {
                continue;
            }
            if let Err(e) = extract_one_to_cache(&app, &src_path, *idx, cache_path).await {
                failures.insert(*idx, e);
            }
        }
    }

    // Build the result vector in input order, registering successes with the
    // media server and returning URLs.
    let mut out: Vec<TrackExtractResult> = Vec::with_capacity(track_indices.len());
    for (&idx, cache_path) in track_indices.iter().zip(cache_paths.iter()) {
        if cache_path.exists() {
            let url = register_url(&state, cache_path.clone()).await;
            out.push(TrackExtractResult {
                track_index: idx,
                url: Some(url),
                error: None,
            });
        } else {
            let err = failures
                .remove(&idx)
                .unwrap_or_else(|| "track extract failed".to_string());
            out.push(TrackExtractResult {
                track_index: idx,
                url: None,
                error: Some(err),
            });
        }
    }
    Ok(out)
}
