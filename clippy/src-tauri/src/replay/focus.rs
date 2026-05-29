//! Foreground window tracker.
//!
//! `SetWinEventHook` with `WINEVENT_OUTOFCONTEXT` requires the calling thread
//! to run a Win32 message loop — the OS dispatches hook callbacks via
//! `GetMessage`/`DispatchMessage`. We dedicate a worker thread for that pump,
//! and forward `EVENT_SYSTEM_FOREGROUND` events through an mpsc channel.

#[derive(Debug, Clone)]
pub struct FocusEvent {
    pub hwnd: isize,
    pub title: String,
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(windows)]
mod windows_impl {
    use super::FocusEvent;
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowTextLengthW, GetWindowTextW,
        PostThreadMessageW, TranslateMessage, EVENT_SYSTEM_FOREGROUND, MSG, WINEVENT_OUTOFCONTEXT,
        WINEVENT_SKIPOWNPROCESS, WM_QUIT,
    };

    /// Sender lives in a static so the C-callable hook proc can reach it.
    /// Wrapped in Mutex<Option<...>> so we can clear it on stop.
    fn event_tx_slot() -> &'static Mutex<Option<SyncSender<FocusEvent>>> {
        static SLOT: OnceLock<Mutex<Option<SyncSender<FocusEvent>>>> = OnceLock::new();
        SLOT.get_or_init(|| Mutex::new(None))
    }

    pub struct FocusMonitor {
        thread_id: u32,
        join_handle: Option<thread::JoinHandle<()>>,
    }

    impl FocusMonitor {
        /// Spawn the focus monitor thread. Returns a receiver for FocusEvents
        /// and a handle for stopping the monitor cleanly.
        pub fn start() -> Result<(Self, Receiver<FocusEvent>), String> {
            let (tx, rx) = mpsc::sync_channel::<FocusEvent>(32);

            // Install the sender for the hook callback to use.
            {
                let mut slot = event_tx_slot()
                    .lock()
                    .map_err(|e| format!("event slot lock: {e}"))?;
                if slot.is_some() {
                    return Err("focus monitor already running".into());
                }
                *slot = Some(tx.clone());
            }

            let (id_tx, id_rx) = mpsc::sync_channel::<u32>(1);

            let join_handle = thread::Builder::new()
                .name("clippy-focus-monitor".into())
                .spawn(move || run_message_pump(id_tx, tx))
                .map_err(|e| format!("spawn focus monitor: {e}"))?;

            let thread_id = id_rx
                .recv()
                .map_err(|_| "focus monitor died during init".to_string())?;

            // Synthesize an initial event so the coordinator can immediately
            // start a worker for whatever window is focused right now.
            unsafe {
                let h = GetForegroundWindow();
                if !h.0.is_null() {
                    let title = window_title(h);
                    let _ = event_tx_slot().lock().map(|g| {
                        g.as_ref().map(|t| {
                            t.try_send(FocusEvent {
                                hwnd: h.0 as isize,
                                title,
                            })
                        })
                    });
                }
            }

            Ok((
                FocusMonitor {
                    thread_id,
                    join_handle: Some(join_handle),
                },
                rx,
            ))
        }

        pub fn stop(mut self) {
            // Wake the message pump and let it tear down the hook.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(h) = self.join_handle.take() {
                let _ = h.join();
            }
            if let Ok(mut slot) = event_tx_slot().lock() {
                *slot = None;
            }
        }
    }

    impl Drop for FocusMonitor {
        fn drop(&mut self) {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(h) = self.join_handle.take() {
                let _ = h.join();
            }
            if let Ok(mut slot) = event_tx_slot().lock() {
                *slot = None;
            }
        }
    }

    fn run_message_pump(id_tx: SyncSender<u32>, _tx_kept_alive: SyncSender<FocusEvent>) {
        let tid = unsafe { GetCurrentThreadId() };
        let _ = id_tx.send(tid);

        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(win_event_proc),
                0, // all processes
                0, // all threads
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if hook.0.is_null() {
            return;
        }

        // Standard Win32 message loop — runs the hook callbacks.
        unsafe {
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWinEvent(hook);
        }
    }

    unsafe extern "system" fn win_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_obj: i32,
        _id_child: i32,
        _thread: u32,
        _time: u32,
    ) {
        // OBJID_WINDOW = 0; ignore non-window objects (menus, scrollbars, etc.)
        if event != EVENT_SYSTEM_FOREGROUND || id_obj != 0 || hwnd.0.is_null() {
            return;
        }
        let title = window_title(hwnd);
        let evt = FocusEvent {
            hwnd: hwnd.0 as isize,
            title,
        };
        if let Ok(slot) = event_tx_slot().lock() {
            if let Some(tx) = slot.as_ref() {
                let _ = tx.try_send(evt);
            }
        }
    }

    fn window_title(hwnd: HWND) -> String {
        unsafe {
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return String::new();
            }
            let mut buf = vec![0u16; (len as usize) + 1];
            let copied = GetWindowTextW(hwnd, &mut buf);
            String::from_utf16_lossy(&buf[..copied as usize])
        }
    }
}

#[cfg(not(windows))]
pub struct FocusMonitor;

#[cfg(not(windows))]
impl FocusMonitor {
    pub fn start() -> Result<(Self, std::sync::mpsc::Receiver<FocusEvent>), String> {
        Err("Windows only".into())
    }
}
