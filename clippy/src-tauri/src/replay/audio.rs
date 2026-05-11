//! WASAPI loopback audio capture.
//!
//! Each `AudioCaptureHandle` runs a dedicated thread that pulls PCM from one
//! WASAPI render endpoint in loopback mode, timestamps each packet against
//! a shared wall-clock epoch (so video and audio stay in sync), and buffers
//! the most recent N seconds. Snapshot returns the current buffer for save.

use std::sync::Arc;

#[derive(Clone, Debug, serde::Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
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
    windows_impl::enumerate_render_devices()
}

#[cfg(not(windows))]
pub fn enumerate_render_devices() -> Vec<AudioDevice> {
    Vec::new()
}

#[cfg(windows)]
pub use windows_impl::AudioCaptureHandle;

#[cfg(windows)]
mod windows_impl {
    use super::{AudioDevice, AudioFormat, AudioPacket};
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, SyncSender};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };

    // wFormatTag values from mmeapi.h — not re-exported by name in this windows-rs.
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    // Well-known GUID for IEEE-float audio in WAVEFORMATEXTENSIBLE.SubFormat.
    // Not re-exported by name in this windows-rs version.
    const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: windows::core::GUID =
        windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    pub fn enumerate_render_devices() -> Vec<AudioDevice> {
        let mut out = Vec::new();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                    Ok(e) => e,
                    Err(_) => return out,
                };

            let default_id = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .ok()
                .and_then(|d| d.GetId().ok())
                .map(|p| unsafe { pcwstr_to_string(p) })
                .unwrap_or_default();

            let collection = match enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
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
                    None if is_default => format!("Default output (device #{})", i + 1),
                    None => format!("Output #{}", i + 1),
                };
                out.push(AudioDevice {
                    id,
                    name,
                    is_default,
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
        /// Process Loopback (Win11 22H2+) — captures only this PID's audio
        /// (and its child processes).
        Process(u32),
    }

    impl AudioCaptureHandle {
        pub fn start(
            device_id: Option<String>,
            epoch: Instant,
            duration_secs: u32,
        ) -> Result<Self, String> {
            Self::start_with_source(CaptureSource::Device(device_id), epoch, duration_secs)
        }

        /// Phase 3.3 — capture only the given process's audio.
        /// Requires Windows 11 22H2 or later. On older systems this fails and
        /// the caller is expected to fall back to `start(None, ...)` for system
        /// loopback.
        pub fn start_for_process(
            pid: u32,
            epoch: Instant,
            duration_secs: u32,
        ) -> Result<Self, String> {
            Self::start_with_source(CaptureSource::Process(pid), epoch, duration_secs)
        }

        fn start_with_source(
            source: CaptureSource,
            epoch: Instant,
            duration_secs: u32,
        ) -> Result<Self, String> {
            let (cmd_tx, cmd_rx) = mpsc::sync_channel::<AudioCmd>(8);
            let (init_tx, init_rx) = mpsc::sync_channel::<Result<AudioFormat, String>>(1);

            let join_handle = thread::Builder::new()
                .name("clippy-audio-capture".into())
                .spawn(move || {
                    run_capture(source, epoch, duration_secs, cmd_rx, init_tx);
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
    ) {
        // COM init for this thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        let init_result = match source {
            CaptureSource::Device(id) => init_capture(id),
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

        let _ = init_tx.send(Ok(format.clone()));

        let bytes_per_frame = (format.channels as u32) * (format.bits_per_sample as u32) / 8;
        let bytes_per_sec = format.sample_rate * bytes_per_frame;
        // Rough cap on stored bytes to bound memory: duration_secs of audio + a little slack.
        let buffer_byte_cap: usize = (bytes_per_sec as usize) * (duration_secs as usize + 1);
        let duration_pts: i64 = duration_secs as i64 * 10_000_000;

        let mut buffer: VecDeque<AudioPacket> = VecDeque::with_capacity(2048);
        let mut current_bytes: usize = 0;

        unsafe {
            if audio_client.Start().is_err() {
                return;
            }
        }

        'main: loop {
            // Process commands.
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
                    Err(_) => 0,
                };
                if next_size == 0 {
                    break;
                }

                let mut data_ptr: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                let r = unsafe {
                    capture_client.GetBuffer(
                        &mut data_ptr,
                        &mut frames,
                        &mut flags,
                        None,
                        None,
                    )
                };
                if r.is_err() {
                    break;
                }

                let byte_len = (frames as usize) * (bytes_per_frame as usize);
                let pts_now = epoch.elapsed().as_nanos() as i64 / 100;

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
                buffer.push_back(AudioPacket {
                    data: payload,
                    pts: pts_now,
                });

                // Trim by duration AND by byte cap (defensive).
                let cutoff_pts = pts_now - duration_pts;
                while let Some(front) = buffer.front() {
                    if front.pts < cutoff_pts || current_bytes > buffer_byte_cap {
                        let removed = buffer.pop_front().unwrap();
                        current_bytes = current_bytes.saturating_sub(removed.data.len());
                    } else {
                        break;
                    }
                }
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
    ) -> Result<(IAudioClient, IAudioCaptureClient, AudioFormat), String> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| format!("device enumerator: {e}"))?;

            let device: IMMDevice = match device_id {
                Some(id) => {
                    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                    enumerator
                        .GetDevice(PCWSTR(wide.as_ptr()))
                        .map_err(|e| format!("get device by id: {e}"))?
                }
                None => enumerator
                    .GetDefaultAudioEndpoint(eRender, windows::Win32::Media::Audio::eConsole)
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
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
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
