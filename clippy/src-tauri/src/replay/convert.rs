//! GPU→CPU texture readback and software BGRA→NV12 color conversion.
//!
//! Phase 1 only — proves the encode pipeline end-to-end. Phase 2+ replaces
//! the CPU readback + software conversion with a D3D11 video processor that
//! keeps everything on the GPU.

#[cfg(windows)]
pub mod windows_impl {
    use windows::{
        core::Result,
        Win32::Graphics::{
            Direct3D11::{
                ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC,
                D3D11_USAGE_STAGING,
            },
            Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
        },
    };

    /// Copy a GPU BGRA texture to a CPU byte buffer.
    /// Creates a staging texture, copies into it, then maps for CPU read.
    pub fn readback_bgra_texture(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        // In windows 0.58, CreateTexture2D outputs via pointer rather than returning.
        let mut staging: Option<ID3D11Texture2D> = None;
        unsafe {
            device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                },
                None,
                Some(&mut staging),
            )?
        };
        let staging = staging.unwrap();

        unsafe { context.CopyResource(&staging, src) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };

        let row_pitch = mapped.RowPitch as usize;
        let row_bytes = width as usize * 4;
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        let src_ptr = mapped.pData as *const u8;
        for y in 0..height as usize {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    src_ptr.add(y * row_pitch),
                    bgra.as_mut_ptr().add(y * row_bytes),
                    row_bytes,
                );
            }
        }

        unsafe { context.Unmap(&staging, 0) };
        Ok(bgra)
    }

    /// Convert BGRA to NV12 with separate source and destination dimensions.
    ///
    /// `dst_w`/`dst_h` must be 16-aligned (H.264 macroblock requirement).
    /// Source pixels are copied into the top-left; any padding area is black.
    pub fn bgra_to_nv12(
        bgra: &[u8],
        src_w: usize,
        src_h: usize,
        dst_w: usize,
        dst_h: usize,
    ) -> Vec<u8> {
        let y_size = dst_w * dst_h;
        let mut nv12 = vec![0u8; y_size + y_size / 2];

        for y in 0..src_h.min(dst_h) {
            for x in 0..src_w.min(dst_w) {
                let i = (y * src_w + x) * 4;
                let b = bgra[i] as f32;
                let g = bgra[i + 1] as f32;
                let r = bgra[i + 2] as f32;
                nv12[y * dst_w + x] = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8;
            }
        }

        let uv_base = y_size;
        for y in (0..src_h.min(dst_h)).step_by(2) {
            for x in (0..src_w.min(dst_w)).step_by(2) {
                let i = (y * src_w + x) * 4;
                let b = bgra[i] as f32;
                let g = bgra[i + 1] as f32;
                let r = bgra[i + 2] as f32;
                let uv_idx = uv_base + (y / 2) * dst_w + x;
                nv12[uv_idx] = (-0.169 * r - 0.331 * g + 0.500 * b + 128.0).clamp(0.0, 255.0) as u8;
                nv12[uv_idx + 1] =
                    (0.500 * r - 0.419 * g - 0.081 * b + 128.0).clamp(0.0, 255.0) as u8;
            }
        }

        nv12
    }

    #[cfg(test)]
    mod tests {
        use super::bgra_to_nv12;

        /// Build a `src_w * src_h * 4` BGRA buffer filled with one color.
        fn solid(b: u8, g: u8, r: u8, src_w: usize, src_h: usize) -> Vec<u8> {
            let mut v = Vec::with_capacity(src_w * src_h * 4);
            for _ in 0..(src_w * src_h) {
                v.extend_from_slice(&[b, g, r, 255]);
            }
            v
        }

        #[test]
        fn output_buffer_is_y_plus_uv_half_size() {
            let bgra = solid(0, 0, 0, 16, 16);
            let nv12 = bgra_to_nv12(&bgra, 16, 16, 16, 16);
            // NV12: full Y plane + interleaved UV at half resolution (×½ chroma).
            assert_eq!(nv12.len(), 16 * 16 + (16 * 16) / 2);
        }

        #[test]
        fn black_input_produces_zero_y_and_neutral_uv() {
            let bgra = solid(0, 0, 0, 16, 16);
            let nv12 = bgra_to_nv12(&bgra, 16, 16, 16, 16);
            let y_size = 16 * 16;
            // Y plane: all zero.
            assert!(nv12[..y_size].iter().all(|&y| y == 0));
            // UV plane: all 128 (chroma midpoint for grayscale).
            assert!(nv12[y_size..].iter().all(|&c| c == 128));
        }

        #[test]
        fn white_input_produces_full_y_and_neutral_uv() {
            let bgra = solid(255, 255, 255, 16, 16);
            let nv12 = bgra_to_nv12(&bgra, 16, 16, 16, 16);
            let y_size = 16 * 16;
            // BT.601 luma for R=G=B=255 lands at 255 (exact).
            assert!(nv12[..y_size].iter().all(|&y| y == 255));
            // Pure white still sits on the chroma neutral axis.
            assert!(nv12[y_size..].iter().all(|&c| c == 128));
        }

        #[test]
        fn pure_red_chroma_lands_above_neutral_v_below_neutral_u() {
            // BT.601 for pure red: Y≈76, U≈85 (V-), V≈255 (clamped from 0.5*255 + 128 = 255.5).
            let bgra = solid(0, 0, 255, 16, 16);
            let nv12 = bgra_to_nv12(&bgra, 16, 16, 16, 16);
            let y_size = 16 * 16;
            // Y for pure red ≈ 76 (allow ±1 for f32 rounding).
            let y = nv12[0];
            assert!((75..=77).contains(&y), "expected Y ≈ 76, got {y}");
            // First chroma pair (U, V) at uv_base = y_size:
            let u = nv12[y_size];
            let v = nv12[y_size + 1];
            assert!(u < 128, "U for red should be < 128, got {u}");
            assert!(v > 128, "V for red should be > 128, got {v}");
        }

        #[test]
        fn padding_area_outside_src_stays_zero() {
            // 8×8 source painted into a 16×16 dst — the bottom-right padding
            // should remain whatever the buffer was initialized to (zero).
            let bgra = solid(255, 255, 255, 8, 8);
            let nv12 = bgra_to_nv12(&bgra, 8, 8, 16, 16);
            // Row 0, col 12 (outside src_w=8): Y must be zero (not touched).
            assert_eq!(nv12[12], 0);
            // Row 12, col 4 (outside src_h=8): Y must be zero.
            assert_eq!(nv12[12 * 16 + 4], 0);
            // Inside source (row 4, col 4): white → Y=255.
            assert_eq!(nv12[4 * 16 + 4], 255);
        }
    }
}
