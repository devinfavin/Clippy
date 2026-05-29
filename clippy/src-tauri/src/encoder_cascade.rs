use std::sync::Mutex;
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

// Cached encoder that we've actually verified works on this machine (set after a successful pass).
pub(crate) static WORKING_ENCODER: Mutex<Option<&'static str>> = Mutex::new(None);

pub(crate) async fn encoder_chain(app: &AppHandle) -> Vec<&'static str> {
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
pub(crate) const SIZED_AUDIO_BPS: u64 = 96_000;

/// Encoder args for a fixed-bitrate (CBR-ish) re-encode targeting a specific
/// video kbps, used by the Discord-size export path.
pub(crate) fn encoder_args_sized(encoder: &str, video_kbps: u64) -> Vec<String> {
    let bv = format!("{}k", video_kbps);
    let maxrate = format!("{}k", video_kbps);
    let bufsize = format!("{}k", video_kbps * 2);
    match encoder {
        "h264_nvenc" => vec![
            "-c:v".into(),
            "h264_nvenc".into(),
            "-preset".into(),
            "p4".into(),
            "-tune".into(),
            "ll".into(),
            "-rc".into(),
            "cbr".into(),
            "-b:v".into(),
            bv,
            "-maxrate".into(),
            maxrate,
            "-bufsize".into(),
            bufsize,
        ],
        "h264_amf" => vec![
            "-c:v".into(),
            "h264_amf".into(),
            "-quality".into(),
            "speed".into(),
            "-rc".into(),
            "cbr".into(),
            "-b:v".into(),
            bv,
            "-maxrate".into(),
            maxrate,
            "-bufsize".into(),
            bufsize,
        ],
        "h264_qsv" => vec![
            "-c:v".into(),
            "h264_qsv".into(),
            "-preset".into(),
            "veryfast".into(),
            "-b:v".into(),
            bv,
            "-maxrate".into(),
            maxrate,
            "-bufsize".into(),
            bufsize,
        ],
        _ => vec![
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-b:v".into(),
            bv,
            "-maxrate".into(),
            maxrate,
            "-bufsize".into(),
            bufsize,
        ],
    }
}

/// Compute the video bitrate (bps) that should hit a target size in MB for a
/// given clip duration, leaving a small safety margin and reserving the audio
/// budget. Floors at 200 kbps to avoid outputs that look like a smear.
pub(crate) fn target_video_bitrate_bps(target_mb: f64, duration_secs: f64) -> u64 {
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

pub(crate) fn encoder_args(encoder: &str) -> Vec<&'static str> {
    match encoder {
        "h264_nvenc" => vec![
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p4",
            "-tune",
            "ll",
            "-rc",
            "vbr",
            "-cq",
            "28",
            "-b:v",
            "0",
        ],
        "h264_amf" => vec![
            "-c:v", "h264_amf", "-quality", "speed", "-rc", "cqp", "-qp_i", "28", "-qp_p", "28",
        ],
        "h264_qsv" => vec![
            "-c:v",
            "h264_qsv",
            "-preset",
            "veryfast",
            "-global_quality",
            "28",
        ],
        _ => vec!["-c:v", "libx264", "-preset", "veryfast", "-crf", "28"],
    }
}

/// High-quality re-encode (visually lossless-ish). Used for crop+no-limit
/// exports where the user expects the cropped output to look the same as the
/// original frame. CQ/CRF ~20 is the conventional "indistinguishable from
/// source" setting at typical screen-recording bitrates.
pub(crate) fn encoder_args_high_quality(encoder: &str) -> Vec<&'static str> {
    match encoder {
        "h264_nvenc" => vec![
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p5",
            "-rc",
            "vbr",
            "-cq",
            "20",
            "-b:v",
            "0",
        ],
        "h264_amf" => vec![
            "-c:v", "h264_amf", "-quality", "balanced", "-rc", "cqp", "-qp_i", "20", "-qp_p", "20",
        ],
        "h264_qsv" => vec![
            "-c:v",
            "h264_qsv",
            "-preset",
            "medium",
            "-global_quality",
            "20",
        ],
        _ => vec!["-c:v", "libx264", "-preset", "medium", "-crf", "20"],
    }
}
