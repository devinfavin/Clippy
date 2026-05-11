//! Process Loopback (Win11 22H2+): captures audio output of a single process
//! and its child process tree.
//!
//! `windows-rs` 0.58 doesn't auto-generate the `_Impl` trait for
//! `IActivateAudioInterfaceCompletionHandler`, so the macro `#[implement(...)]`
//! can't synthesize the COM plumbing. We hand-roll a minimal COM object —
//! a vtable + refcount + the one method the API actually calls — and hand
//! a raw pointer to `IActivateAudioInterfaceCompletionHandler::from_raw`.
//! All unsafe blast-radius is confined to this module.
//!
//! ## COM refcount audit (Phase 10, 2026-05-11)
//!
//! Refcount lifecycle, by branch:
//!
//! 1. `make_callback` allocates the object with refcount = 1 and transfers
//!    ownership to the returned `IActivateAudioInterfaceCompletionHandler`
//!    (via `from_raw`). That handle is the sole owner at this point.
//! 2. `ActivateAudioInterfaceAsync(&handler)` may internally AddRef to keep
//!    the callback alive past our stack frame. Post-call: refcount ≥ 2
//!    (1 ours, ≥ 1 the API's) on success; refcount = 1 (ours only) on err.
//! 3. End of `init_process_capture`: `handler` drops → Release. On success
//!    refcount becomes ≥ 1 (API's ref keeps the object alive). On err it
//!    becomes 0 → box dropped.
//! 4. When `ActivateCompleted` fires (async, possibly after we've returned),
//!    `cb_activate_completed` runs and posts to the channel. After it
//!    returns, the API calls Release on its own ref → refcount = 0 → box
//!    dropped.
//! 5. If we timed out and returned before completion, step 3 fired but the
//!    API still holds a ref. The callback fires later, `take()`s the sender
//!    (which is still `Some`), tries to send on a dropped rx (Err, ignored),
//!    then API releases → refcount = 0 → box dropped. No leak, no UAF.
//!
//! Edge cases verified:
//! - `cb_query_interface` AddRef's on success, leaves `*ppv` null on
//!   E_NOINTERFACE (no spurious ref). ✓
//! - `cb_release` only drops on `prev == 1` (the value BEFORE the decrement
//!   equals 1, i.e. count just hit 0). ✓
//! - `Send + Sync` impls are sound: every field is accessed atomically or
//!   under the `sender` mutex; the vtable pointer is to static memory. ✓
//! - `propvariant_bytes` references `&mut params` on the calling stack;
//!   the OS reads this synchronously during `ActivateAudioInterfaceAsync`
//!   (activation params are documented as in-call, not async). ✓
//!
//! Memory ordering: `cb_add_ref` uses Relaxed (the caller already holds a
//! valid ref, so no inter-thread synchronization is needed there).
//! `cb_release` uses Release on the decrement; the last releaser does an
//! Acquire fence before dropping so it sees all writes from earlier
//! releasers. This is the canonical refcount pattern (Boost.Atomic +
//! libcxx's `shared_ptr`). The previous SeqCst was correct but stricter
//! than needed.

#![cfg(windows)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IAudioCaptureClient, IAudioClient,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, WAVEFORMATEX,
};

use super::audio::AudioFormat;

// ---------- COM IIDs ----------

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_IAICCH: GUID = GUID::from_u128(0x41d949ab_9862_444a_80f6_c261334da5eb);

// ---------- Vtable + state ----------

#[repr(C)]
struct CallbackVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    activate_completed: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}

/// First field MUST be the vtable pointer so the COM client can read methods
/// at fixed offsets via `*this`.
#[repr(C)]
struct CallbackObj {
    vtbl: *const CallbackVtbl,
    refcount: AtomicUsize,
    sender: Mutex<Option<SyncSender<Result<IAudioClient, String>>>>,
}

// SAFETY: CallbackObj is only ever accessed through atomic refcount + Mutex.
unsafe impl Send for CallbackObj {}
unsafe impl Sync for CallbackObj {}

static VTBL: CallbackVtbl = CallbackVtbl {
    query_interface: cb_query_interface,
    add_ref: cb_add_ref,
    release: cb_release,
    activate_completed: cb_activate_completed,
};

unsafe extern "system" fn cb_query_interface(
    this: *mut c_void,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return HRESULT(0x80004003u32 as i32); // E_POINTER
    }
    let want = *riid;
    if want == IID_IUNKNOWN || want == IID_IAICCH {
        *ppv = this;
        cb_add_ref(this);
        HRESULT(0) // S_OK
    } else {
        *ppv = std::ptr::null_mut();
        HRESULT(0x80004002u32 as i32) // E_NOINTERFACE
    }
}

unsafe extern "system" fn cb_add_ref(this: *mut c_void) -> u32 {
    let obj = &*(this as *const CallbackObj);
    // Relaxed: the caller already owns a valid reference, so this fetch_add
    // doesn't need to publish writes or synchronize with anything yet.
    let prev = obj.refcount.fetch_add(1, Ordering::Relaxed);
    (prev + 1) as u32
}

unsafe extern "system" fn cb_release(this: *mut c_void) -> u32 {
    let obj = &*(this as *const CallbackObj);
    // Release: publishes any writes our thread did to the object before the
    // last releaser sees them. The Acquire fence below synchronizes with
    // every other thread's Release decrement so the destructor sees a
    // coherent view of the object.
    let prev = obj.refcount.fetch_sub(1, Ordering::Release);
    if prev == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this as *mut CallbackObj));
        0
    } else {
        (prev - 1) as u32
    }
}

