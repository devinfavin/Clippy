//! Unified ffmpeg sidecar runner.
//!
//! One spawn/progress/stderr/termination loop, replacing the ~7 hand-rolled
//! copies that used to live across `export.rs` and `proxy.rs`. Callers build
//! the arg vector (unchanged from before) and supply an `on_progress`
//! callback; the runner owns the `-progress pipe:1` parsing, stderr capture,
//! and diag logging — including the full-stderr failure log that the old
//! `export.rs` copy had but never reached (a dead early-return; finding F2).

use tauri::AppHandle;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use crate::diag::diag;
use crate::helpers::{basename, trunc};

/// One-liner summary of an ffmpeg invocation for diag entries: output
/// basename (never the full path — privacy), codec/bitrate flags, and whether
/// a `-filter_complex` graph was used (+ its size). Used in START/OK/FAILED
/// log lines so a support report shows *what was attempted*.
pub(crate) fn summarize_ffmpeg_invocation(args: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(last) = args.last() {
        parts.push(format!("out={}", basename(last)));
    }
    let mut i = 0;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "-c:v" | "-c:a" | "-b:v" | "-b:a" | "-preset" | "-crf" | "-f" | "-pix_fmt" => {
                let key = args[i].trim_start_matches('-');
                parts.push(format!("{key}={}", args[i + 1]));
                i += 2;
            }
            "-vn" => {
                parts.push("audio-only".into());
                i += 1;
            }
            "-filter_complex" => {
                parts.push(format!("filter_complex({}B)", args[i + 1].len()));
                i += 2;
            }
            _ => i += 1,
        }
    }
    if parts.is_empty() {
        "ffmpeg".into()
    } else {
        parts.join(" ")
    }
}

/// Spawn `ffmpeg` (the bundled sidecar) with `args`, driving it to completion.
///
/// * Parses `out_time_us=` lines (present only when the caller passed
///   `-progress pipe:1`) and invokes `on_progress(fraction_0_to_1,
///   elapsed_secs)` at ~150 ms cadence, then once more with `1.0` on success.
///   Sites that don't want progress pass `|_, _| {}`.
/// * Captures stderr; on a non-zero exit, logs the full stderr (per line,
///   truncated) to the diag log under `[{label}]` and returns it in the `Err`.
/// * `label` prefixes the diag lines ("export", "concat", "cut", "proxy",
///   "remux", …) so a copied log says which stage ran.
/// * `total_secs` is the expected output duration, used only for the progress
///   fraction; pass `0.0` to disable the fraction (callback still fires `1.0`
///   on success).
pub(crate) async fn run_ffmpeg(
    app: &AppHandle,
    label: &str,
    args: Vec<String>,
    total_secs: f64,
    mut on_progress: impl FnMut(f64, f64) + Send,
) -> Result<(), String> {
    let summary = summarize_ffmpeg_invocation(&args);
    diag(
        app,
        format!("[{label}] START · {summary} · {total_secs:.2}s"),
    );

    let sidecar = match app.shell().sidecar("ffmpeg") {
        Ok(s) => s,
        Err(e) => {
            diag(
                app,
                format!("[{label}] FAILED · {summary} · sidecar lookup: {e}"),
            );
            return Err(e.to_string());
        }
    };
    let (mut rx, _child) = match sidecar.args(args).spawn() {
        Ok(t) => t,
        Err(e) => {
            diag(app, format!("[{label}] FAILED · {summary} · spawn: {e}"));
            return Err(e.to_string());
        }
    };

    let start = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();
    let total_us = total_secs * 1_000_000.0;
    let mut latest_us: f64 = 0.0;
    let mut stderr_buf = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line_bytes) => {
                let line = String::from_utf8_lossy(&line_bytes);
                for part in line.split('\n') {
                    if let Some(rest) = part.trim().strip_prefix("out_time_us=") {
                        if let Ok(us) = rest.parse::<f64>() {
                            // Encoder pipelining can report timestamps out of
                            // order; clamp to monotonic so the % doesn't bounce.
                            if us > latest_us {
                                latest_us = us;
                            }
                        }
                    }
                }
                if last_emit.elapsed().as_millis() >= 150 {
                    let frac = if total_us > 0.0 {
                        (latest_us / total_us).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    on_progress(frac, start.elapsed().as_secs_f64());
                    last_emit = std::time::Instant::now();
                }
            }
            CommandEvent::Stderr(line_bytes) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line_bytes));
            }
            CommandEvent::Terminated(payload) => {
                let elapsed = start.elapsed().as_secs_f64();
                if payload.code != Some(0) {
                    // F2: this detailed failure log now actually runs — the old
                    // export.rs copy guarded it behind an earlier `return Err`,
                    // so export failures never logged their stderr chain.
                    diag(
                        app,
                        format!(
                            "[{label}] FAILED · {summary} · exit={:?} after {elapsed:.2}s · stderr:\n{}",
                            payload.code,
                            stderr_buf
                                .lines()
                                .map(|l| format!("    {}", trunc(l, 240)))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                    );
                    return Err(format!(
                        "ffmpeg exited with code {:?}: {}",
                        payload.code, stderr_buf
                    ));
                }
                if stderr_buf.trim().is_empty() {
                    diag(app, format!("[{label}] OK · {summary} · {elapsed:.2}s"));
                } else {
                    diag(
                        app,
                        format!(
                            "[{label}] OK · {summary} · {elapsed:.2}s · stderr notes:\n{}",
                            stderr_buf
                                .lines()
                                .take(8)
                                .map(|l| format!("    {}", trunc(l, 240)))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                    );
                }
                on_progress(1.0, elapsed);
                break;
            }
            _ => {}
        }
    }
    Ok(())
}
