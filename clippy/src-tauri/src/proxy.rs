use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State};

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
    let mp4_native = lower
        .split(',')
        .any(|x| matches!(x.trim(), "mp4" | "mov" | "m4v" | "3gp" | "3g2"));
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
    let mut args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-i".into(),
        src_path.into(),
        "-vf".into(),
        "scale='min(1280,iw)':-2".into(),
    ];
    args.extend(encoder_args(encoder).into_iter().map(String::from));
    args.extend([
        "-g".into(),
        "15".into(),
        "-keyint_min".into(),
        "15".into(),
        "-sc_threshold".into(),
        "0".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "96k".into(),
        "-movflags".into(),
        "+faststart".into(),
        out_path.into(),
    ]);

    let app_emit = app.clone();
    let ev = event_name.to_string();
    crate::ffmpeg::run_ffmpeg(
        app,
        "proxy",
        args,
        duration_secs,
        move |progress, _elapsed| {
            let elapsed = start.elapsed().as_secs_f64();
            let eta = if progress > 0.01 {
                Some((elapsed / progress) - elapsed)
            } else {
                None
            };
            let _ = app_emit.emit(
                &ev,
                ProxyProgress {
                    progress,
                    elapsed_secs: elapsed,
                    eta_secs: eta,
                },
            );
        },
    )
    .await
}

async fn run_remux_pass(
    app: &AppHandle,
    src_path: &str,
    out_path: &str,
    duration_secs: f64,
    start: std::time::Instant,
) -> Result<(), String> {
    let args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-i".into(),
        src_path.into(),
        "-map".into(),
        "0:v:0?".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c".into(),
        "copy".into(),
        "-map_chapters".into(),
        "-1".into(),
        out_path.into(),
    ];
    let app_emit = app.clone();
    crate::ffmpeg::run_ffmpeg(
        app,
        "remux",
        args,
        duration_secs,
        move |progress, _elapsed| {
            let elapsed = start.elapsed().as_secs_f64();
            let eta = if progress > 0.01 {
                Some((elapsed / progress) - elapsed)
            } else {
                None
            };
            let _ = app_emit.emit(
                "proxy:progress",
                ProxyProgress {
                    progress,
                    elapsed_secs: elapsed,
                    eta_secs: eta,
                },
            );
        },
    )
    .await
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
                ProxyProgress {
                    progress: 1.0,
                    elapsed_secs: 0.0,
                    eta_secs: Some(0.0),
                },
            );
            diag(
                &app,
                format!(
                    "proxy: Direct — {}/{} in {} container, played as-is",
                    info.video_codec,
                    info.audio_codec.as_deref().unwrap_or("none"),
                    info.container
                ),
            );
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
            diag(
                &app,
                format!(
                    "proxy: Remux — {} container, remuxing to MP4",
                    info.container
                ),
            );
            let out_str = out_path.to_string_lossy().to_string();
            let temp_str = format!("{}.tmp.mp4", out_str);
            let result = run_remux_pass(&app, &path, &temp_str, duration_secs, start).await;
            if let Err(e) = result {
                let _ = std::fs::remove_file(&temp_str);
                diag(
                    &app,
                    format!(
                        "proxy: Remux failed ({}), falling back to encode",
                        trunc(&e, 120)
                    ),
                );
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
            diag(
                &app,
                format!("proxy: Remux done in {:.1}s", start.elapsed().as_secs_f64()),
            );
            Ok(ProxyResult {
                play_path: out_str,
                cached: false,
                strategy: "remux".to_string(),
            })
        }
        Strategy::Encode => {
            encode_fallback(&app, &path, duration_secs, start, "proxy:progress").await
        }
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
    diag(
        app,
        "proxy: Encode — codec/container needs re-encode for playback",
    );
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
    diag(
        app,
        format!(
            "proxy: Encode done via {} in {:.1}s",
            used,
            start.elapsed().as_secs_f64()
        ),
    );
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
