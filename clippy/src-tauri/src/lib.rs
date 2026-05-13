pub mod clipboard;
pub mod diag;
pub mod encoder_cascade;
pub mod export;
pub mod extract;
pub mod helpers;
pub mod media_server;
pub mod probe;
pub mod project;
pub mod proxy;
pub mod replay;
pub mod state;
pub mod storage;

// Re-exports so `crate::diag`, `crate::ReplaySaveDir`, etc. keep resolving
// from `replay::*` and other call sites that reach in by the original path.
// (Tauri-command items are NOT re-exported — the macro entries below resolve
// each command from its new module path.)
pub use diag::{diag, epoch_to_ymd_for_filename, DiagLog, DiagVerbose};
pub use state::{HideOnClose, InitialPath, ReplaySaveDir};

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use axum::routing::get;
use axum::Router;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex as AsyncMutex;

/// (Re)register the global save-replay hotkey.
///
/// `shortcut_str` is the Tauri `Shortcut::from_str` format ("Alt+F10",
/// "Ctrl+Shift+S", etc.). Any previously-registered global shortcut is removed
/// first so this is safe to call repeatedly when the user rebinds.
fn register_save_hotkey(app: &AppHandle, shortcut_str: &str) -> Result<(), String> {
    use std::str::FromStr;
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

    let parsed = Shortcut::from_str(shortcut_str)
        .map_err(|e| format!("parse '{shortcut_str}': {e}"))?;

    // We only ever have the one global shortcut for now — clear the slate.
    let _ = app.global_shortcut().unregister_all();

    let app_handle = app.clone();
    let shortcut_label = shortcut_str.to_string();
    app.global_shortcut()
        .on_shortcut(parsed, move |_app, _sc, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let handle = app_handle.clone();
            // Surface that the OS actually delivered the keypress so we
            // can tell "hotkey didn't register" apart from "hotkey fired
            // but save flow choked" when triaging silent-failure reports.
            diag(
                &handle,
                format!("[replay] save hotkey FIRED ({shortcut_label}) — invoking save"),
            );
            tauri::async_runtime::spawn(async move {
                match replay::save_active(&handle).await {
                    Ok(result) => {
                        diag(
                            &handle,
                            format!(
                                "replay save OK — window=\"{}\" → {}",
                                result.window_title, result.path
                            ),
                        );
                        let _ = handle.emit("replay://saved", &result.path);
                    }
                    Err(e) => {
                        diag(&handle, format!("replay save FAILED: {e}"));
                        let _ = handle.emit("replay://save-error", e);
                    }
                }
            });
        })
        .map_err(|e| format!("register: {e}"))?;
    Ok(())
}

