//! Media Foundation H.264 encode session.
//!
//! Accepts D3D11 textures directly via MF_SA_D3D11_BINDABLE (no CPU readback).
//! Hardware encoder waterfall: NVENC → AMF → QSV → software.

#[cfg(windows)]
pub mod windows_impl {
    use windows::{
        core::{Interface, Result, GUID, HRESULT},
        Win32::{
            Foundation::BOOL,
            Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D},
            Media::MediaFoundation::{
                IMFActivate, IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaType, IMFSample,
                IMFTransform, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer,
                MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
                MFShutdown, MFStartup, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12,
                MFVideoInterlace_Progressive, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
                MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
                MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_REGISTER_TYPE_INFO,
                MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
                MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_VERSION,
            },
        },
    };

    // ----- ICodecAPI (hand-rolled) -----
    //
    // `windows-rs 0.58`'s Media-Audio / MediaFoundation feature set doesn't
    // expose `ICodecAPI` or the rate-control codec API GUIDs. Pulling in
    // `Win32_Media_DirectShow` to get them adds noticeable compile time and
    // disk size for a single interface, so we hand-roll the COM vtable call
    // here the same way `process_loopback.rs` does for
    // `IActivateAudioInterfaceCompletionHandler`.
    //
    // The plumbing matters because setting only `MF_MT_AVG_BITRATE` on the
    // output type — which is what the encoder did before this — leaves the
    // AMD AMF MFT in its default quality-targeted VBR mode. Observed effect:
    // a 30 Mbps target produced ~55 Mbps in practice, nearly doubling buffer
    // RAM and saved-clip size. Setting CBR + mean bitrate via ICodecAPI makes
    // the encoder honor the configured rate. The GOP setter (AVEncMPVGOPSize)
    // is the same plumbing gap — without it, the encoder picks its own GOP
    // (~250 frames on most HW MFTs) regardless of the user's keyframe
    // interval setting, and the buffer trim is then granular to the encoder's
    // GOP rather than the user-configured value.

    /// ICodecAPI IID — {901db4c7-31ce-41a2-85dc-8fa0bf41b8da}.
    const IID_ICODECAPI: GUID = GUID::from_u128(0x901db4c7_31ce_41a2_85dc_8fa0bf41b8da);

    /// CODECAPI_AVEncCommonRateControlMode — selects CBR / VBR / quality.
    /// Value type: VT_UI4. Use `RATE_CONTROL_CBR` (= 0) for constant bitrate.
    const CODECAPI_AVENC_COMMON_RATE_CONTROL_MODE: GUID =
        GUID::from_u128(0x1c0608e9_370c_4710_8a58_cb6181c42423);
    /// CODECAPI_AVEncCommonMeanBitRate — target bitrate in bits/sec. VT_UI4.
    const CODECAPI_AVENC_COMMON_MEAN_BIT_RATE: GUID =
        GUID::from_u128(0xf7f0f0d2_2516_4e89_b87f_0c1c3f6c5db1);
    /// CODECAPI_AVEncMPVGOPSize — GOP size in frames. VT_UI4.
    const CODECAPI_AVENC_MPV_GOP_SIZE: GUID =
        GUID::from_u128(0x95f31b26_95a4_4a3a_b2a9_7e83a8c7d0a3);

    /// eAVEncCommonRateControlMode_CBR — constant-bitrate rate control.
    const RATE_CONTROL_CBR: u32 = 0;

    /// `ICodecAPI::SetValue` for a VT_UI4 parameter. QueryInterface from the
    /// IMFTransform (most HW H.264 encoder MFTs implement ICodecAPI on the
    /// same object), build a 24-byte VARIANT with VT_UI4 + the value, then
    /// call vtable slot 9. Best-effort: returns Err on any failure so the
    /// caller can log and continue without breaking encoder creation.
    unsafe fn icodecapi_set_value_u32(
        encoder: &IMFTransform,
        api: &GUID,
        value: u32,
    ) -> std::result::Result<(), String> {
        use std::ffi::c_void;

        // Get ICodecAPI on the encoder via IUnknown::QueryInterface. We can't
        // use windows-rs `cast::<ICodecAPI>()` because ICodecAPI isn't in the
        // feature set, so we call QueryInterface manually through the IUnknown
        // vtable of the IMFTransform (slot 0).
        let unk_raw = encoder.as_raw();
        if unk_raw.is_null() {
            return Err("encoder raw pointer is null".into());
        }
        let unk_vtbl = *(unk_raw as *const *const usize);
        let qi: unsafe extern "system" fn(
            *mut c_void,
            *const GUID,
            *mut *mut c_void,
        ) -> HRESULT = std::mem::transmute(*unk_vtbl.add(0));

        let mut codec_api: *mut c_void = std::ptr::null_mut();
        let hr = qi(unk_raw, &IID_ICODECAPI, &mut codec_api);
        if !hr.is_ok() || codec_api.is_null() {
            return Err(format!(
                "QueryInterface(ICodecAPI) HRESULT {:#x}",
                hr.0 as u32
            ));
        }

        // Build VARIANT (x64 layout, 24 bytes): vt(u16) at offset 0; the
        // wReserved1/2/3 fields fill 2..8 with zero; for VT_UI4 the ulVal
        // sits at offset 8. The remaining 12 bytes (offsets 12..24) are
        // unused for this VT but must exist so SetValue reads a properly
        // sized VARIANT off our stack.
        const VT_UI4: u16 = 19;
        let mut variant = [0u8; 24];
        std::ptr::write_unaligned(variant.as_mut_ptr() as *mut u16, VT_UI4);
        std::ptr::write_unaligned(variant.as_mut_ptr().add(8) as *mut u32, value);

        // ICodecAPI vtable layout: IUnknown methods at 0..3, then ICodecAPI's
        // IsSupported(3) / IsModifiable(4) / GetParameterRange(5) /
        // GetParameterValues(6) / GetDefaultValue(7) / GetValue(8) / SetValue(9).
        let codec_vtbl = *(codec_api as *const *const usize);
        let set_value: unsafe extern "system" fn(*mut c_void, *const GUID, *const u8) -> HRESULT =
            std::mem::transmute(*codec_vtbl.add(9));
        let set_hr = set_value(codec_api, api, variant.as_ptr());

        // Release the QueryInterface ref regardless of SetValue outcome.
        let release: unsafe extern "system" fn(*mut c_void) -> u32 =
            std::mem::transmute(*codec_vtbl.add(2));
        release(codec_api);

        if set_hr.is_ok() {
            Ok(())
        } else {
            Err(format!("ICodecAPI::SetValue HRESULT {:#x}", set_hr.0 as u32))
        }
    }

    pub fn mf_startup() -> Result<()> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
    }

    pub fn mf_shutdown() {
        unsafe {
            let _ = MFShutdown();
        }
    }

    /// Pack width+height into the u64 format MF_MT_FRAME_SIZE expects.
    fn pack_size(w: u32, h: u32) -> u64 {
        ((w as u64) << 32) | h as u64
    }

    /// Pack numerator+denominator into the u64 format MF_MT_FRAME_RATE expects.
    fn pack_ratio(num: u32, den: u32) -> u64 {
        ((num as u64) << 32) | den as u64
    }

    #[cfg(feature = "poc")]
    pub fn create_h264_encoder(
        d3d_device: &ID3D11Device,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        fps_num: u32,
        fps_den: u32,
    ) -> Result<IMFTransform> {
        use windows::Win32::Media::MediaFoundation::MFT_MESSAGE_COMMAND_FLUSH;
        let (encoder, _name) = find_hardware_encoder().or_else(|_| find_software_encoder())?;

        let input_type: IMFMediaType = unsafe { MFCreateMediaType()? };
        unsafe {
            input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(width, height))?;
            input_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps_num, fps_den))?;
            input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
            input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        }

        let output_type: IMFMediaType = unsafe { MFCreateMediaType()? };
        unsafe {
            output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            output_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(width, height))?;
            output_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps_num, fps_den))?;
            output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
            output_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            output_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbps * 1000)?;
        }

        // Wire D3D11 device into the encoder so it can accept GPU-resident textures.
        let mut dm: Option<IMFDXGIDeviceManager> = None;
        let mut reset_token: u32 = 0;
        unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut dm)? };
        let device_manager = dm.unwrap();
        unsafe { device_manager.ResetDevice(d3d_device, reset_token)? };

        unsafe {
            encoder.SetInputType(0, &input_type, 0)?;
            encoder.SetOutputType(0, &output_type, 0)?;
            // Pass device manager as IUnknown pointer so encoder can import textures.
            let dm_unk: windows::core::IUnknown = device_manager.cast()?;
            encoder.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, dm_unk.as_raw() as usize)?;
            encoder.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        Ok(encoder)
    }

    fn find_hardware_encoder() -> Result<(IMFTransform, String)> {
        enumerate_encoders(MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER)
    }

    fn find_software_encoder() -> Result<(IMFTransform, String)> {
        enumerate_encoders(MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER)
    }

    /// Read MFT_FRIENDLY_NAME_Attribute from an IMFActivate. Empty string if
    /// the attribute isn't set or the read fails (best-effort labelling only).
    unsafe fn activate_friendly_name(act: &IMFActivate) -> String {
        use windows::core::PWSTR;
        use windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute;
        use windows::Win32::System::Com::CoTaskMemFree;

        let mut name_ptr = PWSTR::null();
        let mut name_len: u32 = 0;
        if act
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name_ptr, &mut name_len)
            .is_err()
            || name_ptr.is_null()
        {
            return String::new();
        }
        let slice = std::slice::from_raw_parts(name_ptr.0, name_len as usize);
        let name = String::from_utf16_lossy(slice);
        CoTaskMemFree(Some(name_ptr.0 as *const _));
        name
    }

    fn enumerate_encoders(
        flags: windows::Win32::Media::MediaFoundation::MFT_ENUM_FLAG,
    ) -> Result<(IMFTransform, String)> {
        let output_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut pactivates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                None,
                Some(&output_info as *const _),
                &mut pactivates,
                &mut count,
            )?;
        }
        if count == 0 || pactivates.is_null() {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_NOTIMPL,
                "no H.264 encoder found",
            ));
        }
        let (encoder, name) = unsafe {
            let activate = (*pactivates).as_ref().unwrap();
            let name = activate_friendly_name(activate);
            let t: IMFTransform = activate.ActivateObject()?;
            (t, name)
        };
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(pactivates as *const _));
        }
        Ok((encoder, name))
    }

    /// Submit one D3D11 texture to the encoder as an input sample.
    #[cfg(feature = "poc")]
    pub fn submit_texture_frame(
        encoder: &IMFTransform,
        texture: &ID3D11Texture2D,
        pts: i64,
        duration: i64,
    ) -> Result<()> {
        let buffer =
            unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, BOOL(0))? };
        let sample: IMFSample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            encoder.ProcessInput(0, &sample, 0)?;
        }
        Ok(())
    }

    /// Drain all available encoded packets from the encoder.
    /// Returns `(data, pts, is_keyframe)` per packet.
    #[cfg(feature = "poc")]
    pub fn drain_encoder(encoder: &IMFTransform) -> Result<Vec<(Vec<u8>, i64, bool)>> {
        use std::mem::ManuallyDrop;
        use windows::Win32::Media::MediaFoundation::{
            MFT_OUTPUT_STREAM_INFO, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
            MF_E_TRANSFORM_NEED_MORE_INPUT,
        };

        // Check whether the encoder allocates its own output samples.
        let stream_info: MFT_OUTPUT_STREAM_INFO = unsafe { encoder.GetOutputStreamInfo(0)? };
        let encoder_provides =
            (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

        let mut packets = Vec::new();
        loop {
            // Pre-allocate a sample+buffer when the encoder needs the caller to provide one.
            let pre_sample: Option<IMFSample> = if encoder_provides {
                None
            } else {
                let buf: IMFMediaBuffer =
                    unsafe { MFCreateMemoryBuffer(stream_info.cbSize.max(1))? };
                let s: IMFSample = unsafe { MFCreateSample()? };
                unsafe { s.AddBuffer(&buf)? };
                Some(s)
            };

            let mut output_data = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(pre_sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;

            match unsafe { encoder.ProcessOutput(0, &mut output_data, &mut status) } {
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(e) => return Err(e),
                Ok(()) => {}
            }

            // Same ManuallyDrop ownership transfer as in process_output_async
            // — without this, every drained packet leaks its IMFSample +
            // associated buffer. drain_encoder is only used by the PoC paths
            // today, but the bug class is identical.
            let sample_owned: Option<IMFSample> =
                unsafe { ManuallyDrop::take(&mut output_data[0].pSample) };
            let _events_owned: Option<windows::Win32::Media::MediaFoundation::IMFCollection> =
                unsafe { ManuallyDrop::take(&mut output_data[0].pEvents) };

            if let Some(sample) = sample_owned.as_ref() {
                let pts = unsafe { sample.GetSampleTime()? };
                let is_keyframe = unsafe {
                    sample
                        .GetUINT32(
                            &windows::Win32::Media::MediaFoundation::MFSampleExtension_CleanPoint,
                        )
                        .unwrap_or(0)
                        != 0
                };
                let buffer: IMFMediaBuffer = unsafe { sample.ConvertToContiguousBuffer()? };
                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                unsafe {
                    buffer.Lock(&mut ptr, None, Some(&mut len))?;
                    let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                    buffer.Unlock()?;
                    packets.push((data, pts, is_keyframe));
                }
            }
        }
        Ok(packets)
    }

    /// CPU-path encoder: no D3D11 device manager, accepts NV12 memory buffers.
    /// Used for Phase 1 validation only; Phase 2+ uses the GPU path.
    #[cfg(feature = "poc")]
    pub fn create_h264_encoder_simple(
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        fps_num: u32,
        fps_den: u32,
    ) -> Result<IMFTransform> {
        // Hardware encoders (NVENC/AMF/QSV) are async MFTs and require an
        // event-driven protocol. Software encoder is a sync MFT — correct for
        // the Phase 1 direct ProcessInput/ProcessOutput path.
        let (encoder, _name) = find_software_encoder()?;

        let input_type: IMFMediaType = unsafe { MFCreateMediaType()? };
        unsafe {
            input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(width, height))?;
            input_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps_num, fps_den))?;
            input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
            input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        }

        let output_type: IMFMediaType = unsafe { MFCreateMediaType()? };
        unsafe {
            output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            output_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(width, height))?;
            output_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps_num, fps_den))?;
            output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
            output_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            output_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbps * 1000)?;
        }

        unsafe {
            // Output type must be set before input type on the MF H.264 encoder.
            encoder.SetOutputType(0, &output_type, 0)?;
            encoder.SetInputType(0, &input_type, 0)?;
            encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        Ok(encoder)
    }

    /// Submit one NV12 frame as a CPU memory buffer.
    #[cfg(feature = "poc")]
    pub fn submit_nv12_frame(
        encoder: &IMFTransform,
        nv12: &[u8],
        pts: i64,
        duration: i64,
    ) -> Result<()> {
        let buffer: IMFMediaBuffer = unsafe { MFCreateMemoryBuffer(nv12.len() as u32)? };
        let mut ptr: *mut u8 = std::ptr::null_mut();
        unsafe {
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), ptr, nv12.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(nv12.len() as u32)?;
        }
        let sample: IMFSample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            encoder.ProcessInput(0, &sample, 0)?;
        }
        Ok(())
    }

    /// Signal end-of-stream and drain all remaining encoded packets.
    #[cfg(feature = "poc")]
    pub fn flush_encoder(encoder: &IMFTransform) -> Result<Vec<(Vec<u8>, i64, bool)>> {
        use windows::Win32::Media::MediaFoundation::{
            MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
        };
        unsafe {
            let _ = encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = encoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
        drain_encoder(encoder)
    }

    // ---------- async hardware encoder path ----------

    /// Create a hardware H.264 encoder (NVENC/AMF/QSV) wired to a D3D11 device.
    ///
    /// Returns the encoder and the device manager (caller must keep both alive
    /// for the encode session). Encoder is in async-unlocked mode; submit input
    /// only on `METransformNeedInput`, drain only on `METransformHaveOutput`.
    pub fn create_h264_encoder_hw_async(
        d3d_device: &ID3D11Device,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        fps_num: u32,
        fps_den: u32,
        preference: crate::replay::EncoderPreference,
        keyframe_interval_secs: Option<u32>,
    ) -> Result<(IMFTransform, IMFDXGIDeviceManager, String, EncoderRateControlReport)> {
        use windows::Win32::Media::MediaFoundation::MF_TRANSFORM_ASYNC_UNLOCK;

        let (encoder, encoder_name) = pick_encoder(preference)?;

        // Unlock async mode — required before any other configuration call.
        let attrs = unsafe { encoder.GetAttributes()? };
        unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)? };

        // Create + reset the DXGI device manager so encoder can read GPU textures.
        let mut dm: Option<IMFDXGIDeviceManager> = None;
        let mut reset_token: u32 = 0;
        unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut dm)? };
        let device_manager = dm.unwrap();
        unsafe { device_manager.ResetDevice(d3d_device, reset_token)? };

        // Hand the device manager to the encoder.
        let dm_unk: windows::core::IUnknown = device_manager.cast()?;
        unsafe { encoder.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, dm_unk.as_raw() as usize)? };

        // Output type (H.264) must be set before input type.
        let output_type: IMFMediaType = unsafe { MFCreateMediaType()? };
        unsafe {
            output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            output_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(width, height))?;
            output_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps_num, fps_den))?;
            output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
            output_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            output_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbps * 1000)?;
            encoder.SetOutputType(0, &output_type, 0)?;
        }

        let input_type: IMFMediaType = unsafe { MFCreateMediaType()? };
        unsafe {
            input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(width, height))?;
            input_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps_num, fps_den))?;
            input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
            input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            encoder.SetInputType(0, &input_type, 0)?;
        }

        // Rate control + GOP via hand-rolled ICodecAPI (see icodecapi_set_value_u32
        // above for why the COM call is hand-rolled). Each setter is independent
        // and best-effort: failures are recorded in the report rather than
        // failing encoder creation. The Microsoft software H.264 MFT in
        // particular often returns S_FALSE / E_NOTIMPL for some of these
        // parameters even though it implements ICodecAPI — we still want to
        // build an encoder in that case, just with the encoder's defaults.
        let mut report = EncoderRateControlReport {
            cbr_set: false,
            bitrate_set: false,
            gop_set: false,
            applied_gop_frames: None,
            notes: String::new(),
        };

        unsafe {
            match icodecapi_set_value_u32(
                &encoder,
                &CODECAPI_AVENC_COMMON_RATE_CONTROL_MODE,
                RATE_CONTROL_CBR,
            ) {
                Ok(()) => report.cbr_set = true,
                Err(e) => report.notes.push_str(&format!("rate-mode: {e}; ")),
            }
            match icodecapi_set_value_u32(
                &encoder,
                &CODECAPI_AVENC_COMMON_MEAN_BIT_RATE,
                bitrate_kbps.saturating_mul(1000),
            ) {
                Ok(()) => report.bitrate_set = true,
                Err(e) => report.notes.push_str(&format!("mean-bitrate: {e}; ")),
            }
            if let Some(gop_secs) = keyframe_interval_secs {
                // GOP size in frames = fps * gop_seconds. Guard against
                // zero-denominator-fps configs by clamping to at least 1.
                let fps = if fps_den == 0 {
                    fps_num
                } else {
                    fps_num / fps_den.max(1)
                };
                let gop_frames = fps.saturating_mul(gop_secs).max(1);
                match icodecapi_set_value_u32(
                    &encoder,
                    &CODECAPI_AVENC_MPV_GOP_SIZE,
                    gop_frames,
                ) {
                    Ok(()) => {
                        report.gop_set = true;
                        report.applied_gop_frames = Some(gop_frames);
                    }
                    Err(e) => report.notes.push_str(&format!("gop-size: {e}; ")),
                }
            }
        }

        unsafe {
            encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            encoder.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        Ok((encoder, device_manager, encoder_name, report))
    }

    /// Outcome of the ICodecAPI calls in `create_h264_encoder_hw_async`. The
    /// worker logs this alongside the encoder friendly name so a "bitrate
    /// looks like 2× the configured value" or "GOP isn't where I asked" is
    /// directly visible in the diag log without rerunning. Field semantics:
    ///
    /// - `cbr_set` / `bitrate_set` / `gop_set`: did the matching SetValue
    ///   call succeed. If `cbr_set && bitrate_set`, the encoder is honoring
    ///   the configured kbps; if not, the encoder is in its default mode
    ///   (typically quality-VBR) and may overshoot.
    /// - `applied_gop_frames`: the frame count we sent the encoder. Useful
    ///   for spotting a mismatch with `effective_fps` at save time.
    /// - `notes`: concatenated failure messages (HRESULT etc.) for any setter
    ///   that didn't succeed. Empty when everything worked.
    #[derive(Debug, Clone)]
    pub struct EncoderRateControlReport {
        pub cbr_set: bool,
        pub bitrate_set: bool,
        pub gop_set: bool,
        pub applied_gop_frames: Option<u32>,
        pub notes: String,
    }

    /// Resolve the user's encoder preference to a concrete IMFTransform plus
    /// its friendly name (used for the worker init diag entry — which encoder
    /// did we actually get?). Auto / Software use the existing helpers; the
    /// vendor variants pick by friendly-name substring with Auto fallback.
    fn pick_encoder(pref: crate::replay::EncoderPreference) -> Result<(IMFTransform, String)> {
        use crate::replay::EncoderPreference as P;
        match pref {
            P::Auto => find_hardware_encoder().or_else(|_| find_software_encoder()),
            P::Software => find_software_encoder(),
            P::Nvenc => find_hw_encoder_by_substring(&["nvidia", "nvenc"])
                .or_else(|_| find_hardware_encoder()),
            P::Amf => {
                find_hw_encoder_by_substring(&["amd", "amf"]).or_else(|_| find_hardware_encoder())
            }
            P::Qsv => find_hw_encoder_by_substring(&["intel", "qsv", "quick sync"])
                .or_else(|_| find_hardware_encoder()),
        }
    }

    /// Enumerate HW H.264 encoders and activate the first one whose friendly
    /// name contains any of the provided substrings (case-insensitive).
    fn find_hw_encoder_by_substring(needles: &[&str]) -> Result<(IMFTransform, String)> {
        use windows::Win32::System::Com::CoTaskMemFree;

        let output_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut pactivates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                None,
                Some(&output_info as *const _),
                &mut pactivates,
                &mut count,
            )?;
        }
        if count == 0 || pactivates.is_null() {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_NOTIMPL,
                "no HW H.264 encoder enumerated",
            ));
        }

        let mut chosen: Option<(IMFTransform, String)> = None;
        unsafe {
            for i in 0..count as isize {
                let entry = &*pactivates.offset(i);
                let Some(act) = entry else { continue };
                let name = activate_friendly_name(act);
                if name.is_empty() {
                    continue;
                }
                let lower = name.to_lowercase();
                if needles.iter().any(|n| lower.contains(n)) {
                    if let Ok(t) = act.ActivateObject::<IMFTransform>() {
                        chosen = Some((t, name));
                        break;
                    }
                }
            }
            CoTaskMemFree(Some(pactivates as *const _));
        }

        chosen.ok_or_else(|| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_NOTIMPL,
                "no HW encoder matched preference",
            )
        })
    }

    /// Submit one D3D11 NV12 texture as encoder input. Must only be called
    /// after `METransformNeedInput` has fired for the input stream.
    pub fn submit_nv12_texture(
        encoder: &IMFTransform,
        nv12: &ID3D11Texture2D,
        pts: i64,
        duration: i64,
    ) -> Result<()> {
        let buffer: IMFMediaBuffer =
            unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, BOOL(0))? };
        let sample: IMFSample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            encoder.ProcessInput(0, &sample, 0)?;
        }
        Ok(())
    }

    /// Pull one encoded packet on a `METransformHaveOutput` event.
    /// Returns None if the encoder unexpectedly had no sample to give.
    pub fn process_output_async(encoder: &IMFTransform) -> Result<Option<(Vec<u8>, i64, bool)>> {
        use std::mem::ManuallyDrop;
        use windows::Win32::Media::MediaFoundation::{
            MFT_OUTPUT_STREAM_INFO, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
        };

        let stream_info: MFT_OUTPUT_STREAM_INFO = unsafe { encoder.GetOutputStreamInfo(0)? };
        let provides = (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

        let pre_sample: Option<IMFSample> = if provides {
            None
        } else {
            let buf: IMFMediaBuffer = unsafe { MFCreateMemoryBuffer(stream_info.cbSize.max(1))? };
            let s: IMFSample = unsafe { MFCreateSample()? };
            unsafe { s.AddBuffer(&buf)? };
            Some(s)
        };

        let mut output_data = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(pre_sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;

        unsafe { encoder.ProcessOutput(0, &mut output_data, &mut status)? };

        // MFT_OUTPUT_DATA_BUFFER uses ManuallyDrop on its IUnknown-typed
        // fields because the Microsoft API contract hands ownership of the
        // sample + events to the caller and windows-rs can't safely auto-
        // drop them. We have to take ownership ourselves so their COM
        // Release runs at end of scope. Without this, every encoded packet
        // leaks its IMFSample, which holds the encoder-allocated output
        // buffer (~115 KB at 1440x848@60 30Mbps = ~6.9 MB/s of RSS growth;
        // measured 2026-05-12).
        let sample_owned: Option<IMFSample> =
            unsafe { ManuallyDrop::take(&mut output_data[0].pSample) };
        let _events_owned: Option<windows::Win32::Media::MediaFoundation::IMFCollection> =
            unsafe { ManuallyDrop::take(&mut output_data[0].pEvents) };

        if let Some(sample) = sample_owned.as_ref() {
            let pts = unsafe { sample.GetSampleTime()? };
            let is_keyframe = unsafe {
                sample
                    .GetUINT32(
                        &windows::Win32::Media::MediaFoundation::MFSampleExtension_CleanPoint,
                    )
                    .unwrap_or(0)
                    != 0
            };
            let buffer: IMFMediaBuffer = unsafe { sample.ConvertToContiguousBuffer()? };
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            unsafe {
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                buffer.Unlock()?;
                Ok(Some((data, pts, is_keyframe)))
            }
        } else {
            Ok(None)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pack_size_matches_media_foundation_layout() {
            // MF_MT_FRAME_SIZE: width in high u32, height in low u32.
            let packed = pack_size(1920, 1080);
            assert_eq!(packed >> 32, 1920);
            assert_eq!(packed & 0xFFFF_FFFF, 1080);
        }

        #[test]
        fn pack_size_round_trips_through_high_low_u32() {
            for (w, h) in [(640, 480), (1280, 720), (2560, 1440), (3840, 2160)] {
                let p = pack_size(w, h);
                assert_eq!(p >> 32, w as u64);
                assert_eq!(p & 0xFFFF_FFFF, h as u64);
            }
        }

        #[test]
        fn pack_size_handles_dim_zero_edge() {
            // Macroblock alignment guarantees ≥ 16 in practice, but the
            // helper itself should be branchless and total. Zero in either
            // axis must still pack cleanly.
            assert_eq!(pack_size(0, 0), 0);
            assert_eq!(pack_size(1, 0) >> 32, 1);
            assert_eq!(pack_size(1, 0) & 0xFFFF_FFFF, 0);
        }

        #[test]
        fn pack_ratio_matches_media_foundation_layout() {
            // MF_MT_FRAME_RATE: numerator in high u32, denominator in low u32.
            let packed = pack_ratio(60_000, 1001); // 59.94 fps
            assert_eq!(packed >> 32, 60_000);
            assert_eq!(packed & 0xFFFF_FFFF, 1001);
        }

        #[test]
        fn pack_ratio_common_framerates() {
            for (num, den) in [(30, 1), (60, 1), (120, 1), (144, 1), (240, 1)] {
                let p = pack_ratio(num, den);
                assert_eq!(p >> 32, num as u64);
                assert_eq!(p & 0xFFFF_FFFF, den as u64);
            }
        }
    }
}
