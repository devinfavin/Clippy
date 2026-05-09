use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use axum::body::Body;
use axum::extract::{Query, State as AxumState};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::io::ReaderStream;

// ----- localhost media server -----
//
// Chromium plays HTTP files much more reliably than custom protocols, so we
// stand up a tiny in-process HTTP server on a random localhost port and point
// the <video> element at it. The server:
//   * requires a per-session token (so other origins in the webview can't
//     guess the URL),
//   * only serves files in an allowlist (paths the user has explicitly
//     registered through register_file_url), and
//   * supports byte-range requests, which is what kills the asset:// hitches.

#[derive(Clone)]
struct ServerState {
    token: String,
    allowlist: Arc<AsyncMutex<HashSet<PathBuf>>>,
}

struct ServerInfo {
    port: u16,
    state: ServerState,
}

#[derive(Deserialize)]
struct ServeQuery {
    token: String,
    p: String,
}

fn generate_session_token() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut h = Sha256::new();
    h.update(now.to_le_bytes());
    h.update(pid.to_le_bytes());
    format!("{:x}", h.finalize())
}

async fn serve_file(
    AxumState(state): AxumState<ServerState>,
    Query(q): Query<ServeQuery>,
    headers: HeaderMap,
) -> Response {
    if q.token != state.token {
        return (StatusCode::FORBIDDEN, "bad token").into_response();
    }
    let path = PathBuf::from(&q.p);
    {
        let allow = state.allowlist.lock().await;
        if !allow.contains(&path) {
            return (StatusCode::FORBIDDEN, "not allowed").into_response();
        }
    }
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "open failed").into_response(),
    };
    let total = match file.metadata().await {
        Ok(m) => m.len(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "metadata").into_response(),
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mime_str = mime.essence_str().to_string();

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some(range) = range_header {
        if let Some(rest) = range.strip_prefix("bytes=") {
            let parts: Vec<&str> = rest.split('-').collect();
            if parts.len() == 2 {
                let start: u64 = parts[0].parse().unwrap_or(0);
                let end: u64 = if parts[1].is_empty() {
                    total.saturating_sub(1)
                } else {
                    parts[1]
                        .parse::<u64>()
                        .unwrap_or(total.saturating_sub(1))
                        .min(total.saturating_sub(1))
                };
                if total == 0 || start > end {
                    return Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{}", total))
                        .body(Body::empty())
                        .unwrap();
                }
                let chunk_len = end - start + 1;
                if file.seek(SeekFrom::Start(start)).await.is_err() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "seek").into_response();
                }
                let limited = file.take(chunk_len);
                let stream = ReaderStream::new(limited);
                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, mime_str)
                    .header(header::CONTENT_LENGTH, chunk_len.to_string())
                    .header(
                        header::CONTENT_RANGE,
                        format!("bytes {}-{}/{}", start, end, total),
                    )
                    .header(header::ACCEPT_RANGES, "bytes")
                    .body(Body::from_stream(stream))
                    .unwrap();
            }
        }
    }

    let stream = ReaderStream::new(file);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_str)
        .header(header::CONTENT_LENGTH, total.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Return the size of a file in bytes. Used by the post-export toast to show
/// the resulting file size without round-tripping through tauri-plugin-fs scope.
#[tauri::command]
fn file_size(path: String) -> Result<u64, String> {
    std::fs::metadata(&path)
        .map(|m| m.len())
        .map_err(|e| format!("file_size {}: {}", path, e))
}

/// Open the OS file manager with the given file selected. Windows-specific:
/// uses explorer.exe with /select,. On other platforms we'd fall back to opening
/// the parent directory.
#[tauri::command]
fn reveal_in_folder(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        // Linux fallback: open the parent dir with xdg-open
        let parent = std::path::Path::new(&path)
            .parent()
            .ok_or_else(|| "no parent directory".to_string())?;
        std::process::Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn register_file_url(
    state: State<'_, ServerInfo>,
    path: String,
) -> Result<String, String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("path does not exist: {}", path));
    }
    state.state.allowlist.lock().await.insert(p);
    let encoded = urlencoding::encode(&path).into_owned();
    Ok(format!(
        "http://127.0.0.1:{}/vid?token={}&p={}",
        state.port, state.state.token, encoded
    ))
}