/// Tauri command: rebind the save-replay global hotkey.
#[tauri::command]
fn replay_set_save_hotkey(app: AppHandle, shortcut: String) -> Result<(), String> {
    register_save_hotkey(&app, &shortcut)?;
    diag(&app, format!("[replay] save hotkey rebound to {shortcut}"));
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // Intercept the X button so we can hide-to-tray instead of exit when
        // the user opted in (keeps the replay buffer running in background).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let hide = app
                    .state::<HideOnClose>()
                    .0
                    .load(std::sync::atomic::Ordering::SeqCst);
                if hide {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // No CLI args — Clippy autodetects state from saved settings.
            None,
        ))
        // Self-updating: checks the configured endpoint for a newer signed
        // installer; the frontend drives check/download/install via the JS
        // plugin. `process` is needed so the app can restart itself after
        // installing the update.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            app.manage(DiagLog::new());
            app.manage(InitialPath(Mutex::new(state::parse_initial_path())));
            app.manage(replay::ReplayState::new());
            app.manage(HideOnClose(std::sync::atomic::AtomicBool::new(false)));
            app.manage(DiagVerbose(std::sync::atomic::AtomicBool::new(false)));

            // Replay save dir — load persisted preference if present, otherwise
            // compute the default and create it on disk so the user's first
            // save doesn't fail on a missing folder.
            {
                let handle = app.handle();
                let save_dir = replay::load_save_dir(&handle.clone());
                if let Err(e) = std::fs::create_dir_all(&save_dir) {
                    eprintln!(
                        "[clippy] couldn't create replay save dir {}: {e}",
                        save_dir.display()
                    );
                }
                app.manage(ReplaySaveDir(Mutex::new(save_dir)));
            }

            // System tray icon. Left-click shows the window; menu has Show + Quit.
            // Combined with the close-to-tray setting, lets the user keep the
            // replay buffer running in the background after closing the window.
            {
                use tauri::menu::{Menu, MenuEvent, MenuItem};
                use tauri::tray::{
                    MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
                };

                let show_item = MenuItem::with_id(app, "tray-show", "Show Clippy", true, None::<&str>)?;
                let quit_item = MenuItem::with_id(app, "tray-quit", "Quit Clippy", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

                let on_menu = |app: &AppHandle, event: MenuEvent| {
                    match event.id().as_ref() {
                        "tray-show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                            }
                        }
                        "tray-quit" => app.exit(0),
                        _ => {}
                    }
                };
                let on_icon = |tray: &tauri::tray::TrayIcon, event: TrayIconEvent| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    }
                };

                if let Some(icon) = app.default_window_icon() {
                    let _ = TrayIconBuilder::new()
                        .icon(icon.clone())
                        .tooltip("Clippy")
                        .menu(&menu)
                        .show_menu_on_left_click(false)
                        .on_menu_event(on_menu)
                        .on_tray_icon_event(on_icon)
                        .build(app);
                }
            }

            #[cfg(debug_assertions)]
            if let Some(win) = app.get_webview_window("main") {
                win.open_devtools();
            }

            // Global hotkey for "save replay buffer" — defaults to Alt+F10.
            // Re-registered at runtime by `replay_set_save_hotkey` when the
            // user rebinds via the keybind editor.
            if let Err(e) = register_save_hotkey(&app.handle().clone(), "Alt+F10") {
                eprintln!("[clippy] save hotkey init failed: {e}");
            }

            // Auto-prune cache files >30 days untouched. Background thread so a
            // slow disk doesn't block startup. Manual "Clear cache" is exposed
            // via the clear_cache command for users who want it now.
            if let Ok(data_dir) = app.path().app_data_dir() {
                let proxies = data_dir.join("proxies");
                std::thread::spawn(move || storage::prune_old_cache(proxies, 30));
            }

            // Bind the listener synchronously so the port is known before any
            // frontend command can fire, then drive accept/serve on tokio.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.set_nonblocking(true)?;
            let port = listener.local_addr()?.port();
            let token = media_server::generate_session_token();
            let server_state = media_server::ServerState {
                token,
                port,
                allowlist: Arc::new(AsyncMutex::new(HashSet::new())),
            };
            app.manage(media_server::ServerInfo {
                port,
                state: server_state.clone(),
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
                    .route("/vid", get(media_server::serve_file).options(media_server::serve_options))
                    .with_state(server_state);
                if let Err(e) = axum::serve(tokio_listener, app).await {
                    eprintln!("[clippy] media server stopped: {}", e);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            probe::probe_video,
            proxy::generate_proxy,
            export::export_clip,
            export::export_clip_sized,
            export::export_concat,
            export::export_concat_sized,
            export::export_clip_audio,
            export::export_concat_audio,
            media_server::register_file_url,
            probe::extract_waveform,
            probe::probe_keyframes,
            storage::file_size,
            storage::reveal_in_folder,
            state::get_initial_path,
            storage::cache_size,
            storage::clear_cache,
            export::export_clip_gif,
            export::export_concat_gif,
            clipboard::export_frame_png,
            clipboard::copy_frame_to_clipboard,
            project::load_project,
            project::save_project,
            extract::extract_track,
            diag::get_diagnostics,
            replay::commands::get_replay_status,
            replay::commands::replay_start,
            replay::commands::replay_stop,
            replay::commands::replay_save,
            replay::commands::replay_list_monitors,
            replay::commands::replay_list_audio_devices,
            replay::commands::replay_set_audio_names,
            replay::commands::replay_get_system_info,
            replay::commands::replay_list_games,
            replay::commands::replay_rescan_games,
            replay::commands::replay_add_game,
            replay::commands::replay_add_current_game,
            replay::commands::replay_remove_game,
            replay::commands::replay_recent_games,
            replay::commands::replay_get_save_dir,
            replay::commands::replay_set_save_dir,
            replay::commands::replay_reset_save_dir,
            storage::storage_summary,
            diag::clear_diagnostics_log,
            replay_set_save_hotkey,
            state::set_hide_on_close,
            diag::set_diag_verbose,
            // PoC pipeline-validation commands are dev-only — gated behind
            // the `poc` cargo feature so a default release build doesn't
            // expose them. Build `cargo build --features poc` (or invoke
            // the matching `pnpm tauri dev` recipe) to enable them.
            #[cfg(feature = "poc")]
            replay::poc::replay_poc_test,
            #[cfg(feature = "poc")]
            replay::poc::replay_poc_gpu_convert,
            #[cfg(feature = "poc")]
            replay::poc::replay_poc_gpu_full
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Persist the in-memory diag log on graceful exit so a problem
            // report after the fact still has the most-recent ~200 events.
            // Hard crashes that bypass RunEvent::Exit will lose this round.
            if let tauri::RunEvent::Exit = event {
                diag::persist_diag_log(app);
            }
        });
}
