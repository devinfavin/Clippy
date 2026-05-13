use tauri::{AppHandle, State};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use crate::media_server::ServerInfo;
use crate::proxy::{proxy_cache_key, proxy_dir};

// ---- Per-track audio extraction (for WebAudio multi-track preview) ----
//
// SteelSeries Sonar / OBS produce MP4s with separate audio tracks for game,
// mic, Discord, etc. The HTML5 video element only plays one track at a time,
// so to give the user real per-track mute/volume sliders we extract each
// audio stream into its own playable file and feed them through WebAudio.
// The same fingerprint-keyed cache as proxies/waveforms.

#[tauri::command]
pub async fn extract_track(
    app: AppHandle,
    state: State<'_, ServerInfo>,
    src_path: String,
    track_index: u32,
) -> Result<String, String> {
    let key = proxy_cache_key(&src_path)?;
    let cache_path = proxy_dir(&app)?.join(format!(
        "{}.track-{}.m4a",
        &key[..32],
        track_index
    ));
    if !cache_path.exists() {
        let cache_str = cache_path.to_string_lossy().to_string();
        let temp_str = format!("{}.tmp.m4a", cache_str);
        // Stream-copy the requested audio track into an MP4-in-M4A container.
        // -bsf:a aac_adtstoasc handles the rare AAC-in-MPEG-TS case; harmless
        // for already-clean AAC.
        let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
        let (mut rx, _child) = sidecar
            .args([
                "-y",
                "-hide_banner",
                "-loglevel", "error",
                "-i", &src_path,
                "-map", &format!("0:a:{}?", track_index),
                "-vn",
                "-c:a", "copy",
                "-bsf:a", "aac_adtstoasc",
                "-map_chapters", "-1",
                &temp_str,
            ])
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut stderr_buf = String::new();
        let mut ok = false;
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
                CommandEvent::Terminated(payload) => {
                    if payload.code == Some(0) { ok = true; }
                    break;
                }
                _ => {}
            }
        }
        if !ok {
            // Fallback: re-encode to AAC. Source might be a codec we can't
            // copy into M4A (e.g. opus). Slow but always works.
            let _ = std::fs::remove_file(&temp_str);
            let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
            let (mut rx, _child) = sidecar
                .args([
                    "-y",
                    "-hide_banner",
                    "-loglevel", "error",
                    "-i", &src_path,
                    "-map", &format!("0:a:{}?", track_index),
                    "-vn",
                    "-c:a", "aac",
                    "-b:a", "192k",
                    "-map_chapters", "-1",
                    &temp_str,
                ])
                .spawn()
                .map_err(|e| e.to_string())?;
            stderr_buf.clear();
            let mut ok2 = false;
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
                    CommandEvent::Terminated(payload) => {
                        if payload.code == Some(0) { ok2 = true; }
                        break;
                    }
                    _ => {}
                }
            }
            if !ok2 {
                let _ = std::fs::remove_file(&temp_str);
                return Err(format!("track extract failed: {}", stderr_buf));
            }
        }
        std::fs::rename(&temp_str, &cache_path).map_err(|e| e.to_string())?;
    }
    // Register with the media server and return the playable URL.
    let path_str = cache_path.to_string_lossy().to_string();
    state.state.allowlist.lock().await.insert(cache_path);
    let encoded = urlencoding::encode(&path_str).into_owned();
    Ok(format!(
        "http://127.0.0.1:{}/vid?token={}&p={}",
        state.port, state.state.token, encoded
    ))
}
