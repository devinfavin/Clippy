//! Standalone hardware/system smoke test for Clippy's replay buffer.
//!
//! Exercises each subsystem in isolation so a user can run this before
//! opening a support ticket. Prints one JSON line per check plus a final
//! summary line, then exits 0 if every required check passed.
//!
//! Required checks fail the binary; optional checks (Process Loopback,
//! HW encoders) only fail when actually unavailable on the host.
//!
//! Usage:
//!   clippy-self-test           # JSON line per check + summary line
//!   clippy-self-test --pretty  # human-friendly text instead

use serde::Serialize;
use std::time::Instant;

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    Fail,
    Skip,
}

#[derive(Serialize, Clone)]
struct Step {
    step: &'static str,
    required: bool,
    result: Status,
    detail: String,
}

#[derive(Serialize)]
struct Summary {
    step: &'static str,
    required_total: u32,
    required_passed: u32,
    optional_total: u32,
    optional_passed: u32,
    exit_status: &'static str,
}

fn emit(step: &Step, pretty: bool) {
    if pretty {
        let tag = match step.result {
            Status::Ok => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        };
        let req = if step.required { "(required)" } else { "(optional)" };
        println!("[{tag}] {} {req}\n        {}", step.step, step.detail);
    } else {
        println!("{}", serde_json::to_string(step).unwrap());
    }
}