// Cached encoder that we've actually verified works on this machine (set after a successful pass).
static WORKING_ENCODER: Mutex<Option<&'static str>> = Mutex::new(None);

async fn encoder_chain(app: &AppHandle) -> Vec<&'static str> {
    // If we already know what works on this box, skip the others.
    if let Some(working) = *WORKING_ENCODER.lock().unwrap() {
        if working == "libx264" {
            return vec!["libx264"];
        }
        return vec![working, "libx264"];
    }
    // First time — detect what's in the ffmpeg build, then try them in priority order.
    let priority: [&'static str; 3] = ["h264_nvenc", "h264_amf", "h264_qsv"];
    let mut chain: Vec<&'static str> = vec![];
    if let Ok(sidecar) = app.shell().sidecar("ffmpeg") {
        if let Ok(out) = sidecar.args(["-hide_banner", "-encoders"]).output().await {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).to_string();
                for enc in priority.iter() {
                    if s.contains(*enc) {
                        chain.push(*enc);
                    }
                }
            }
        }
    }
    chain.push("libx264");
    chain
}

/// Audio bitrate (in bps) used for size-targeted re-encoded exports. Subtracted
/// from the size budget when calculating target video bitrate.
const SIZED_AUDIO_BPS: u64 = 96_000;

/// Encoder args for a fixed-bitrate (CBR-ish) re-encode targeting a specific
/// video kbps, used by the Discord-size export path.
fn encoder_args_sized(encoder: &str, video_kbps: u64) -> Vec<String> {
    let bv = format!("{}k", video_kbps);
    let maxrate = format!("{}k", video_kbps);
    let bufsize = format!("{}k", video_kbps * 2);
    match encoder {
        "h264_nvenc" => vec![
            "-c:v".into(), "h264_nvenc".into(),
            "-preset".into(), "p4".into(),
            "-tune".into(), "ll".into(),
            "-rc".into(), "cbr".into(),
            "-b:v".into(), bv,
            "-maxrate".into(), maxrate,
            "-bufsize".into(), bufsize,
        ],
        "h264_amf" => vec![
            "-c:v".into(), "h264_amf".into(),
            "-quality".into(), "speed".into(),
            "-rc".into(), "cbr".into(),
            "-b:v".into(), bv,
            "-maxrate".into(), maxrate,
            "-bufsize".into(), bufsize,
        ],
        "h264_qsv" => vec![
            "-c:v".into(), "h264_qsv".into(),
            "-preset".into(), "veryfast".into(),
            "-b:v".into(), bv,
            "-maxrate".into(), maxrate,
            "-bufsize".into(), bufsize,
        ],
        _ => vec![
            "-c:v".into(), "libx264".into(),
            "-preset".into(), "veryfast".into(),
            "-b:v".into(), bv,
            "-maxrate".into(), maxrate,
            "-bufsize".into(), bufsize,
        ],
    }
}

/// Compute the video bitrate (bps) that should hit a target size in MB for a
/// given clip duration, leaving a small safety margin and reserving the audio
/// budget. Floors at 200 kbps to avoid outputs that look like a smear.
fn target_video_bitrate_bps(target_mb: f64, duration_secs: f64) -> u64 {
    if duration_secs <= 0.0 {
        return 200_000;
    }
    let safety = 0.95_f64;
    let target_bytes = target_mb * 1024.0 * 1024.0 * safety;
    let audio_bytes = (SIZED_AUDIO_BPS as f64) / 8.0 * duration_secs;
    let video_bytes = (target_bytes - audio_bytes).max(25_000.0);
    let bps = (video_bytes * 8.0 / duration_secs) as u64;
    bps.max(200_000)
}

