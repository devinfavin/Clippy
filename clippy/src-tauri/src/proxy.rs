use std::path::PathBuf;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use crate::diag::diag;
use crate::encoder_cascade::{encoder_args, encoder_chain, WORKING_ENCODER};
use crate::helpers::trunc;
use crate::media_server::{allowlist_trust, ServerInfo};
use crate::probe::VideoInfo;

#[derive(Serialize, Clone, Debug)]
pub(crate) struct ProxyProgress {
    progress: f64,
    elapsed_secs: f64,
    eta_secs: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProxyResult {
    play_path: String,
    cached: bool,
    strategy: String,
}

#[derive(Clone, Debug)]
pub(crate) enum Strategy {
    Direct,
    Remux,
    Encode,
}

pub(crate) fn classify_strategy(info: &VideoInfo) -> Strategy {
    let video_ok = matches!(info.video_codec.as_str(), "h264" | "hevc");
    let audio_ok = match &info.audio_codec {
        None => true,
        Some(c) => matches!(c.as_str(), "aac"),
    };
    if !video_ok || !audio_ok {
        return Strategy::Encode;
    }
    let lower = info.container.to_lowercase();
    let mp4_native = lower.split(',').any(|x| matches!(x.trim(), "mp4" | "mov" | "m4v" | "3gp" | "3g2"));
    if mp4_native {
        Strategy::Direct
    } else {
        Strategy::Remux
    }
}

pub(crate) fn proxy_cache_key(src_path: &str) -> Result<String, String> {
    let metadata = std::fs::metadata(src_path).map_err(|e| e.to_string())?;
    let mtime = metadata.modified().map_err(|e| e.to_string())?;
    let mtime_secs = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let size = metadata.len();
    let mut hasher = Sha256::new();
    hasher.update(src_path.as_bytes());
    hasher.update(mtime_secs.to_le_bytes());
    hasher.update(size.to_le_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn proxy_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("proxies");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

async fn run_proxy_pass(
    app: &AppHandle,
    src_path: &str,
    out_path: &str,
    encoder: &str,
    duration_secs: f64,
    start: std::time::Instant,
    event_name: &str,
) -> Result<(), String> {
    let mut args: Vec<&str> = vec![
        "-y",
        "-hide_banner",
        "-loglevel", "error",
        "-progress", "pipe:1",
        "-nostats",
        "-i", src_path,
        "-vf", "scale='min(1280,iw)':-2",
    ];
    args.extend(encoder_args(encoder));
    args.extend([
        "-g", "15",
        "-keyint_min", "15",
        "-sc_threshold", "0",
        "-c:a", "aac",
        "-b:a", "96k",
        "-movflags", "+faststart",
        out_path,
    ]);

    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar.args(args).spawn().map_err(|e| e.to_string())?;

    let mut last_emit = std::time::Instant::now();
    let total_us = duration_secs * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            // Encoder pipelining can report out-of-order timestamps;
                            // clamp to monotonic so the displayed % doesn't bounce.
                            if us > latest_us { latest_us = us; }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 200 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let elapsed = start.elapsed().as_secs_f64();
                    let eta = if progress > 0.01 {
                        Some((elapsed / progress) - elapsed)
                    } else {
                        None
                    };
                    let _ = app.emit(
                        event_name,
                        ProxyProgress { progress, elapsed_secs: elapsed, eta_secs: eta },
                    );
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "ffmpeg ({}) exited with code {:?}: {}",
                        encoder, payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

async fn run_remux_pass(
    app: &AppHandle,
    src_path: &str,
    out_path: &str,
    duration_secs: f64,
    start: std::time::Instant,
) -> Result<(), String> {
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-progress", "pipe:1",
            "-nostats",
            "-i", src_path,
            "-map", "0:v:0?",
            "-map", "0:a:0?",
            "-c", "copy",
            "-map_chapters", "-1",
            out_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    let mut last_emit = std::time::Instant::now();
    let total_us = duration_secs * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            // Encoder pipelining can report out-of-order timestamps;
                            // clamp to monotonic so the displayed % doesn't bounce.
                            if us > latest_us { latest_us = us; }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 100 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let elapsed = start.elapsed().as_secs_f64();
                    let eta = if progress > 0.01 {
                        Some((elapsed / progress) - elapsed)
                    } else {
                        None
                    };
                    let _ = app.emit(
                        "proxy:progress",
                        ProxyProgress { progress, elapsed_secs: elapsed, eta_secs: eta },
                    );
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "ffmpeg (remux) exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn generate_proxy(
    app: AppHandle,
    state: State<'_, ServerInfo>,
    path: String,
    info: VideoInfo,
) -> Result<ProxyResult, String> {
    // Caller has already probed; reuse the result so we don't pay another
    // ffprobe spawn (~300-500 ms on big MKVs).
    let duration_secs = info.duration_secs;
    let strategy = classify_strategy(&info);
    let start = std::time::Instant::now();

    match strategy {
        Strategy::Direct => {
            // Direct path returns the source MP4 unchanged. It's outside
            // proxy_dir, so register_file_url's scope check would otherwise
            // reject it — pre-trust it here since the user explicitly
            // selected this file.
            allowlist_trust(&state.state, std::path::Path::new(&path)).await?;
            let _ = app.emit(
                "proxy:progress",
                ProxyProgress { progress: 1.0, elapsed_secs: 0.0, eta_secs: Some(0.0) },
            );
            diag(&app, format!("proxy: Direct — {}/{} in {} container, played as-is",
                info.video_codec, info.audio_codec.as_deref().unwrap_or("none"), info.container));
            Ok(ProxyResult {
                play_path: path,
                cached: true,
                strategy: "direct".to_string(),
            })
        }
        Strategy::Remux => {
            let key = proxy_cache_key(&path)?;
            let out_path = proxy_dir(&app)?.join(format!("{}.remux.mp4", &key[..32]));
            if out_path.exists() {
                diag(&app, "proxy: Remux — cache hit");
                return Ok(ProxyResult {
                    play_path: out_path.to_string_lossy().to_string(),
                    cached: true,
                    strategy: "remux".to_string(),
                });
            }
            diag(&app, format!("proxy: Remux — {} container, remuxing to MP4", info.container));
            let out_str = out_path.to_string_lossy().to_string();
            let temp_str = format!("{}.tmp.mp4", out_str);
            let result = run_remux_pass(&app, &path, &temp_str, duration_secs, start).await;
            if let Err(e) = result {
                let _ = std::fs::remove_file(&temp_str);
                diag(&app, format!("proxy: Remux failed ({}), falling back to encode", trunc(&e, 120)));
                eprintln!("[clippy] remux failed: {} — falling back to encode", e);
                return encode_fallback(&app, &path, duration_secs, start, "proxy:progress").await;
            }
            std::fs::rename(&temp_str, &out_path).map_err(|e| e.to_string())?;
            let _ = app.emit(
                "proxy:progress",
                ProxyProgress {
                    progress: 1.0,
                    elapsed_secs: start.elapsed().as_secs_f64(),
                    eta_secs: Some(0.0),
                },
            );
            diag(&app, format!("proxy: Remux done in {:.1}s", start.elapsed().as_secs_f64()));
            Ok(ProxyResult {
                play_path: out_str,
                cached: false,
                strategy: "remux".to_string(),
            })
        }
        Strategy::Encode => encode_fallback(&app, &path, duration_secs, start, "proxy:progress").await,
    }
}

async fn encode_fallback(
    app: &AppHandle,
    path: &str,
    duration_secs: f64,
    start: std::time::Instant,
    event_name: &str,
) -> Result<ProxyResult, String> {
    let key = proxy_cache_key(path)?;
    let out_path = proxy_dir(app)?.join(format!("{}.proxy.mp4", &key[..32]));
    if out_path.exists() {
        diag(app, "proxy: Encode — cache hit");
        return Ok(ProxyResult {
            play_path: out_path.to_string_lossy().to_string(),
            cached: true,
            strategy: "encode (cached)".to_string(),
        });
    }
    diag(app, "proxy: Encode — codec/container needs re-encode for playback");
    let out_str = out_path.to_string_lossy().to_string();
    let temp_str = format!("{}.tmp.mp4", out_str);

    let chain = encoder_chain(app).await;
    let mut used: Option<&'static str> = None;
    let mut last_err = String::from("no encoders available");

    for enc in chain.iter() {
        let _ = std::fs::remove_file(&temp_str);
        match run_proxy_pass(app, path, &temp_str, enc, duration_secs, start, event_name).await {
            Ok(()) => {
                used = Some(*enc);
                *WORKING_ENCODER.lock().unwrap() = Some(*enc);
                break;
            }
            Err(e) => {
                diag(app, format!("proxy: encoder {} failed — trying next", enc));
                eprintln!("[clippy] encoder {} failed: {}", enc, e);
                last_err = e;
            }
        }
    }

    let used = match used {
        Some(e) => e,
        None => {
            let _ = std::fs::remove_file(&temp_str);
            return Err(last_err);
        }
    };

    std::fs::rename(&temp_str, &out_path).map_err(|e| e.to_string())?;
    diag(app, format!("proxy: Encode done via {} in {:.1}s", used, start.elapsed().as_secs_f64()));
    let _ = app.emit(
        event_name,
        ProxyProgress {
            progress: 1.0,
            elapsed_secs: start.elapsed().as_secs_f64(),
            eta_secs: Some(0.0),
        },
    );
    Ok(ProxyResult {
        play_path: out_str,
        cached: false,
        strategy: format!("encode ({})", used),
    })
}