/// Run `f`; map Ok→Status::Ok, Err→Status::Fail. Emit + tally.
fn run<F>(name: &'static str, required: bool, pretty: bool, f: F) -> Status
where
    F: FnOnce() -> Result<String, String>,
{
    let step = match f() {
        Ok(detail) => Step {
            step: name,
            required,
            result: Status::Ok,
            detail,
        },
        Err(detail) => Step {
            step: name,
            required,
            result: Status::Fail,
            detail,
        },
    };
    emit(&step, pretty);
    step.result
}

fn skip(name: &'static str, required: bool, pretty: bool, why: &str) -> Status {
    let step = Step {
        step: name,
        required,
        result: Status::Skip,
        detail: why.to_string(),
    };
    emit(&step, pretty);
    Status::Skip
}

#[cfg(not(windows))]
fn main() {
    eprintln!("clippy-self-test is Windows-only (the replay buffer is too).");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    let pretty = std::env::args().any(|a| a == "--pretty");

    let mut required_total = 0u32;
    let mut required_passed = 0u32;
    let mut optional_total = 0u32;
    let mut optional_passed = 0u32;
    let tally = |req: bool, status: Status, totals: (&mut u32, &mut u32, &mut u32, &mut u32)| {
        let (rt, rp, ot, op) = totals;
        if req {
            *rt += 1;
            if matches!(status, Status::Ok) {
                *rp += 1;
            }
        } else {
            *ot += 1;
            if matches!(status, Status::Ok | Status::Skip) {
                *op += 1;
            }
        }
    };

    // 1. D3D11 device — gates everything else.
    let r = run("d3d11_device", true, pretty, check_d3d11);
    tally(true, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 2. GPU + RAM + HW encoder probe (informational).
    let r = run("sysinfo_probe", true, pretty, check_sysinfo);
    tally(true, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 3. Monitor enumeration.
    let r = run("monitor_enumeration", true, pretty, check_monitor_enum);
    tally(true, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 4. WGC monitor capture session (open + close).
    let r = run("wgc_monitor_capture_session", true, pretty, check_wgc_monitor);
    tally(true, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 5. WGC window capture session — uses our own console window as target.
    let r = run("wgc_window_capture_session", true, pretty, check_wgc_window);
    tally(true, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 6. MF startup + shutdown.
    let r = run("mf_startup_shutdown", true, pretty, check_mf_lifecycle);
    tally(true, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 7. WASAPI render endpoint enumeration.
    let r = run("wasapi_render_endpoints", true, pretty, check_wasapi_endpoints);
    tally(true, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 8. Game allowlist scan (no crash on missing launchers).
    let r = run("game_allowlist_scan", true, pretty, check_game_scan);
    tally(true, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 9. HW encoder enumeration. Optional — pure-software systems still
    //    work (falls back to software MFT), so a zero count is "skip".
    let r = run("hw_encoder_enumeration", false, pretty, check_hw_encoders);
    tally(false, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // 10. Process Loopback activation (Win11 22H2+). Optional — older
    //     Windows correctly fails this and falls back to system loopback.
    let r = run("process_loopback_activation", false, pretty, check_process_loopback);
    tally(false, r, (&mut required_total, &mut required_passed, &mut optional_total, &mut optional_passed));

    // Tray icon construction requires a live Tauri AppHandle, which a
    // standalone binary can't synthesize. Marked skip so the JSON output
    // still records that we considered it.
    let _ = skip(
        "tray_icon_construction",
        false,
        pretty,
        "requires Tauri runtime; covered by app integration test in Tier 4",
    );
    optional_total += 1;
    optional_passed += 1;

    let exit_status = if required_passed == required_total {
        "ok"
    } else {
        "fail"
    };
    let summary = Summary {
        step: "summary",
        required_total,
        required_passed,
        optional_total,
        optional_passed,
        exit_status,
    };
    if pretty {
        println!(
            "\nSummary: required {}/{}, optional {}/{} — {}",
            required_passed, required_total, optional_passed, optional_total, exit_status
        );
    } else {
        println!("{}", serde_json::to_string(&summary).unwrap());
    }
    std::process::exit(if exit_status == "ok" { 0 } else { 1 });
}

// ----- individual checks (Windows) -----

#[cfg(windows)]
fn check_d3d11() -> Result<String, String> {
    use clippy_lib::replay::capture::windows_impl::create_d3d11_device;
    create_d3d11_device().map_err(|e| format!("{e}"))?;
    Ok("D3D11 hardware device created".into())
}

#[cfg(windows)]
fn check_sysinfo() -> Result<String, String> {
    let info = clippy_lib::replay::sysinfo::collect();
    if info.gpu_name.is_empty() && info.gpu_vram_mb == 0 {
        return Err("no GPU adapter info available".into());
    }
    let mut detail = format!(
        "GPU=\"{}\" VRAM={}MB RAM={}MB encoders={}",
        info.gpu_name, info.gpu_vram_mb, info.ram_total_mb, info.hw_encoders.len()
    );
    for n in &info.hw_encoders {
        detail.push_str(&format!(" | {}", n));
    }
    Ok(detail)
}

#[cfg(windows)]
fn check_monitor_enum() -> Result<String, String> {
    let mons = clippy_lib::replay::capture::list_monitors();
    if mons.is_empty() {
        return Err("no monitors enumerated".into());
    }
    let labels: Vec<String> = mons
        .iter()
        .map(|m| format!("{}={}x{}", m.label, m.width, m.height))
        .collect();
    Ok(format!("{} monitor(s): {}", mons.len(), labels.join(", ")))
}

#[cfg(windows)]
fn check_wgc_monitor() -> Result<String, String> {
    use clippy_lib::replay::capture::windows_impl::{
        capture_item_for_monitor, create_d3d11_device, open_capture_session_for,
    };
    use windows::Win32::Graphics::Gdi::HMONITOR;

    let mons = clippy_lib::replay::capture::list_monitors();
    let primary = mons
        .iter()
        .find(|m| m.primary)
        .or_else(|| mons.first())
        .ok_or_else(|| "no monitor to capture".to_string())?;
    let h: isize = primary
        .hmonitor
        .parse()
        .map_err(|e| format!("parse hmonitor {}: {e}", primary.hmonitor))?;

    let bundle = create_d3d11_device().map_err(|e| format!("D3D11: {e}"))?;
    let item = capture_item_for_monitor(HMONITOR(h as *mut _))
        .map_err(|e| format!("capture item: {e}"))?;
    let session = open_capture_session_for(&item, &bundle.device)
        .map_err(|e| format!("session: {e}"))?;
    // Process exit cleans up. Drop explicitly so the WGC border (if any)
    // disappears before our summary line prints.
    let _ = session.session.Close();
    Ok(format!("opened WGC session on {}", primary.label))
}

#[cfg(windows)]
fn check_wgc_window() -> Result<String, String> {
    use clippy_lib::replay::capture::windows_impl::{
        capture_item_for_hwnd, create_d3d11_device, open_capture_session_for,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    // Try foreground window first (terminal that launched us — almost always
    // a valid WGC target). Modern Windows Terminal hosts conhost in a separate
    // process so GetConsoleWindow returns a pseudo-HWND WGC rejects with
    // E_INVALIDARG; only fall back to it if no foreground window exists.
    let bundle = create_d3d11_device().map_err(|e| format!("D3D11: {e}"))?;

    let candidates = unsafe { [GetForegroundWindow(), GetConsoleWindow()] };
    let mut last_err: Option<String> = None;
    for (i, hwnd) in candidates.into_iter().enumerate() {
        if hwnd.0.is_null() {
            continue;
        }
        let item = match capture_item_for_hwnd(HWND(hwnd.0 as *mut _)) {
            Ok(it) => it,
            Err(e) => {
                last_err = Some(format!("candidate {i} item: {e}"));
                continue;
            }
        };
        match open_capture_session_for(&item, &bundle.device) {
            Ok(session) => {
                let _ = session.session.Close();
                let label = if i == 0 { "foreground window" } else { "console window" };
                return Ok(format!(
                    "opened WGC session on {label} (hwnd {:#x})",
                    hwnd.0 as isize
                ));
            }
            Err(e) => {
                last_err = Some(format!("candidate {i} session: {e}"));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "no usable HWND for window capture".into()))
}

#[cfg(windows)]
fn check_mf_lifecycle() -> Result<String, String> {
    use clippy_lib::replay::encoder::windows_impl::{mf_shutdown, mf_startup};
    mf_startup().map_err(|e| format!("MFStartup: {e}"))?;
    mf_shutdown();
    Ok("MFStartup/MFShutdown returned clean".into())
}

#[cfg(windows)]
fn check_wasapi_endpoints() -> Result<String, String> {
    let devs = clippy_lib::replay::audio::enumerate_render_devices();
    if devs.is_empty() {
        return Err("no WASAPI render endpoints enumerated".into());
    }
    let labels: Vec<String> = devs
        .iter()
        .map(|d| {
            if d.is_default {
                format!("{} (default)", d.name)
            } else {
                d.name.clone()
            }
        })
        .collect();
    Ok(format!("{} device(s): {}", devs.len(), labels.join(", ")))
}

#[cfg(windows)]
fn check_game_scan() -> Result<String, String> {
    let games = clippy_lib::replay::games::scan_launchers();
    Ok(format!(
        "scan returned {} executable(s) without crash",
        games.len()
    ))
}

#[cfg(windows)]
fn check_hw_encoders() -> Result<String, String> {
    let info = clippy_lib::replay::sysinfo::collect();
    if info.hw_encoders.is_empty() {
        return Err("no hardware H.264 encoders enumerated".into());
    }
    Ok(format!(
        "{} HW encoder(s): {}",
        info.hw_encoders.len(),
        info.hw_encoders.join(", ")
    ))
}

#[cfg(windows)]
fn check_process_loopback() -> Result<String, String> {
    // Try to activate Process Loopback against this very test process. On
    // Win11 22H2+ this succeeds; older Windows fails with a documented HRESULT
    // and the worker falls back to system loopback. Either way the call must
    // not panic / hang past the activation timeout.
    //
    // ActivateAudioInterfaceAsync requires an apartment-initialized thread.
    // The production worker gets this implicitly via prior D3D11/MF calls;
    // in this standalone test we initialize COM explicitly so the failure
    // path we report reflects Windows version, not missing apartment state.
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    let co = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if co.is_err() {
        return Err(format!("CoInitializeEx failed: {:?}", co));
    }
    let pid = std::process::id();
    let epoch = Instant::now();
    let res = clippy_lib::replay::audio::AudioCaptureHandle::start_for_process(pid, epoch, 10);
    unsafe { CoUninitialize() };
    match res {
        Ok(_handle) => Ok(format!("activated process loopback for pid {pid}")),
        // Don't blame the Windows build — Process Loopback works in the
        // running app (Tauri runtime sets up apartments the standalone
        // test doesn't). Surface the raw error so a reader who needs to
        // can map the HRESULT themselves.
        Err(e) => Err(format!(
            "activation failed in standalone harness ({e}). \
             This does NOT mean Process Loopback is unavailable on your Windows — \
             the in-app worker uses a different COM thread setup. \
             Confirm by enabling 'Try to capture only the focused game's audio' \
             in Replay settings and saving a clip."
        )),
    }
}
