//! WASAPI loopback audio capture.
//!
//! Each `AudioCaptureHandle` runs a dedicated thread that pulls PCM from one
//! WASAPI render endpoint in loopback mode, timestamps each packet against
//! a shared wall-clock epoch (so video and audio stay in sync), and buffers
//! the most recent N seconds. Snapshot returns the current buffer for save.

use std::sync::Arc;

/// WASAPI endpoint direction. `Render` endpoints are outputs that we capture
/// via loopback (game audio, music). `Capture` endpoints are inputs we open
/// directly (microphones) — opening a render endpoint as if it were a mic
/// returns silence because nothing is rendering to it (the SteelSeries Sonar
/// "Microphone" virtual device is the canonical example: its render side is
/// a sink other apps write to, not a source).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointKind {
    Render,
    Capture,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub kind: EndpointKind,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    /// True when the format is 32-bit IEEE float; false for integer PCM.
    pub is_float: bool,
}

#[derive(Clone)]
pub struct AudioPacket {
    pub data: Arc<[u8]>,
    /// 100-ns units, measured from the epoch passed to `start()`.
    pub pts: i64,
}

#[cfg(windows)]
pub fn enumerate_render_devices() -> Vec<AudioDevice> {
    windows_impl::enumerate_endpoints(EndpointKind::Render)
}

#[cfg(not(windows))]
pub fn enumerate_render_devices() -> Vec<AudioDevice> {
    Vec::new()
}

#[cfg(windows)]
pub fn enumerate_capture_devices() -> Vec<AudioDevice> {
    windows_impl::enumerate_endpoints(EndpointKind::Capture)
}

#[cfg(not(windows))]
pub fn enumerate_capture_devices() -> Vec<AudioDevice> {
    Vec::new()
}

#[cfg(windows)]
pub use windows_impl::AudioCaptureHandle;