unsafe extern "system" fn cb_activate_completed(
    this: *mut c_void,
    op_raw: *mut c_void,
) -> HRESULT {
    let obj = &*(this as *const CallbackObj);

    let result: Result<IAudioClient, String> = if op_raw.is_null() {
        Err("null IActivateAudioInterfaceAsyncOperation".into())
    } else {
        let op = IActivateAudioInterfaceAsyncOperation::from_raw_borrowed(&op_raw);
        match op {
            Some(op) => {
                let mut hr = HRESULT(0);
                let mut iface: Option<windows::core::IUnknown> = None;
                let _ = op.GetActivateResult(&mut hr, &mut iface);
                if hr.is_ok() {
                    iface
                        .ok_or_else(|| "no interface from activation".to_string())
                        .and_then(|u| u.cast::<IAudioClient>().map_err(|e| e.to_string()))
                } else {
                    Err(format!("activation HRESULT {:#x}", hr.0 as u32))
                }
            }
            None => Err("could not borrow async operation".into()),
        }
    };

    if let Ok(mut g) = obj.sender.lock() {
        if let Some(tx) = g.take() {
            let _ = tx.send(result);
        }
    }
    HRESULT(0)
}

/// Allocate a callback on the heap, return a handle the COM API can use.
/// The caller's IAudioClient receiver is wrapped inside; `ActivateCompleted`
/// posts the result to it.
fn make_callback(
    tx: SyncSender<Result<IAudioClient, String>>,
) -> IActivateAudioInterfaceCompletionHandler {
    let obj = Box::new(CallbackObj {
        vtbl: &VTBL as *const CallbackVtbl,
        // Start at 1: the IActivateAudioInterfaceCompletionHandler we hand out
        // owns this reference. ActivateAudioInterfaceAsync internally bumps
        // the refcount to keep its own; on completion (or destruction) it
        // releases. When all refs are gone the box gets dropped.
        refcount: AtomicUsize::new(1),
        sender: Mutex::new(Some(tx)),
    });
    let raw = Box::into_raw(obj) as *mut c_void;
    // SAFETY: `raw` points to a valid CallbackObj whose first field is a
    // CallbackVtbl with the IUnknown + IActivateAudioInterfaceCompletionHandler
    // method layout. windows-rs treats Interface wrappers as transparent
    // pointers, so from_raw just stores the pointer.
    unsafe { IActivateAudioInterfaceCompletionHandler::from_raw(raw) }
}

// ---------- Public entry point ----------

/// Activate Process Loopback for the given PID and return a fully initialized
/// (IAudioClient, IAudioCaptureClient, AudioFormat) triple ready for normal
/// loopback capture (same downstream code as endpoint loopback).
pub fn init_process_capture(
    pid: u32,
) -> Result<(IAudioClient, IAudioCaptureClient, AudioFormat), String> {
    // Activation params: include this process tree (so child renderers count
    // too — many games spawn child audio processes).
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };

    // PROPVARIANT (VT_BLOB) constructed as raw bytes — windows-rs's PROPVARIANT
    // type isn't in our feature set, but the C ABI is fixed so we build the
    // 24-byte layout directly: vt at offset 0, BLOB.cbSize at 8, BLOB.pBlobData at 16.
    let mut propvariant_bytes = [0u8; 24];
    unsafe {
        let raw = propvariant_bytes.as_mut_ptr();
        std::ptr::write_unaligned(raw as *mut u16, 65); // VT_BLOB
        std::ptr::write_unaligned(
            raw.add(8) as *mut u32,
            std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        );
        std::ptr::write_unaligned(
            raw.add(16) as *mut *mut u8,
            &mut params as *mut _ as *mut u8,
        );
    }

    // The activation is async; we block on a oneshot channel until our
    // callback fires (or 5 s timeout — Win10 / locked-down processes).
    let (tx, rx) = mpsc::sync_channel::<Result<IAudioClient, String>>(1);
    let handler = make_callback(tx);

    let path: Vec<u16> = "VAD\\Process_Loopback\0".encode_utf16().collect();
    // ActivateAudioInterfaceAsync (windows-rs 0.58) returns the operation;
    // we don't need the handle ourselves, the API holds onto our callback
    // until ActivateCompleted fires.
    let _op: IActivateAudioInterfaceAsyncOperation = unsafe {
        ActivateAudioInterfaceAsync(
            PCWSTR(path.as_ptr()),
            &IAudioClient::IID,
            Some(propvariant_bytes.as_ptr() as *const _),
            &handler,
        )
        .map_err(|e| format!("ActivateAudioInterfaceAsync: {e}"))?
    };

    let audio_client = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "process loopback activation timed out".to_string())??;

    // Process Loopback requires a fixed wave format per Microsoft's docs:
    // 16-bit PCM, 44.1 kHz, stereo. Other rates fail with E_INVALIDARG.
    let wave = WAVEFORMATEX {
        wFormatTag: 1, // WAVE_FORMAT_PCM
        nChannels: 2,
        nSamplesPerSec: 44_100,
        nAvgBytesPerSec: 44_100 * 2 * 2,
        nBlockAlign: 2 * 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    unsafe {
        audio_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                2_000_000,
                0,
                &wave,
                None,
            )
            .map_err(|e| format!("init process loopback client: {e}"))?;
    }

    let capture_client: IAudioCaptureClient = unsafe {
        audio_client
            .GetService()
            .map_err(|e| format!("get capture client: {e}"))?
    };

    Ok((
        audio_client,
        capture_client,
        AudioFormat {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            is_float: false,
        },
    ))
}
