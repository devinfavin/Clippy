//! D3D11 video processor — GPU BGRA→NV12 color conversion with optional resize.
//!
//! Replaces the CPU readback + software color math used in the Phase 1 PoC.
//! Input: BGRA D3D11 texture from WGC. Output: NV12 D3D11 texture suitable
//! for either readback (Stage A validation) or direct submission to a
//! hardware Media Foundation encoder (Stage B production path).

#[cfg(windows)]
pub mod windows_impl {
    use std::mem::ManuallyDrop;
    use windows::{
        core::{Interface, Result},
        Win32::{
            Foundation::BOOL,
            Graphics::{
                Direct3D11::{
                    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext,
                    ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
                    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET,
                    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
                    D3D11_MAP_READ, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC,
                    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
                    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
                    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
                    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
                    D3D11_VPOV_DIMENSION_TEXTURE2D,
                },
                Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC},
            },
        },
    };

    /// GPU video processor that converts BGRA input textures to NV12 in-place.
    ///
    /// Owns the destination NV12 texture and reuses it across calls — the
    /// conversion writes into the same texture on every `convert()`. Caller
    /// must consume the result before calling `convert()` again.
    pub struct VideoProcessor {
        device: ID3D11Device,
        video_device: ID3D11VideoDevice,
        video_context: ID3D11VideoContext,
        enumerator: ID3D11VideoProcessorEnumerator,
        processor: ID3D11VideoProcessor,
        nv12: ID3D11Texture2D,
        output_view: ID3D11VideoProcessorOutputView,
        dst_w: u32,
        dst_h: u32,
    }

    impl VideoProcessor {
        pub fn new(
            device: &ID3D11Device,
            context: &ID3D11DeviceContext,
            src_w: u32,
            src_h: u32,
            dst_w: u32,
            dst_h: u32,
            fps: u32,
        ) -> Result<Self> {
            let video_device: ID3D11VideoDevice = device.cast()?;
            let video_context: ID3D11VideoContext = context.cast()?;

            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: fps,
                    Denominator: 1,
                },
                InputWidth: src_w,
                InputHeight: src_h,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: fps,
                    Denominator: 1,
                },
                OutputWidth: dst_w,
                OutputHeight: dst_h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };

            let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content_desc)? };
            let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0)? };

            // NV12 destination texture. Bind flags allow video processor write
            // (RENDER_TARGET) and later use as encoder input (SHADER_RESOURCE).
            let nv12_desc = D3D11_TEXTURE2D_DESC {
                Width: dst_w,
                Height: dst_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut nv12: Option<ID3D11Texture2D> = None;
            unsafe { device.CreateTexture2D(&nv12_desc, None, Some(&mut nv12))? };
            let nv12 = nv12.unwrap();

            let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
            unsafe {
                video_device.CreateVideoProcessorOutputView(
                    &nv12,
                    &enumerator,
                    &output_view_desc,
                    Some(&mut output_view),
                )?
            };
            let output_view = output_view.unwrap();

            Ok(Self {
                device: device.clone(),
                video_device,
                video_context,
                enumerator,
                processor,
                nv12,
                output_view,
                dst_w,
                dst_h,
            })
        }

        /// Convert one BGRA frame to NV12. Writes into the owned NV12 texture.
        pub fn convert(&self, bgra: &ID3D11Texture2D) -> Result<&ID3D11Texture2D> {
            let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut input_view = None;
            unsafe {
                self.video_device.CreateVideoProcessorInputView(
                    bgra,
                    &self.enumerator,
                    &input_view_desc,
                    Some(&mut input_view),
                )?
            };

            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(input_view),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };

            unsafe {
                self.video_context.VideoProcessorBlt(
                    &self.processor,
                    &self.output_view,
                    0,
                    std::slice::from_ref(&stream),
                )?;
                // Reclaim the COM references that we transferred into the
                // ManuallyDrop fields. Without this, dropping `stream` here
                // wouldn't release the IUnknown refs inside ManuallyDrop, so
                // every convert() call would leak one ID3D11VideoProcessorInputView.
                // Same bug class fixed in encoder.rs::process_output_async
                // (MFT_OUTPUT_DATA_BUFFER::pSample). pInputSurfaceRight holds
                // None today but we take from it too so the pattern matches
                // and a future stereo path won't reintroduce the leak.
                let _ = ManuallyDrop::take(&mut stream.pInputSurface);
                let _ = ManuallyDrop::take(&mut stream.pInputSurfaceRight);
            }

            Ok(&self.nv12)
        }

        /// Read the current NV12 texture contents into a contiguous CPU buffer
        /// in encoder-ready layout: Y plane then UV plane, width-byte rows.
        pub fn readback_nv12(&self, context: &ID3D11DeviceContext) -> Result<Vec<u8>> {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: self.dst_w,
                Height: self.dst_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            unsafe {
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))?
            };
            let staging = staging.unwrap();

            unsafe { context.CopyResource(&staging, &self.nv12) };

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };

            let row_pitch = mapped.RowPitch as usize;
            let w = self.dst_w as usize;
            let h = self.dst_h as usize;
            let y_size = w * h;
            let uv_size = w * h / 2;
            let mut nv12 = vec![0u8; y_size + uv_size];

            let src = mapped.pData as *const u8;
            // Y plane: h rows of `w` valid bytes each, src stride = row_pitch
            for y in 0..h {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        src.add(y * row_pitch),
                        nv12.as_mut_ptr().add(y * w),
                        w,
                    );
                }
            }
            // UV plane: starts at row_pitch * h in the mapped buffer; h/2 rows
            let uv_src_base = row_pitch * h;
            for y in 0..h / 2 {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        src.add(uv_src_base + y * row_pitch),
                        nv12.as_mut_ptr().add(y_size + y * w),
                        w,
                    );
                }
            }

            unsafe { context.Unmap(&staging, 0) };
            Ok(nv12)
        }

        /// Direct access to the NV12 GPU texture (for encoder input,
        /// including duplicate-frame submissions during pacing).
        pub fn nv12_texture(&self) -> &ID3D11Texture2D {
            &self.nv12
        }
    }
}