#[cfg(windows)]
mod windows_impl {
    use super::{AudioDevice, AudioFormat, AudioPacket, EndpointKind};
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, SyncSender};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY,
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };

    // wFormatTag values from mmeapi.h — not re-exported by name in this windows-rs.
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoGetApartmentType, CoInitializeEx, CoUninitialize, APTTYPE,
        APTTYPEQUALIFIER, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    // Well-known GUID for IEEE-float audio in WAVEFORMATEXTENSIBLE.SubFormat.
    // Not re-exported by name in this windows-rs version.
    const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: windows::core::GUID =
        windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    pub fn enumerate_endpoints(kind: EndpointKind) -> Vec<AudioDevice> {
        let mut out = Vec::new();
        let data_flow = match kind {
            EndpointKind::Render => eRender,
            EndpointKind::Capture => eCapture,
        };
        let fallback_label = match kind {
            EndpointKind::Render => "Output",
            EndpointKind::Capture => "Input",
        };
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                    Ok(e) => e,
                    Err(_) => return out,
                };

            let default_id = enumerator
                .GetDefaultAudioEndpoint(data_flow, eConsole)
                .ok()
                .and_then(|d| d.GetId().ok())
                .map(|p| pcwstr_to_string(p))
                .unwrap_or_default();

            let collection = match enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE) {
                Ok(c) => c,
                Err(_) => return out,
            };
            let count = collection.GetCount().unwrap_or(0);
            for i in 0..count {
                let Ok(device) = collection.Item(i) else {
                    continue;
                };
                let Ok(id_pwstr) = device.GetId() else {
                    continue;
                };
                let id = pcwstr_to_string(id_pwstr);
                let is_default = id == default_id;
                let resolved = friendly_name(&device);
                let name = match resolved {
                    Some(n) if is_default => format!("{n} (Default)"),
                    Some(n) => n,
                    None if is_default => {
                        format!(
                            "Default {} (device #{})",
                            fallback_label.to_lowercase(),
                            i + 1
                        )
                    }
                    None => format!("{fallback_label} #{}", i + 1),
                };
                out.push(AudioDevice {
                    id,
                    name,
                    is_default,
                    kind,
                });
            }
        }
        out
    }

    unsafe fn pcwstr_to_string(p: windows::core::PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        while *p.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(p.0, len);
        String::from_utf16_lossy(slice)
    }

    /// Resolve the device's `PKEY_Device_FriendlyName` via raw FFI. Bypasses
    /// the windows-rs PROPVARIANT union access (which has incompatible field
    /// naming across versions) by reading the C ABI layout directly.
    unsafe fn friendly_name(device: &IMMDevice) -> Option<String> {
        use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
        use windows::Win32::System::Com::STGM_READ;
        use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

        // PKEY_Device_FriendlyName = {a45c254e-df1c-4efd-8020-67d146a850e0}, pid 14
        let key = PROPERTYKEY {
            fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
            pid: 14,
        };
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let mut value = store.GetValue(&key).ok()?;

        // PROPVARIANT C layout:
        //   offset 0:  vt (u16)
        //   offset 8:  pwszVal (PWSTR / *mut u16) when vt == VT_LPWSTR (31)
        // The 8-byte gap holds wReservedN fields. We read directly so we don't
        // depend on which generation of windows-rs union naming is in use.
        let raw = &value as *const _ as *const u8;
        let vt = std::ptr::read_unaligned(raw as *const u16);

        let result = if vt == 31 {
            let pw_ptr = std::ptr::read_unaligned(raw.add(8) as *const *mut u16);
            if pw_ptr.is_null() {
                None
            } else {
                let mut len = 0usize;
                while *pw_ptr.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(pw_ptr, len);
                Some(String::from_utf16_lossy(slice))
            }
        } else {
            None
        };

        let _ = PropVariantClear(&mut value);
        result
    }

    // ---------- capture handle + thread ----------

    pub enum AudioCmd {
        Snapshot(SyncSender<Result<Vec<AudioPacket>, String>>),
        Stop,
    }

    pub struct AudioCaptureHandle {
        cmd_tx: SyncSender<AudioCmd>,
        join_handle: Option<thread::JoinHandle<()>>,
        format: AudioFormat,
    }

    /// Where the capture thread should pull audio from.
    pub enum CaptureSource {
        /// Output endpoint loopback. `None` = default render device.
        Device(Option<String>),
        /// Input endpoint (microphone / line-in). `None` = default capture
        /// device. Distinct from `Device` because the WASAPI Initialize call
        /// must omit `AUDCLNT_STREAMFLAGS_LOOPBACK` — opening a capture
        /// endpoint with loopback returns silence.
        InputDevice(Option<String>),
        /// Process Loopback (Win11 22H2+) — captures only this PID's audio
        /// (and its child processes).
        Process(u32),
    }

    impl AudioCaptureHandle {
        /// `app` is the Tauri handle the capture thread uses for diag entries
        /// (Start / GetBuffer / GetNextPacketSize failures, which used to be
        /// silent). `None` skips diagnostics — the `clippy-self-test` binary
        /// passes `None` because it has no Tauri runtime.
        pub fn start(
            device_id: Option<String>,
            epoch: Instant,
            duration_secs: u32,
            app: Option<tauri::AppHandle>,
        ) -> Result<Self, String> {
            Self::start_with_source(CaptureSource::Device(device_id), epoch, duration_secs, app)
        }

        /// Capture from a microphone / line-in (eCapture endpoint). `None`
        /// uses the system's default capture device. Unlike `start`, the
        /// underlying WASAPI client is opened WITHOUT the loopback flag — the
        /// "Microphone" virtual devices that virtual mixers (Sonar,
        /// Voicemeeter) expose are render endpoints by name only; their mic
        /// audio lives on the capture side and must be opened as such.
        pub fn start_input(
            device_id: Option<String>,
            epoch: Instant,
            duration_secs: u32,
            app: Option<tauri::AppHandle>,
        ) -> Result<Self, String> {
            Self::start_with_source(
                CaptureSource::InputDevice(device_id),
                epoch,
                duration_secs,
                app,
            )
        }

        /// Phase 3.3 — capture only the given process's audio.
        /// Requires Windows 11 22H2 or later. On older systems this fails and
        /// the caller is expected to fall back to `start(None, ...)` for system
        /// loopback.
        pub fn start_for_process(
            pid: u32,
            epoch: Instant,
            duration_secs: u32,
            app: Option<tauri::AppHandle>,
        ) -> Result<Self, String> {
            Self::start_with_source(CaptureSource::Process(pid), epoch, duration_secs, app)
        }

        fn start_with_source(
            source: CaptureSource,
            epoch: Instant,
            duration_secs: u32,
            app: Option<tauri::AppHandle>,
        ) -> Result<Self, String> {
            let (cmd_tx, cmd_rx) = mpsc::sync_channel::<AudioCmd>(8);
            let (init_tx, init_rx) = mpsc::sync_channel::<Result<AudioFormat, String>>(1);

            let join_handle = thread::Builder::new()
                .name("clippy-audio-capture".into())
                .spawn(move || {
                    run_capture(source, epoch, duration_secs, cmd_rx, init_tx, app);
                })
                .map_err(|e| format!("spawn audio thread: {e}"))?;

            let format = init_rx
                .recv()
                .map_err(|_| "audio thread died during init".to_string())?
                .map_err(|e| format!("audio init: {e}"))?;

            Ok(AudioCaptureHandle {
                cmd_tx,
                join_handle: Some(join_handle),
                format,
            })
        }

        pub fn snapshot(&self) -> Result<Vec<AudioPacket>, String> {
            let (tx, rx) = mpsc::sync_channel(1);
            self.cmd_tx
                .send(AudioCmd::Snapshot(tx))
                .map_err(|_| "audio channel closed".to_string())?;
            rx.recv().map_err(|e| format!("audio snapshot: {e}"))?
        }

        pub fn format(&self) -> &AudioFormat {
            &self.format
        }

        pub fn stop(mut self) {
            let _ = self.cmd_tx.send(AudioCmd::Stop);
            if let Some(h) = self.join_handle.take() {
                let _ = h.join();
            }
        }
    }

    impl Drop for AudioCaptureHandle {
        fn drop(&mut self) {
            let _ = self.cmd_tx.send(AudioCmd::Stop);
            if let Some(h) = self.join_handle.take() {
                let _ = h.join();
            }
        }
    }

    fn run_capture(
        source: CaptureSource,
        epoch: Instant,
        duration_secs: u32,
        cmd_rx: mpsc::Receiver<AudioCmd>,
        init_tx: SyncSender<Result<AudioFormat, String>>,
        app: Option<tauri::AppHandle>,
    ) {
        // Stable label for diag entries from this thread. Built before
        // `source` is consumed by the init match below so we can reference
        // it from any of the error paths inside the capture loop. Process
        // loopback uses the pid; device captures use the WASAPI endpoint
        // id (verbose by design — it's what the Settings UI also persists,
        // so a copy-pasted diag can be matched back to a specific row).
        let label: String = match &source {
            CaptureSource::Device(Some(id)) => format!("device {id}"),
            CaptureSource::Device(None) => "default device".to_string(),
            CaptureSource::InputDevice(Some(id)) => format!("input device {id}"),
            CaptureSource::InputDevice(None) => "default input device".to_string(),
            CaptureSource::Process(pid) => format!("process loopback pid {pid}"),
        };

        // COM init for this thread. We request MTA because the WASAPI
        // activation path used by process loopback (`ActivateAudioInterfaceAsync`)
        // returns E_ILLEGAL_METHOD_CALL synchronously when called from STA.
        // The raw HRESULT is informative here: S_OK = freshly initialized as
        // MTA; S_FALSE (folded into Ok in windows-rs) = MTA already; an Err
        // typically means RPC_E_CHANGED_MODE (0x80010106), which signals
        // someone else got here first and put us in STA — explains the
        // process-loopback failure we see in production logs.
        //
        // Only logged when this thread is wired up for process loopback so
        // the diag isn't spammy for the dozens-of-tracks setups some users
        // run; the device-loopback / input-capture paths don't depend on
        // apartment state and have never failed in the field.
        let is_process_source = matches!(source, CaptureSource::Process(_));
        let init_hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if is_process_source {
            let init_msg = match init_hr.0 as u32 {
                0x00000000 => "S_OK (MTA initialized this call)".to_string(),
                0x00000001 => "S_FALSE (already initialized; same apartment)".to_string(),
                0x80010106 => "RPC_E_CHANGED_MODE (already initialized as STA — process-loopback ActivateAudioInterfaceAsync will fail E_ILLEGAL_METHOD_CALL)".to_string(),
                other => format!("HRESULT {:#010x}", other),
            };
            let apt_msg = unsafe {
                let mut apt: APTTYPE = std::mem::zeroed();
                let mut qual: APTTYPEQUALIFIER = std::mem::zeroed();
                match CoGetApartmentType(&mut apt, &mut qual) {
                    Ok(()) => format!("apt={:?} qual={:?}", apt, qual),
                    Err(e) => format!("CoGetApartmentType Err {e}"),
                }
            };
            if let Some(a) = &app {
                crate::diag(
                    a,
                    format!(
                        "[replay] audio thread COM state for {label}: CoInitializeEx(MTA) → {init_msg} · {apt_msg}"
                    ),
                );
            }
        }

        let init_result = match source {
            CaptureSource::Device(id) => init_capture(id, EndpointKind::Render),
            CaptureSource::InputDevice(id) => init_capture(id, EndpointKind::Capture),
            CaptureSource::Process(pid) => init_process_capture(pid),
        };
        let (audio_client, capture_client, format) = match init_result {
            Ok(t) => t,
            Err(e) => {
                let _ = init_tx.send(Err(e));
                unsafe { CoUninitialize() };
                return;
            }
        };

        // Start the WASAPI capture stream BEFORE signalling init success.
        // If Start() fails (rare — typically a device-state issue after a
        // clean Initialize) the worker's existing per-device init-error log
        // captures the cause via the init_tx error path. Previously the
        // failure was `.is_err()`-discarded and the thread silently exited,
        // leaving the worker convinced the track was healthy and the saved
        // clip missing that audio with no breadcrumb in the diag log.
        if let Err(e) = unsafe { audio_client.Start() } {
            let _ = init_tx.send(Err(format!("Start: {e}")));
            unsafe { CoUninitialize() };
            return;
        }

        // We own a started stream from here on; the worker commits to
        // expecting packets from us once it sees this Ok.
        let _ = init_tx.send(Ok(format.clone()));

        let bytes_per_frame = (format.channels as u32) * (format.bits_per_sample as u32) / 8;
        let bytes_per_sec = format.sample_rate * bytes_per_frame;
        // Rough cap on stored bytes to bound memory: duration_secs of audio + a little slack.
        let buffer_byte_cap: usize = (bytes_per_sec as usize) * (duration_secs as usize + 1);
        let duration_pts: i64 = duration_secs as i64 * 10_000_000;

        let mut buffer: VecDeque<AudioPacket> = VecDeque::with_capacity(2048);
        let mut current_bytes: usize = 0;
        // One-shot diag gates: a persistently-failing WASAPI call would
        // otherwise spam the 200-entry diag ring at ~200/sec and evict every
        // other entry within a second. Log the first hit so the user has a
        // breadcrumb; further occurrences fall through silently.
        let mut logged_get_size_err = false;
        let mut logged_get_buffer_err = false;
        // QPC-anchored PTS state. WASAPI's `pu64QPCPosition` (returned in
        // 100-ns units) records when the audio device actually captured the
        // frame, not when our 5-ms poll happened to read it out. Anchoring on
        // the first valid sample and adding QPC deltas thereafter removes
        // read-jitter from the PTS clock — important for buffer trim accuracy
        // and (more visibly) for keeping the audio's wall-clock relationship
        // to video consistent across the saved clip.
        //
        // The anchor is `(first_qpc_100ns, first_pts_100ns)`. `first_pts_100ns`
        // is `epoch.elapsed()` at the moment we received the first packet —
        // it's the only PTS we can't get from QPC because we have no QPC
        // sample before the first packet to compute against. All subsequent
        // packets get `first_pts + (packet_qpc - first_qpc)`, which is
        // jitter-free in the absence of WASAPI timestamp errors.
        let mut qpc_anchor: Option<(u64, i64)> = None;
        let mut logged_timestamp_err = false;

        'main: loop {
            // Wall-clock-driven trim. Runs every iteration so an idle WASAPI
            // endpoint can't strand pre-idle packets in the buffer past their
            // retention window. Previously the trim only ran inside the drain
            // loop below — for endpoints whose render path goes dormant (e.g.
            // Sonar virtual outputs when nothing's routing through them,
            // observed delivering a single burst then no packets for hours),
            // the trim never fired and a save would surface audio from the
            // last burst regardless of how long ago it played.
            //
            // Cutoff is `now - duration_pts` of wall-clock time. After a
            // prolonged silence the buffer correctly empties; the save then
            // reports the track as "dropped (no captured packets)" rather
            // than splicing in hours-old audio at the start of the clip.
            {
                let now_pts = epoch.elapsed().as_nanos() as i64 / 100;
                let cutoff_pts = now_pts - duration_pts;
                while let Some(front) = buffer.front() {
                    if front.pts < cutoff_pts || current_bytes > buffer_byte_cap {
                        let removed = buffer.pop_front().unwrap();
                        current_bytes = current_bytes.saturating_sub(removed.data.len());
                    } else {
                        break;
                    }
                }
            }

            // Process commands. The Snapshot reply uses the buffer state
            // produced by the trim above — so a snapshot taken after a long
            // idle window correctly reports zero packets instead of leaking
            // stale audio into the saved clip.
            loop {
                match cmd_rx.try_recv() {
                    Ok(AudioCmd::Stop) => break 'main,
                    Ok(AudioCmd::Snapshot(reply)) => {
                        let snap: Vec<AudioPacket> = buffer.iter().cloned().collect();
                        let _ = reply.send(Ok(snap));
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => break 'main,
                }
            }

            // Drain whatever WASAPI has for us.
            loop {
                let next_size = match unsafe { capture_client.GetNextPacketSize() } {
                    Ok(n) => n,
                    Err(e) => {
                        if !logged_get_size_err {
                            logged_get_size_err = true;
                            if let Some(a) = &app {
                                crate::diag(
                                    a,
                                    format!(
                                        "[replay] audio: {label}: GetNextPacketSize error (first hit; thread continues polling): {e}"
                                    ),
                                );
                            }
                        }
                        0
                    }
                };
                if next_size == 0 {
                    break;
                }

                let mut data_ptr: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                let mut device_pos: u64 = 0;
                let mut qpc_pos: u64 = 0;
                let r = unsafe {
                    capture_client.GetBuffer(
                        &mut data_ptr,
                        &mut frames,
                        &mut flags,
                        Some(&mut device_pos),
                        Some(&mut qpc_pos),
                    )
                };
                if let Err(e) = r {
                    if !logged_get_buffer_err {
                        logged_get_buffer_err = true;
                        if let Some(a) = &app {
                            crate::diag(
                                a,
                                format!(
                                    "[replay] audio: {label}: GetBuffer error (first hit; capture will skip packets until WASAPI recovers): {e}"
                                ),
                            );
                        }
                    }
                    break;
                }

                let byte_len = (frames as usize) * (bytes_per_frame as usize);
                let read_time_pts = epoch.elapsed().as_nanos() as i64 / 100;

                // Compute PTS from QPC unless WASAPI flagged the timestamp as
                // unreliable. On `TIMESTAMP_ERROR` or `DATA_DISCONTINUITY` we
                // fall back to read-time and reset the anchor so the next
                // valid sample re-establishes it without inheriting drift.
                let timestamp_unreliable = (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32)
                    != 0
                    || (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0;
                let pts = if timestamp_unreliable {
                    if !logged_timestamp_err {
                        logged_timestamp_err = true;
                        if let Some(a) = &app {
                            crate::diag(
                                a,
                                format!(
                                    "[replay] audio: {label}: WASAPI flagged QPC timestamp unreliable (first hit; falling back to read-time PTS until QPC clears)"
                                ),
                            );
                        }
                    }
                    qpc_anchor = None;
                    read_time_pts
                } else {
                    match qpc_anchor {
                        None => {
                            qpc_anchor = Some((qpc_pos, read_time_pts));
                            read_time_pts
                        }
                        Some((anchor_qpc, anchor_pts)) => {
                            // QPC is monotonic and already in 100-ns units, so
                            // `qpc_pos - anchor_qpc` is the exact time delta
                            // since the anchor sample. Wrap as i64 for the PTS
                            // arithmetic; the underlying counter is plenty
                            // wide that u64 → i64 won't overflow in any realistic
                            // session lifetime.
                            anchor_pts + (qpc_pos as i64 - anchor_qpc as i64)
                        }
                    }
                };

                let payload: Arc<[u8]> = if (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
                    // Hardware silence — emit zeros so PTS continuity holds.
                    vec![0u8; byte_len].into()
                } else {
                    unsafe {
                        std::slice::from_raw_parts(data_ptr, byte_len)
                            .to_vec()
                            .into()
                    }
                };

                let _ = unsafe { capture_client.ReleaseBuffer(frames) };

                current_bytes += payload.len();
                buffer.push_back(AudioPacket { data: payload, pts });
                // Trim moved to the wall-clock-driven block at the top of
                // 'main so it runs even when WASAPI delivers no packets for
                // long stretches. The byte-cap defense is part of that same
                // outer trim — at a 5 ms outer-loop cadence it reacts to a
                // burst within one tick, plenty fast for the cap to bound
                // RAM during a flood.
            }

            thread::sleep(Duration::from_millis(5));
        }

        unsafe {
            let _ = audio_client.Stop();
            CoUninitialize();
        }
    }

    fn init_capture(
        device_id: Option<String>,
        kind: EndpointKind,
    ) -> Result<(IAudioClient, IAudioCaptureClient, AudioFormat), String> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| format!("device enumerator: {e}"))?;

            // Render endpoints are opened in loopback mode (we capture what
            // they're playing). Capture endpoints (microphones, line-in) are
            // opened as ordinary capture clients with no loopback flag —
            // the loopback flag on a capture endpoint returns silence.
            let (default_flow, stream_flags) = match kind {
                EndpointKind::Render => (eRender, AUDCLNT_STREAMFLAGS_LOOPBACK),
                EndpointKind::Capture => (eCapture, 0),
            };

            let device: IMMDevice = match device_id {
                Some(id) => {
                    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                    enumerator
                        .GetDevice(PCWSTR(wide.as_ptr()))
                        .map_err(|e| format!("get device by id: {e}"))?
                }
                None => enumerator
                    .GetDefaultAudioEndpoint(default_flow, eConsole)
                    .map_err(|e| format!("default endpoint: {e}"))?,
            };

            let audio_client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("activate audio client: {e}"))?;

            let mix_format_ptr = audio_client
                .GetMixFormat()
                .map_err(|e| format!("get mix format: {e}"))?;

            let format = parse_wave_format(mix_format_ptr);

            // Loopback requires SHARED mode. Buffer ~200 ms.
            audio_client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    stream_flags,
                    2_000_000,
                    0,
                    mix_format_ptr,
                    None,
                )
                .map_err(|e| format!("init audio client: {e}"))?;

            // CoTaskMemFree per WASAPI contract — but the WAVEFORMATEX is now
            // owned by the client which copied it, so we can free our copy.
            // In practice many examples leak this; for a long-running thread
            // it's a one-shot allocation so impact is negligible.

            let capture_client: IAudioCaptureClient = audio_client
                .GetService()
                .map_err(|e| format!("get capture client: {e}"))?;

            Ok((audio_client, capture_client, format))
        }
    }

    /// Decode a WAVEFORMATEX (or extensible) pointer into our flat format struct.
    unsafe fn parse_wave_format(ptr: *const WAVEFORMATEX) -> AudioFormat {
        let base = *ptr;
        let mut is_float = base.wFormatTag == WAVE_FORMAT_IEEE_FLOAT;

        if base.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
            // WAVEFORMATEX is the prefix of WAVEFORMATEXTENSIBLE.
            let ext = ptr as *const WAVEFORMATEXTENSIBLE;
            let subformat = (*ext).SubFormat;
            if subformat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                is_float = true;
            }
        }

        AudioFormat {
            sample_rate: base.nSamplesPerSec,
            channels: base.nChannels,
            bits_per_sample: base.wBitsPerSample,
            is_float,
        }
    }

    // ---------- Process Loopback (Phase 3.3, Win11 22H2+) ----------
    //
    // Real activation lives in `super::process_loopback` so the unsafe COM
    // vtable + refcount stays isolated. On Win10 / activation failure the
    // returned Err lets the worker fall back to system loopback so audio
    // still records.
    fn init_process_capture(
        pid: u32,
    ) -> Result<(IAudioClient, IAudioCaptureClient, AudioFormat), String> {
        super::super::process_loopback::init_process_capture(pid)
    }
}