fn encoder_args(encoder: &str) -> Vec<&'static str> {
    match encoder {
        "h264_nvenc" => vec![
            "-c:v", "h264_nvenc",
            "-preset", "p4",
            "-tune", "ll",
            "-rc", "vbr",
            "-cq", "28",
            "-b:v", "0",
        ],
        "h264_amf" => vec![
            "-c:v", "h264_amf",
            "-quality", "speed",
            "-rc", "cqp",
            "-qp_i", "28",
            "-qp_p", "28",
        ],
        "h264_qsv" => vec![
            "-c:v", "h264_qsv",
            "-preset", "veryfast",
            "-global_quality", "28",
        ],
        _ => vec![
            "-c:v", "libx264",
            "-preset", "veryfast",
            "-crf", "28",
        ],
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct VideoInfo {
    duration_secs: f64,
    width: u32,
    height: u32,
    fps: f64,
    video_codec: String,
    audio_codec: Option<String>,
    container: String,
    bit_rate_bps: Option<u64>,
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
    let audio_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"));

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
    let audio_codec = audio_stream.and_then(|s| {
        s.get("codec_name")
            .and_then(|v| v.as_str())
            .map(|x| x.to_string())
    });

    Ok(VideoInfo {
        duration_secs,
        width,
        height,
        fps,
        video_codec,
        audio_codec,
        container,
        bit_rate_bps,
    })
}

#[tauri::command]
async fn probe_video(app: AppHandle, path: String) -> Result<VideoInfo, String> {
    probe_video_inner(&app, &path).await
}

#[derive(Serialize, Clone, Debug)]
struct ProxyProgress {
    progress: f64,
    elapsed_secs: f64,
    eta_secs: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
struct ProxyResult {
    play_path: String,
    cached: bool,
    strategy: String,
}

#[derive(Clone, Debug)]
enum Strategy {
    Direct,
    Remux,
    Encode,
}

fn classify_strategy(info: &VideoInfo) -> Strategy {
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

fn proxy_cache_key(src_path: &str) -> Result<String, String> {
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

fn proxy_dir(app: &AppHandle) -> Result<PathBuf, String> {
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
async fn generate_proxy(
    app: AppHandle,
    path: String,
    duration_secs: f64,
) -> Result<ProxyResult, String> {
    let info = probe_video_inner(&app, &path).await?;
    let strategy = classify_strategy(&info);
    let start = std::time::Instant::now();

    match strategy {
        Strategy::Direct => {
            let _ = app.emit(
                "proxy:progress",
                ProxyProgress { progress: 1.0, elapsed_secs: 0.0, eta_secs: Some(0.0) },
            );
            Ok(ProxyResult {
                play_path: path,
                cached: true,
                strategy: "direct".to_string(),
            })
        }
        Strategy::Remux => {
            let key = proxy_cache_key(&path)?;
            let out_path = proxy_dir(&app)?.join(format!("{}.remux.mp4", &key[..16]));
            if out_path.exists() {
                return Ok(ProxyResult {
                    play_path: out_path.to_string_lossy().to_string(),
                    cached: true,
                    strategy: "remux".to_string(),
                });
            }
            let out_str = out_path.to_string_lossy().to_string();
            let temp_str = format!("{}.tmp.mp4", out_str);
            let result = run_remux_pass(&app, &path, &temp_str, duration_secs, start).await;
            if let Err(e) = result {
                let _ = std::fs::remove_file(&temp_str);
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
    let out_path = proxy_dir(app)?.join(format!("{}.proxy.mp4", &key[..16]));
    if out_path.exists() {
        return Ok(ProxyResult {
            play_path: out_path.to_string_lossy().to_string(),
            cached: true,
            strategy: "encode (cached)".to_string(),
        });
    }
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

const WAVEFORM_BINS: usize = 4000;

/// Extract a peak-amplitude waveform from the source audio. Returns a vector of
/// WAVEFORM_BINS f32 values in [0, 1] where each bin is the max sample magnitude
/// over its slice of the timeline. Cached on disk per source-file fingerprint.
#[tauri::command]
async fn extract_waveform(app: AppHandle, path: String) -> Result<Vec<f32>, String> {
    // Cache check
    let key = proxy_cache_key(&path)?;
    let cache_path = proxy_dir(&app)?.join(format!("{}.wave.f32", &key[..16]));
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
                return Ok(bins);
            }
        }
    }

    // Probe to know if there's audio at all and the duration.
    let info = probe_video_inner(&app, &path).await?;
    if info.audio_codec.is_none() || info.duration_secs <= 0.0 {
        return Ok(vec![0.0; WAVEFORM_BINS]);
    }

    // Stream raw mono 8kHz s16le PCM from ffmpeg's stdout.
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-i", &path,
            "-vn",
            "-ac", "1",
            "-ar", "8000",
            "-f", "s16le",
            "-",
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    let mut pcm: Vec<u8> = Vec::with_capacity((info.duration_secs * 8000.0 * 2.0) as usize + 1024);
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(bytes) => pcm.extend_from_slice(&bytes),
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

    let n_samples = pcm.len() / 2;
    let mut bins = vec![0.0f32; WAVEFORM_BINS];
    if n_samples > 0 {
        let samples_per_bin = (n_samples as f64) / (WAVEFORM_BINS as f64);
        for bin_idx in 0..WAVEFORM_BINS {
            let start = (bin_idx as f64 * samples_per_bin).floor() as usize;
            let end = (((bin_idx + 1) as f64) * samples_per_bin).ceil() as usize;
            let end = end.min(n_samples);
            if start >= end {
                continue;
            }
            let mut max_amp: f32 = 0.0;
            for i in start..end {
                let lo = pcm[i * 2];
                let hi = pcm[i * 2 + 1];
                let sample = i16::from_le_bytes([lo, hi]);
                let amp = (sample.unsigned_abs() as f32) / 32768.0;
                if amp > max_amp {
                    max_amp = amp;
                }
            }
            bins[bin_idx] = max_amp;
        }
    }

    // Cache
    let mut buf = Vec::with_capacity(WAVEFORM_BINS * 4);
    for &v in &bins {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let _ = std::fs::write(&cache_path, &buf);

    Ok(bins)
}

#[derive(Serialize, Clone, Debug)]
struct ExportProgress {
    progress: f64,
    elapsed_secs: f64,
}

/// Run ffmpeg with the given args and report progress on the given event
/// channel until termination. Used by both the size-targeted clip and stitch
/// exporters.
async fn run_ffmpeg_with_progress(
    app: &AppHandle,
    args: Vec<String>,
    duration_secs: f64,
    event_name: &str,
) -> Result<(), String> {
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar.args(args).spawn().map_err(|e| e.to_string())?;
    let start = std::time::Instant::now();
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
                            if us > latest_us {
                                latest_us = us;
                            }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 150 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let _ = app.emit(
                        event_name,
                        ExportProgress {
                            progress,
                            elapsed_secs: start.elapsed().as_secs_f64(),
                        },
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
                        "ffmpeg exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                let _ = app.emit(
                    event_name,
                    ExportProgress {
                        progress: 1.0,
                        elapsed_secs: start.elapsed().as_secs_f64(),
                    },
                );
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Re-encode a single region from src_path to fit within target_size_mb.
/// Cascades through the available hardware encoders and falls back to libx264.
#[tauri::command]
async fn export_clip_sized(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
    target_size_mb: f64,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    if duration < 0.05 {
        return Err("selection too short".into());
    }
    let video_bps = target_video_bitrate_bps(target_size_mb, duration);
    let video_kbps = video_bps / 1000;

    let chain = encoder_chain(&app).await;
    let mut last_err = String::from("no encoders available");
    for enc in chain.iter() {
        let mut args: Vec<String> = vec![
            "-y".into(), "-hide_banner".into(),
            "-loglevel".into(), "error".into(),
            "-progress".into(), "pipe:1".into(), "-nostats".into(),
            "-ss".into(), format!("{:.6}", in_secs),
            "-to".into(), format!("{:.6}", out_secs),
            "-i".into(), src_path.clone(),
            "-map".into(), "0:v:0?".into(),
            "-map".into(), "0:a:0?".into(),
        ];
        args.extend(encoder_args_sized(enc, video_kbps));
        args.extend([
            "-c:a".into(), "aac".into(),
            "-b:a".into(), format!("{}k", SIZED_AUDIO_BPS / 1000),
            "-movflags".into(), "+faststart".into(),
            "-map_chapters".into(), "-1".into(),
            output_path.clone(),
        ]);
        match run_ffmpeg_with_progress(&app, args, duration, "export:progress").await {
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
async fn export_concat_sized(
    app: AppHandle,
    src_path: String,
    regions: Vec<(f64, f64)>,
    output_path: String,
    target_size_mb: f64,
) -> Result<(), String> {
    if regions.is_empty() {
        return Err("no regions to concat".into());
    }
    let total_duration: f64 = regions.iter().map(|(a, b)| (b - a).max(0.0)).sum();
    if total_duration < 0.05 {
        return Err("total duration too short".into());
    }
    let video_bps = target_video_bitrate_bps(target_size_mb, total_duration);
    let video_kbps = video_bps / 1000;

    // Write a concat list (forward slashes + escaped quotes for ffmpeg).
    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let list_file = temp_dir.join(format!("concat-sized-{}.txt", stamp));
    let normalized = src_path.replace('\\', "/");
    let escaped = normalized.replace('\'', "'\\''");
    let mut content = String::new();
    for (in_t, out_t) in &regions {
        content.push_str(&format!("file '{}'\n", escaped));
        content.push_str(&format!("inpoint {:.6}\n", in_t));
        content.push_str(&format!("outpoint {:.6}\n", out_t));
    }
    std::fs::write(&list_file, &content).map_err(|e| e.to_string())?;
    let list_str = list_file.to_string_lossy().to_string();

    let chain = encoder_chain(&app).await;
    let mut last_err = String::from("no encoders available");
    for enc in chain.iter() {
        let mut args: Vec<String> = vec![
            "-y".into(), "-hide_banner".into(),
            "-loglevel".into(), "error".into(),
            "-progress".into(), "pipe:1".into(), "-nostats".into(),
            "-f".into(), "concat".into(),
            "-safe".into(), "0".into(),
            "-i".into(), list_str.clone(),
            "-map".into(), "0:v:0?".into(),
            "-map".into(), "0:a:0?".into(),
            "-fflags".into(), "+genpts".into(),
        ];
        args.extend(encoder_args_sized(enc, video_kbps));
        args.extend([
            "-c:a".into(), "aac".into(),
            "-b:a".into(), format!("{}k", SIZED_AUDIO_BPS / 1000),
            "-movflags".into(), "+faststart".into(),
            "-map_chapters".into(), "-1".into(),
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

/// Concatenate N regions from the same source into a single output file via
/// the concat demuxer with stream-copy. Boundaries snap to source keyframes
/// (~1s with OBS keyframe=1). No re-encode, ~constant time regardless of total
/// region length.
#[tauri::command]
async fn export_concat(
    app: AppHandle,
    src_path: String,
    regions: Vec<(f64, f64)>,
    output_path: String,
) -> Result<(), String> {
    if regions.is_empty() {
        return Err("no regions to concat".into());
    }
    let total_duration: f64 = regions
        .iter()
        .map(|(a, b)| (b - a).max(0.0))
        .sum();
    if total_duration < 0.05 {
        return Err("total duration is too short to export".into());
    }

    // Write a temporary concat list file. Use forward slashes for FFmpeg parser
    // friendliness, and escape single quotes per the concat demuxer's grammar.
    let temp_dir = std::env::temp_dir().join("clippy");
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let list_file = temp_dir.join(format!("concat-{}.txt", stamp));
    let normalized = src_path.replace('\\', "/");
    let escaped = normalized.replace('\'', "'\\''");
    let mut content = String::new();
    for (in_t, out_t) in &regions {
        content.push_str(&format!("file '{}'\n", escaped));
        content.push_str(&format!("inpoint {:.6}\n", in_t));
        content.push_str(&format!("outpoint {:.6}\n", out_t));
    }
    std::fs::write(&list_file, &content).map_err(|e| e.to_string())?;
    let list_str = list_file.to_string_lossy().to_string();

    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-progress", "pipe:1",
            "-nostats",
            "-f", "concat",
            "-safe", "0",
            "-i", &list_str,
            "-map", "0:v:0?",
            "-map", "0:a:0?",
            "-c", "copy",
            "-map_chapters", "-1",
            "-fflags", "+genpts",
            &output_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    let start = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();
    let total_us = total_duration * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            if us > latest_us {
                                latest_us = us;
                            }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 150 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let _ = app.emit(
                        "export:progress",
                        ExportProgress {
                            progress,
                            elapsed_secs: start.elapsed().as_secs_f64(),
                        },
                    );
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                let _ = std::fs::remove_file(&list_file);
                if payload.code != Some(0) {
                    return Err(format!(
                        "ffmpeg concat exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                let _ = app.emit(
                    "export:progress",
                    ExportProgress {
                        progress: 1.0,
                        elapsed_secs: start.elapsed().as_secs_f64(),
                    },
                );
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

#[tauri::command]
async fn export_clip(
    app: AppHandle,
    src_path: String,
    in_secs: f64,
    out_secs: f64,
    output_path: String,
) -> Result<(), String> {
    let duration = (out_secs - in_secs).max(0.0);
    if duration < 0.05 {
        return Err("selection too short".into());
    }

    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-progress", "pipe:1",
            "-nostats",
            "-ss", &format!("{:.6}", in_secs),
            "-to", &format!("{:.6}", out_secs),
            "-i", &src_path,
            "-c", "copy",
            "-avoid_negative_ts", "make_zero",
            "-map", "0",
            "-map_chapters", "-1",
            &output_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    let start = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();
    let total_us = duration * 1_000_000.0;
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
                if last_emit.elapsed().as_millis() >= 150 {
                    let progress = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let _ = app.emit(
                        "export:progress",
                        ExportProgress {
                            progress,
                            elapsed_secs: start.elapsed().as_secs_f64(),
                        },
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
                        "ffmpeg exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                let _ = app.emit(
                    "export:progress",
                    ExportProgress {
                        progress: 1.0,
                        elapsed_secs: start.elapsed().as_secs_f64(),
                    },
                );
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Bind the listener synchronously so the port is known before any
            // frontend command can fire, then drive accept/serve on tokio.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.set_nonblocking(true)?;
            let port = listener.local_addr()?.port();
            let token = generate_session_token();
            let state = ServerState {
                token,
                allowlist: Arc::new(AsyncMutex::new(HashSet::new())),
            };
            app.manage(ServerInfo {
                port,
                state: state.clone(),
            });
            tauri::async_runtime::spawn(async move {
                let tokio_listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("[clippy] failed to convert listener: {}", e);
                        return;
                    }
                };
                let app = Router::new()
                    .route("/vid", get(serve_file))
                    .with_state(state);
                if let Err(e) = axum::serve(tokio_listener, app).await {
                    eprintln!("[clippy] media server stopped: {}", e);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            probe_video,
            generate_proxy,
            export_clip,
            export_clip_sized,
            export_concat,
            export_concat_sized,
            register_file_url,
            extract_waveform,
            file_size,
            reveal_in_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
