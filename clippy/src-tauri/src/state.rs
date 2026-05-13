use std::path::PathBuf;
use std::sync::Mutex;
use tauri::State;

/// Whether closing the main window should hide to the system tray instead
/// of exiting. Frontend keeps a localStorage mirror; this is the canonical
/// runtime copy the window-close handler reads.
pub struct HideOnClose(pub std::sync::atomic::AtomicBool);

#[tauri::command]
pub fn set_hide_on_close(state: tauri::State<'_, HideOnClose>, enabled: bool) {
    state.0.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

/// Where saved replays land. The coordinator's `finish_save` reads this on
/// every save. Default is `Videos/Clippy Replays` (computed at startup);
/// user can change it via `replay_set_save_dir` and the choice is persisted
/// to `<appdata>/save_dir.txt`.
pub struct ReplaySaveDir(pub Mutex<PathBuf>);

/// File path passed on the command line (Windows "Open with" or drag-on-exe
/// invokes us as `Clippy.exe "C:\path\video.mp4"`). Parsed once at startup,
/// then taken (cleared) by the frontend on first mount.
pub struct InitialPath(pub Mutex<Option<String>>);

pub(crate) fn parse_initial_path() -> Option<String> {
    const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "webm", "m4v", "avi"];
    for arg in std::env::args().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        let lower = arg.to_lowercase();
        let ext_ok = VIDEO_EXTS
            .iter()
            .any(|e| lower.ends_with(&format!(".{}", e)));
        if ext_ok && std::path::Path::new(&arg).is_file() {
            return Some(arg);
        }
    }
    None
}

#[tauri::command]
pub fn get_initial_path(state: State<'_, InitialPath>) -> Option<String> {
    state.0.lock().ok().and_then(|mut g| g.take())
}
