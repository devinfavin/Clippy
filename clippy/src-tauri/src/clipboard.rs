use tauri::AppHandle;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

/// Copy a single frame at `time_secs` to the OS clipboard as a raster image.
/// Uses ffmpeg's rawvideo+rgba pipe so we don't need a PNG decoder in-process —
/// arboard takes raw RGBA bytes directly.
#[tauri::command]
pub async fn copy_frame_to_clipboard(
    app: AppHandle,
    src_path: String,
    time_secs: f64,
    width: u32,
    height: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("invalid source dimensions".into());
    }
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-ss", &format!("{:.6}", time_secs),
            "-i", &src_path,
            "-frames:v", "1",
            "-vsync", "0",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "pipe:1",
        ])
        .spawn()
        .map_err(|e| e.to_string())?;

    let expected_bytes = (width as usize) * (height as usize) * 4;
    let mut buf: Vec<u8> = Vec::with_capacity(expected_bytes);
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(b) => buf.extend_from_slice(&b),
            CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "frame extract failed (code {:?}): {}",
                        payload.code, stderr_buf
                    ));
                }
                break;
            }
            _ => {}
        }
    }
    if buf.len() != expected_bytes {
        return Err(format!(
            "raw RGBA pipe returned {} bytes, expected {}",
            buf.len(),
            expected_bytes
        ));
    }
    // arboard's set_image consumes the buffer. Done on a blocking task because
    // Windows clipboard APIs aren't async-friendly.
    let w = width as usize;
    let h = height as usize;
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        let img = arboard::ImageData {
            width: w,
            height: h,
            bytes: std::borrow::Cow::Owned(buf),
        };
        cb.set_image(img).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Save a single frame at `time_secs` as a PNG at source resolution.
#[tauri::command]
pub async fn export_frame_png(
    app: AppHandle,
    src_path: String,
    time_secs: f64,
    output_path: String,
) -> Result<(), String> {
    let sidecar = app.shell().sidecar("ffmpeg").map_err(|e| e.to_string())?;
    let (mut rx, _child) = sidecar
        .args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-ss", &format!("{:.6}", time_secs),
            "-i", &src_path,
            "-frames:v", "1",
            "-vsync", "0",
            "-q:v", "1",
            &output_path,
        ])
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stderr_buf = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stderr(b) => stderr_buf.push_str(&String::from_utf8_lossy(&b)),
            CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!(
                        "frame export failed (code {:?}): {}",
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
